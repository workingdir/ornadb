//! Format-3 native Git graph validation.
//!
//! This module is deliberately a seam for the repository context owner.  The
//! public values describe native objects and decoded node data, but admission
//! is `pub(crate)` and requires an owner-issued [`ValidatedRepositoryContext`].
//! In particular, a raw OID or a caller-provided resolver is not a repository
//! authority.

use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt,
    fs::{self, File},
    io::{Read, Write},
    ops::Range,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering},
    },
};

use fs2::FileExt;
use orna_value_v1::{
    AvailabilityState, Blob, BlobMetadata, BlobResolver, ContentIdentity as ValueContentIdentity,
    ContentReference, ContextValue, Error as ValueError, Format3Context, Format3Quota, GitHash,
    MediaAnnotation, MimeRegistry, NativeOid as ValueNativeOid, OwnerLifetime, Raw,
    SchemaDescriptor, Snapshot as ValueSnapshot, ValueFormat,
};
use sha2::{Digest, Sha256};


pub const REPOSITORY_FORMAT: u8 = 3;
pub const NODE_DATA_LIMIT: usize = 65_536;
pub const MAX_REFS: usize = 256;
pub const MAX_GRAPH_HEIGHT: u8 = 64;
pub const MAX_SIGNED_LENGTH: u64 = i64::MAX as u64;
pub const MAX_RANGE_READ_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_FULL_VERIFY_OBJECTS: u64 = 1_000_000;
pub const MAX_RANGE_GRAPH_OBJECTS: u64 = 2_048;
pub const MAX_RANGE_METADATA_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_FULL_METADATA_BYTES: u64 = 1024 * 1024 * 1024;
pub const MAX_PACK_INDEX_FILES: usize = 2_048;
pub const MAX_INDEXED_TREE_BYTES: u64 = NODE_DATA_LIMIT as u64;
const MAX_PACK_INDEX_SCAN_BYTES: u64 = 1 << 40;
const MAX_PACK_SET_SYNC_BYTES: u64 = 64 * 1024 * 1024 * 1024;

/// The two native Git object hash modes admitted by OGS-1.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum GitHashAlgorithm {
    Sha1,
    Sha256,
}

impl GitHashAlgorithm {
    pub const fn width(self) -> usize {
        match self {
            Self::Sha1 => 20,
            Self::Sha256 => 32,
        }
    }
}

/// A complete native Git object ID.  The algorithm is retained with the
/// bytes so a SHA-1 OID can never be accepted in a SHA-256 repository (or vice
/// versa) merely because its textual spelling looks valid.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NativeOid {
    algorithm: GitHashAlgorithm,
    bytes: Vec<u8>,
}

/// Compatibility name for the native Git object identifier used in graph
/// envelopes.  This is distinct from the repository crate's textual ID type.
pub type NativeObjectId = NativeOid;

impl NativeOid {
    pub fn new(algorithm: GitHashAlgorithm, bytes: impl AsRef<[u8]>) -> Result<Self, GraphError> {
        let bytes = bytes.as_ref();
        if bytes.len() != algorithm.width() {
            return Err(GraphError::InvalidOidWidth {
                expected: algorithm.width(),
                actual: bytes.len(),
            });
        }
        Ok(Self {
            algorithm,
            bytes: bytes.to_vec(),
        })
    }

    pub fn from_hex(algorithm: GitHashAlgorithm, value: &str) -> Result<Self, GraphError> {
        if value.len() != algorithm.width() * 2 {
            return Err(GraphError::InvalidOidWidth {
                expected: algorithm.width(),
                actual: value.len() / 2,
            });
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(GraphError::MalformedOid);
        }
        let mut bytes = Vec::with_capacity(algorithm.width());
        for pair in value.as_bytes().chunks_exact(2) {
            let high = (pair[0] as char)
                .to_digit(16)
                .ok_or(GraphError::MalformedOid)?;
            let low = (pair[1] as char)
                .to_digit(16)
                .ok_or(GraphError::MalformedOid)?;
            bytes.push(((high << 4) | low) as u8);
        }
        Self::new(algorithm, bytes)
    }

    /// Converts the repository's validated textual Git ID only after checking
    /// that its spelling and width agree with the repository hash mode.
    pub fn from_repository_id(
        algorithm: GitHashAlgorithm,
        value: &crate::NativeObjectId,
    ) -> Result<Self, GraphError> {
        Self::from_hex(algorithm, value.as_str())
    }

    pub const fn algorithm(&self) -> GitHashAlgorithm {
        self.algorithm
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn to_hex(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut output = String::with_capacity(self.bytes.len() * 2);
        for byte in &self.bytes {
            output.push(HEX[(byte >> 4) as usize] as char);
            output.push(HEX[(byte & 0x0f) as usize] as char);
        }
        output
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct ContextIdentity([u8; 32]);

/// Owner-issued access to one validated format-3 repository snapshot.
///
/// Construction is crate-private and reserved for repository-context
/// integration.  The fields bind the local owner, database, immutable store
/// root and schema generation; callers cannot create a context from raw OIDs.
#[derive(Debug)]
struct PrivateRefCleanupOwner {
    repository: crate::Repository,
}

impl PrivateRefCleanupOwner {
    fn git_command(&self) -> Command {
        let mut command = Command::new("git");
        command
            .current_dir(self.repository.worktree())
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES");
        crate::bind_git_discovery_to_worktree(&mut command, self.repository.worktree());
        command
    }

    fn git_output(&self, args: &[&str]) -> Result<Vec<u8>, GraphError> {
        let output = self
            .git_command()
            .args(args)
            .output()
            .map_err(|_| GraphError::GitCommandFailed)?;
        if !output.status.success() {
            // A `cat-file` invocation that names one object reports an object
            // that is absent locally. Name it, and say whether a partial clone
            // defers it, so a promised object is never an anonymous
            // availability failure (ORNA-GIT-002, ORNA-ROW-007).
            return Err(match cat_file_object_id(args) {
                Some(object_id) => absent_object_error(&self.repository, object_id),
                None => GraphError::GitCommandFailed,
            });
        }
        Ok(output.stdout)
    }

    fn create_protected_ref(&self, reference: &str, oid: &NativeOid) -> Result<(), GraphError> {
        let zero = "0".repeat(oid.algorithm().width() * 2);
        self.git_output(&["update-ref", reference, &oid.to_hex(), &zero])?;
        self.sync_ref_file(reference)
    }

    fn sync_ref_file(&self, reference: &str) -> Result<(), GraphError> {
        let output = self.git_output(&["rev-parse", "--git-path", reference])?;
        let path = resolve_git_path(self.repository.worktree(), &output)?;
        let metadata = fs::symlink_metadata(&path).map_err(|_| GraphError::DurabilityFailed)?;
        if !metadata.file_type().is_file() {
            return Err(GraphError::DurabilityFailed);
        }
        File::open(&path)
            .and_then(|file| file.sync_all())
            .map_err(|_| GraphError::DurabilityFailed)?;
        self.sync_ref_directories(reference)
    }
    fn delete_protected_ref(&self, reference: &str, oid: &NativeOid) -> Result<(), GraphError> {
        self.git_output(&["update-ref", "-d", reference, &oid.to_hex()])?;
        self.sync_ref_directories(reference)
    }

    fn sync_ref_directories(&self, reference: &str) -> Result<(), GraphError> {
        let common_dir_output = self.git_output(&["rev-parse", "--git-common-dir"])?;
        let common_dir = resolve_git_path(self.repository.worktree(), &common_dir_output)?;
        let common_dir = fs::canonicalize(common_dir).map_err(|_| GraphError::DurabilityFailed)?;
        let ref_path_output = self.git_output(&["rev-parse", "--git-path", reference])?;
        let ref_path = resolve_git_path(self.repository.worktree(), &ref_path_output)?;
        let mut directory = ref_path
            .parent()
            .ok_or(GraphError::DurabilityFailed)?
            .to_path_buf();
        let directory = loop {
            match fs::canonicalize(&directory) {
                Ok(directory) => break directory,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    if !directory.pop() {
                        return Err(GraphError::DurabilityFailed);
                    }
                }
                Err(_) => return Err(GraphError::DurabilityFailed),
            }
        };
        if !directory.starts_with(&common_dir) {
            return Err(GraphError::DurabilityFailed);
        }
        let mut current = directory;
        loop {
            sync_directory(&current)?;
            if current == common_dir || !current.pop() {
                break;
            }
        }
        let packed_refs = common_dir.join("packed-refs");
        match fs::symlink_metadata(&packed_refs) {
            Ok(metadata) => {
                if !metadata.file_type().is_file() {
                    return Err(GraphError::DurabilityFailed);
                }
                File::open(&packed_refs)
                    .and_then(|file| file.sync_all())
                    .map_err(|_| GraphError::DurabilityFailed)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(GraphError::DurabilityFailed),
        }
        Ok(())
    }
}

#[derive(Debug)]
struct PendingPrivateRefCleanup {
    owner: Arc<PrivateRefCleanupOwner>,
    reference: String,
    oid: NativeOid,
    armed: bool,
}

impl PendingPrivateRefCleanup {
    fn new(owner: Arc<PrivateRefCleanupOwner>, reference: String, oid: NativeOid) -> Self {
        Self {
            owner,
            reference,
            oid,
            armed: true,
        }
    }

    fn cleanup(&mut self) -> Result<(), GraphError> {
        self.cleanup_with_identity().map_err(|(_, error)| error)
    }

    fn cleanup_with_identity(&mut self) -> Result<(), (String, GraphError)> {
        if self.armed {
            self.owner
                .delete_protected_ref(&self.reference, &self.oid)
                .map_err(|error| (self.reference.clone(), error))?;
            self.armed = false;
        }
        Ok(())
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PendingPrivateRefCleanup {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

#[derive(Debug)]
pub struct NativeGraphContext {
    repository: crate::Repository,
    private_ref_owner: Arc<PrivateRefCleanupOwner>,
    repository_id: [u8; 32],
    identity: ContextIdentity,
    database_id: [u8; 16],
    owner_id: [u8; 16],
    accepted_pin_fallback_owner_id: Option<[u8; 16]>,
    snapshot_id: [u8; 32],
    algorithm: GitHashAlgorithm,
    store_root: NativeOid,
    schema_digest: [u8; 32],
    row_snapshot: crate::row_store::RowMapSnapshot,
    cancelled: Arc<AtomicBool>,
    node_cache: std::sync::Mutex<NodeCache>,
}

/// One schema-ordered field identity and its payload-free Blob projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmittedRowField {
    field_id: [u8; 16],
    blob_metadata: Option<BlobMetadata>,
}

impl AdmittedRowField {
    pub const fn field_id(&self) -> &[u8; 16] {
        &self.field_id
    }

    pub fn blob_metadata(&self) -> Option<&BlobMetadata> {
        self.blob_metadata.as_ref()
    }
}

/// Canonical ROV3 field tuple bound to one admitted row and schema generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmittedRowFields {
    version: crate::row_store::RowMapVersion,
    key: crate::row_store::TypedKey,
    canonical_fields: Vec<u8>,
    fields: Vec<AdmittedRowField>,
}

impl AdmittedRowFields {
    pub fn version(&self) -> &crate::row_store::RowMapVersion {
        &self.version
    }

    pub fn schema_generation(&self) -> &crate::row_store::SchemaGeneration {
        self.version.schema()
    }

    pub fn key(&self) -> &crate::row_store::TypedKey {
        &self.key
    }

    /// Complete canonical ROV3 field tuple in descriptor order.
    pub fn canonical_fields(&self) -> &[u8] {
        &self.canonical_fields
    }

    /// Field identities and Blob metadata in the same order as the tuple.
    pub fn fields(&self) -> &[AdmittedRowField] {
        &self.fields
    }

    /// The row's format-3 identity (store-format-3 section 5): `orna.row.v3`
    /// over the SOV-3 encoding of `[database_uuid, table_uuid, typed_key,
    /// stored_fields]`. Blobs enter by length, SHA-256 and annotations and
    /// overflow values as the values they hold, so rows with the same
    /// logical content have the same identity however they are stored, and
    /// computing it reads nothing beyond the fields already decoded.
    pub fn row_identity(&self) -> Result<[u8; 32], GraphError> {
        row_identity_v3(
            self.version.database_id(),
            self.version.relation_id(),
            &self.key,
            &self.canonical_fields,
            self.version.store_root().algorithm().width(),
        )
    }
}

/// Shared owner-issued graph and read-scope authority for one Blob capture
/// pipeline. Clones share the graph owner and the scope's cumulative quotas.
#[derive(Clone, Debug)]
pub struct RepositoryCaptureCapability {
    graph: Arc<NativeGraphContext>,
    scope: RepositoryReadScope,
    /// The snapshot-pinned value context that captured candidates are bound
    /// to, present when a format context issued this capability.
    capture_context: Option<Format3Context>,
}

impl PartialEq for RepositoryCaptureCapability {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.graph, &other.graph)
            && Arc::ptr_eq(&self.scope.bytes_used, &other.scope.bytes_used)
    }
}

impl Eq for RepositoryCaptureCapability {}

impl RepositoryCaptureCapability {
    /// Joins a graph with the read scope issued by that exact graph owner.
    pub fn new(
        graph: Arc<NativeGraphContext>,
        scope: RepositoryReadScope,
    ) -> Result<Self, GraphError> {
        scope.authorize(&graph)?;
        Ok(Self {
            graph,
            scope,
            capture_context: None,
        })
    }

    /// Binds the values this capability issues for captured candidates to the
    /// exact snapshot pin of the format context that issued it.
    pub(crate) fn with_capture_pin(mut self, pin: ValueSnapshot) -> Result<Self, ValueError> {
        self.capture_context = Some(self.value_context(pin)?);
        Ok(self)
    }

    /// Cumulative media payload bytes this capability has read from the graph.
    ///
    /// This is the accounting a metadata-only query is measured against: it
    /// stays at zero while the same capability still walks row pages, byte
    /// index nodes and descriptors, which are charged as objects instead.
    pub fn payload_bytes_read(&self) -> u64 {
        self.scope.payload_bytes_read()
    }

    /// Cumulative media payload bytes this capability has written to the graph.
    pub fn payload_bytes_written(&self) -> u64 {
        self.scope.payload_bytes_written()
    }

    /// Cumulative native Git objects this capability has charged its owner.
    pub fn objects_read(&self) -> u64 {
        self.scope.objects_read()
    }

    /// Streams a source into the graph's existing OGB-2 writer. The resulting
    /// provisional candidate is private until a repository row owner accepts
    /// it; dropping it releases its pending private ref.
    pub fn capture_blob_candidate<R: Read>(
        &self,
        source: R,
        max_bytes: u64,
    ) -> Result<CapturedBlobCandidate, GraphError> {
        self.graph
            .capture_blob_candidate(source, max_bytes, &self.scope)
    }

    /// The lazy Blob value of a captured candidate: its content identity, the
    /// octet-stream fallback annotation and its descriptor, as a ROV-3 value
    /// in this capability's snapshot-pinned context. Reading it streams
    /// verified chunks from the graph on demand; the value never holds the
    /// payload, whatever its size.
    pub fn candidate_value(
        &self,
        candidate: &CapturedBlobCandidate,
    ) -> Result<ContextValue, GraphError> {
        let context = self
            .capture_context
            .as_ref()
            .ok_or(GraphError::ContextMismatch)?;
        if candidate.context != self.graph.identity || candidate.owner_id != self.graph.owner_id {
            return Err(GraphError::ContextMismatch);
        }
        let identity =
            ValueContentIdentity::new(candidate.identity.length(), candidate.identity.sha256())
                .map_err(|_| GraphError::InvalidLength(candidate.identity.length()))?;
        let descriptor = ValueNativeOid::from_bytes(candidate.descriptor_oid.as_bytes())
            .map_err(|_| GraphError::MalformedOid)?;
        let reference = context
            .reference(identity, descriptor)
            .map_err(|_| GraphError::MalformedOid)?;
        let blob = Blob::from_reference(reference).map_err(|_| GraphError::ContextMismatch)?;
        ContextValue::from_blob(&blob, ValueFormat::Rov3).map_err(|_| GraphError::ContextMismatch)
    }

    /// Promotes a captured source into an owner-scoped durable pending pin,
    /// then binds the MIME-1 annotation supplied by the OVB-2 value.
    pub fn accept_captured_blob(
        &self,
        candidate: CapturedBlobCandidate,
        media_type: &str,
        suffix: Option<&str>,
    ) -> Result<OrpBlobBinding, GraphError> {
        let pin = self
            .graph
            .protect_captured_blob(candidate, &self.scope)?;
        self.graph
            .accept_protected_blob_pin_with_annotation(pin, media_type, suffix)
    }
}

impl RepositoryCaptureCapability {
    /// Issues value-layer authority for this graph's exact repository snapshot.
    pub(crate) fn value_context(&self, pin: ValueSnapshot) -> Result<Format3Context, ValueError> {
        let version = self.graph.row_snapshot.version();
        let schema_root = ValueNativeOid::from_bytes(version.schema().schema_oid().as_bytes())?;
        let store_root = ValueNativeOid::from_bytes(self.graph.store_root.as_bytes())?;
        let git_hash = match self.graph.algorithm {
            GitHashAlgorithm::Sha1 => GitHash::Sha1,
            GitHashAlgorithm::Sha256 => GitHash::Sha256,
        };
        let resolver: Arc<dyn BlobResolver> = Arc::new(RepositoryValueResolver {
            graph: Arc::clone(&self.graph),
            scope: self.scope.clone(),
            pin: pin.clone(),
        });
        let quota = Format3Quota::new(MAX_RANGE_READ_BYTES, 0)?;
        Format3Context::from_persisted(
            self.graph.database_id,
            pin,
            git_hash,
            schema_root,
            store_root,
            AvailabilityState::Available,
            OwnerLifetime::new(self.graph.owner_id),
            quota,
            resolver,
        )
    }
}

struct RepositoryValueResolver {
    graph: Arc<NativeGraphContext>,
    scope: RepositoryReadScope,
    pin: ValueSnapshot,
}

impl BlobResolver for RepositoryValueResolver {
    fn read_range(
        &self,
        reference: &ContentReference,
        offset: u64,
        length: u64,
    ) -> Result<Vec<u8>, ValueError> {
        let context = reference.context();
        let schema_oid = self.graph.row_snapshot.version().schema().schema_oid();
        if context.pin() != &self.pin
            || context.database_uuid() != self.graph.database_id
            || context.owner().id() != self.graph.owner_id
            || context.schema_root().as_bytes() != schema_oid.as_bytes()
            || context.store_root().as_bytes() != self.graph.store_root.as_bytes()
        {
            return Err(ValueError::InvalidContext);
        }
        let descriptor_oid = NativeOid::new(
            self.graph.algorithm,
            reference.descriptor_oid().as_bytes(),
        )
        .map_err(graph_error_to_value_error)?;
        let identity = crate::blob_store::ContentIdentity::new(
            reference.identity().length(),
            reference.identity().sha256(),
        )
        .map_err(|_| ValueError::InvalidValue)?;
        let admitted = AdmittedBlobReference {
            context: self.graph.identity,
            database_id: self.graph.database_id,
            owner_id: self.graph.owner_id,
            snapshot_id: self.graph.snapshot_id,
            descriptor_oid,
            identity,
        };
        let end = offset.checked_add(length).ok_or(ValueError::InvalidRange)?;
        let range = self
            .graph
            .read_blob_range(&admitted, offset..end, &self.scope)
            .map_err(graph_error_to_value_error)?;
        Ok(range.bytes)
    }
}

fn graph_error_to_value_error(error: GraphError) -> ValueError {
    match error {
        GraphError::InvalidRange => ValueError::InvalidRange,
        GraphError::ReadQuotaExceeded
        | GraphError::MetadataQuotaExceeded
        | GraphError::InventoryQuotaExceeded => ValueError::QuotaExceeded,
        GraphError::ReadCancelled => ValueError::Cancelled,
        GraphError::ContentIdentityMismatch
        | GraphError::ChunkDigestMismatch
        | GraphError::GitObjectHashMismatch => ValueError::ContentDigestMismatch,
        GraphError::InvalidOidWidth { .. } | GraphError::MalformedOid => ValueError::InvalidOid,
        GraphError::ContextMismatch => ValueError::InvalidContext,
        _ => ValueError::ContentUnavailable,
    }
}

impl NativeGraphContext {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn issue(
        repository: crate::Repository,
        repository_id: [u8; 32],
        identity: [u8; 32],
        database_id: [u8; 16],
        owner_id: [u8; 16],
        snapshot_id: [u8; 32],
        algorithm: GitHashAlgorithm,
        store_root: NativeOid,
        schema_digest: [u8; 32],
        row_snapshot: crate::row_store::RowMapSnapshot,
    ) -> Result<Self, GraphError> {
        let version = row_snapshot.version();
        let schema_oid = version.schema().schema_oid();
        if store_root.algorithm() != algorithm || schema_oid.algorithm() != algorithm {
            return Err(GraphError::InvalidOidWidth {
                expected: algorithm.width(),
                actual: if store_root.algorithm() != algorithm {
                    store_root.as_bytes().len()
                } else {
                    schema_oid.as_bytes().len()
                },
            });
        }
        if version.database_id() != &database_id
            || version.schema().database_id() != &database_id
            || version.store_root() != &store_root
            || version.schema().schema_digest() != &schema_digest
        {
            return Err(GraphError::ContextMismatch);
        }
        let context = Self {
            private_ref_owner: Arc::new(PrivateRefCleanupOwner {
                repository: repository.clone(),
            }),
            repository,
            repository_id,
            identity: ContextIdentity(identity),
            database_id,
            owner_id,
            accepted_pin_fallback_owner_id: None,
            snapshot_id,
            algorithm,
            store_root,
            schema_digest,
            row_snapshot,
            cancelled: Arc::new(AtomicBool::new(false)),
            node_cache: std::sync::Mutex::new(NodeCache::default()),
        };
        context.validate_sealed_schema_node()?;
        Ok(context)
    }

    /// Makes accepted pins held by `owner_id` admissible through this graph
    /// when this graph's own owner holds no ref for them.
    pub(crate) fn with_accepted_pin_fallback_owner(mut self, owner_id: [u8; 16]) -> Self {
        if owner_id != self.owner_id {
            self.accepted_pin_fallback_owner_id = Some(owner_id);
        }
        self
    }

    fn validate_sealed_schema_node(&self) -> Result<(), GraphError> {
        let scope = self.open_read_scope()?;
        let mut objects = ObjectBudget::new(
            scope.max_objects,
            scope.max_objects,
            Arc::clone(&scope.objects_used),
            scope.metadata_quota,
            Arc::clone(&scope.metadata_used),
        );
        let schema_oid = self.row_snapshot.version().schema().schema_oid();
        let node = self.read_native_node(schema_oid, &scope, &mut objects)?;
        let NodeData::Schema {
            encoded_length,
            schema_digest,
            byte_root,
        } = node
        else {
            return Err(GraphError::WrongNodeKind {
                expected: NodeKind::Schema,
                actual: node.kind(),
            });
        };
        if schema_digest != self.schema_digest {
            return Err(GraphError::ContentIdentityMismatch);
        }
        // The node records the `orna.schema.v3` identity of its descriptor
        // bytes (store-format-3 section 5), streamed here with the byte
        // index's chunk digests and boundaries.
        let mut digest =
            crate::blob_store::ContentDigest::domain_separated(SCHEMA_IDENTITY_DOMAIN);
        let mut chunk_boundaries = GearBoundaryVerifier::new();
        self.walk_full_index(
            &byte_root,
            encoded_length,
            None,
            &scope,
            &mut objects,
            &mut digest,
            &mut chunk_boundaries,
        )?;
        let actual = digest.finish();
        if actual.length() != encoded_length || actual.sha256() != schema_digest {
            return Err(GraphError::ContentIdentityMismatch);
        }
        Ok(())
    }
    /// Returns the immutable row-map identity this graph is pinned to.
    pub(crate) fn row_map_version(&self) -> &crate::row_store::RowMapVersion {
        self.row_snapshot.version()
    }

    /// Opens the repository owner's fixed-budget read scope. Each range is
    /// limited to 8 MiB; object count and metadata limits are fixed here, not
    /// selected by the consumer. The cumulative payload allowance is bounded
    /// by the format's signed-length limit so a verified pin can stream a
    /// complete content closure.
    pub fn open_read_scope(&self) -> Result<RepositoryReadScope, GraphError> {
        self.issue_read_scope(
            self.owner_id,
            Arc::clone(&self.cancelled),
            MAX_SIGNED_LENGTH,
            MAX_RANGE_READ_BYTES,
            MAX_FULL_VERIFY_OBJECTS,
            MAX_FULL_METADATA_BYTES,
        )
    }

    /// Decodes the schema descriptor retained by this graph's sealed row map.
    /// The caller cannot substitute an OID or bytes from another snapshot.
    pub fn read_schema_descriptor(
        &self,
        scope: &RepositoryReadScope,
    ) -> Result<SchemaDescriptor, GraphError> {
        scope.authorize(self)?;
        let version = self.row_snapshot.version();
        let schema_oid = version.schema().schema_oid();
        let mut objects = ObjectBudget::new(
            scope.max_objects,
            scope.max_objects,
            Arc::clone(&scope.objects_used),
            scope.metadata_quota,
            Arc::clone(&scope.metadata_used),
        );
        let node = self.read_native_node(schema_oid, scope, &mut objects)?;
        let NodeData::Schema {
            encoded_length,
            schema_digest,
            byte_root,
        } = node
        else {
            return Err(GraphError::WrongNodeKind {
                expected: NodeKind::Schema,
                actual: node.kind(),
            });
        };
        if schema_digest != self.schema_digest
            || schema_digest != *version.schema().schema_digest()
            || encoded_length > MAX_FULL_METADATA_BYTES
        {
            return Err(GraphError::ContentIdentityMismatch);
        }
        objects.record_metadata(encoded_length)?;
        let capacity = usize::try_from(encoded_length)
            .map_err(|_| GraphError::InvalidLength(encoded_length))?;
        let mut encoded = Vec::new();
        encoded
            .try_reserve_exact(capacity)
            .map_err(|_| GraphError::ReadQuotaExceeded)?;
        self.read_full_index_bytes(
            &byte_root,
            encoded_length,
            None,
            scope,
            &mut objects,
            &mut encoded,
        )?;
        if encoded.len() as u64 != encoded_length {
            return Err(GraphError::ContentIdentityMismatch);
        }
        SchemaDescriptor::decode(&encoded).map_err(|_| GraphError::NonCanonicalData)
    }

    /// Cancels every read scope opened from this repository graph context.
    pub fn cancel_reads(&self) {
        self.cancelled.store(true, AtomicOrdering::Release);
    }

    /// Performs one bounded logical-key lookup against this context's pinned
    /// primary ORP root. The key is a logical typed key, not a native OID.
    pub fn lookup_row(
        &self,
        key: &crate::row_store::TypedKey,
        scope: &RepositoryReadScope,
    ) -> Result<Option<crate::row_store::AdmittedRow>, GraphError> {
        scope.authorize(self)?;
        self.lookup_row_in_version(self.row_snapshot.version(), key, scope)
    }

    fn lookup_row_in_version(
        &self,
        version: &crate::row_store::RowMapVersion,
        key: &crate::row_store::TypedKey,
        scope: &RepositoryReadScope,
    ) -> Result<Option<crate::row_store::AdmittedRow>, GraphError> {
        scope.authorize(self)?;
        if version.database_id() != &self.database_id {
            return Err(GraphError::ContextMismatch);
        }
        let expected_count = version.row_count().ok_or(GraphError::ContextMismatch)?;
        let root = version.primary_root().clone();
        let domain = rows_domain_for_version(version);
        let entry = {
            let mut objects = ObjectBudget::new(
                scope.max_objects.min(MAX_RANGE_GRAPH_OBJECTS),
                scope.max_objects,
                Arc::clone(&scope.objects_used),
                scope.metadata_quota.min(MAX_RANGE_METADATA_BYTES),
                Arc::clone(&scope.metadata_used),
            );
            let mut read_node = |oid: &NativeOid| self.read_native_node(oid, scope, &mut objects);
            let root_node = read_node(&root)?;
            validate_ordered_root_node(&root_node, &domain, expected_count)?;
            find_ordered_entry_with(&mut read_node, root_node, &domain, key)?
        };
        let Some(entry) = entry else {
            return Ok(None);
        };
        let mut objects = ObjectBudget::new(
            scope.max_objects.min(MAX_RANGE_GRAPH_OBJECTS),
            scope.max_objects,
            Arc::clone(&scope.objects_used),
            scope.metadata_quota.min(MAX_RANGE_METADATA_BYTES),
            Arc::clone(&scope.metadata_used),
        );
        let key = crate::row_store::TypedKey::decode_canonical(&entry.key)
            .map_err(|_| GraphError::NonCanonicalData)?;
        let value = self.decode_persisted_row_value(entry.value, scope, &mut objects)?;
        Ok(Some(crate::row_store::AdmittedRow::issue(
            version, key, value,
        )))
    }

    /// Resolves a stored table reference through the containing row's immutable
    /// repository snapshot. The referenced relation and key are looked up under
    /// the same repository read scope, never through the current worktree state.
    pub fn resolve_table_reference(
        &self,
        containing_row: &crate::row_store::AdmittedRow,
        encoded_reference: &[u8],
        scope: &RepositoryReadScope,
    ) -> Result<Option<crate::row_store::AdmittedRow>, GraphError> {
        scope.authorize(self)?;
        let owner_version = containing_row.version();
        if owner_version.database_id() != &self.database_id
            || self.lookup_row_in_version(owner_version, containing_row.key(), scope)?
                .as_ref()
                != Some(containing_row)
        {
            return Err(GraphError::ContextMismatch);
        }
        let CborValue::Tag(60021, payload) = decode_canonical_cbor(encoded_reference)? else {
            return Err(GraphError::NonCanonicalData);
        };
        let CborValue::Array(fields) = *payload else {
            return Err(GraphError::NonCanonicalData);
        };
        let [database, relation, key] = fields.as_slice() else {
            return Err(GraphError::NonCanonicalData);
        };
        let database = stored_uuid(database)?;
        let relation = stored_uuid(relation)?;
        if database != *owner_version.database_id() {
            return Ok(None);
        }
        let key = crate::row_store::TypedKey::decode_canonical(&canonical_value_bytes(key))
            .map_err(|_| GraphError::NonCanonicalData)?;
        let target_version = if relation == *owner_version.relation_id() {
            owner_version.clone()
        } else {
            self.row_map_version_for_relation(
                owner_version.store_root(),
                *owner_version.database_id(),
                relation,
                owner_version.generation(),
                scope,
            )?
        };
        self.lookup_row_in_version(&target_version, &key, scope)
    }

    fn row_map_version_for_relation(
        &self,
        store_root: &NativeOid,
        database_id: [u8; 16],
        relation_id: [u8; 16],
        generation: u64,
        scope: &RepositoryReadScope,
    ) -> Result<crate::row_store::RowMapVersion, GraphError> {
        scope.authorize(self)?;
        let mut objects = ObjectBudget::new(
            scope.max_objects.min(MAX_RANGE_GRAPH_OBJECTS),
            scope.max_objects,
            Arc::clone(&scope.objects_used),
            scope.metadata_quota.min(MAX_RANGE_METADATA_BYTES),
            Arc::clone(&scope.metadata_used),
        );
        let store = self.read_native_node(store_root, scope, &mut objects)?;
        let NodeData::StoreRoot { relation_map } = store else {
            return Err(GraphError::WrongNodeKind {
                expected: NodeKind::StoreRoot,
                actual: store.kind(),
            });
        };
        let relations_domain = canonical_domain(vec![
            CborValue::Text("relations".to_owned()),
            CborValue::Bytes(database_id.to_vec()),
        ]);
        let relation_key = crate::row_store::TypedKey::Bytes(relation_id.to_vec());
        let relation_node = self.read_native_node(&relation_map, scope, &mut objects)?;
        let relation_entry = {
            let mut read_node = |oid: &NativeOid| self.read_native_node(oid, scope, &mut objects);
            find_ordered_entry_with(
                &mut read_node,
                relation_node,
                &relations_domain,
                &relation_key,
            )?
        }
        .ok_or(GraphError::ContextMismatch)?;
        let (schema_oid, primary_root, _secondary_root, row_count) =
            decode_relation_value(&relation_entry.value, self.algorithm)?;
        let schema_node = self.read_native_node(&schema_oid, scope, &mut objects)?;
        let NodeData::Schema {
            encoded_length,
            schema_digest,
            byte_root,
        } = schema_node
        else {
            return Err(GraphError::WrongNodeKind {
                expected: NodeKind::Schema,
                actual: schema_node.kind(),
            });
        };
        let mut schema_bytes = Vec::new();
        let capacity = usize::try_from(encoded_length).map_err(|_| GraphError::ReadQuotaExceeded)?;
        if encoded_length > scope.metadata_quota.min(MAX_RANGE_METADATA_BYTES) {
            return Err(GraphError::ReadQuotaExceeded);
        }
        schema_bytes
            .try_reserve_exact(capacity)
            .map_err(|_| GraphError::ReadQuotaExceeded)?;
        self.read_full_index_bytes(
            &byte_root,
            encoded_length,
            None,
            scope,
            &mut objects,
            &mut schema_bytes,
        )?;
        if schema_identity_v3(&schema_bytes) != schema_digest {
            return Err(GraphError::ContentIdentityMismatch);
        }
        let descriptor = SchemaDescriptor::decode(&schema_bytes)
            .map_err(|_| GraphError::NonCanonicalData)?;
        if schema_relation_id(&descriptor)? != relation_id {
            return Err(GraphError::ContextMismatch);
        }
        let schema = crate::row_store::SchemaGeneration::issue(
            database_id,
            relation_id,
            schema_oid,
            schema_digest,
            generation,
        );
        crate::row_store::RowMapVersion::issue(
            database_id,
            relation_id,
            store_root.clone(),
            schema,
            primary_root,
            generation,
            Some(row_count),
        )
        .map_err(|_| GraphError::ContextMismatch)
    }

    /// Reads a bounded logical-key interval from the pinned primary ORP root.
    /// The result cap is part of `KeyRange`; object and metadata budgets are
    /// fixed by the owner-issued read scope.
    pub fn range_rows(
        &self,
        range: &crate::row_store::KeyRange,
        scope: &RepositoryReadScope,
    ) -> Result<Vec<crate::row_store::AdmittedRow>, GraphError> {
        scope.authorize(self)?;
        let version = self.row_snapshot.version();
        let expected_count = version.row_count().ok_or(GraphError::ContextMismatch)?;
        let root = version.primary_root().clone();
        let domain = rows_domain_for_version(version);
        let entries = {
            let mut objects = ObjectBudget::new(
                scope.max_objects.min(MAX_RANGE_GRAPH_OBJECTS),
                scope.max_objects,
                Arc::clone(&scope.objects_used),
                scope.metadata_quota.min(MAX_RANGE_METADATA_BYTES),
                Arc::clone(&scope.metadata_used),
            );
            let mut read_node = |oid: &NativeOid| self.read_native_node(oid, scope, &mut objects);
            let root_node = read_node(&root)?;
            validate_ordered_root_node(&root_node, &domain, expected_count)?;
            let mut entries = Vec::with_capacity(range.limit());
            collect_ordered_range_with(
                &mut read_node,
                root_node,
                &domain,
                None,
                None,
                range.lower_inclusive(),
                range.resume_after(),
                range.upper_exclusive(),
                range.limit(),
                &mut entries,
            )?;
            entries
        };
        let mut objects = ObjectBudget::new(
            scope.max_objects.min(MAX_RANGE_GRAPH_OBJECTS),
            scope.max_objects,
            Arc::clone(&scope.objects_used),
            scope.metadata_quota.min(MAX_RANGE_METADATA_BYTES),
            Arc::clone(&scope.metadata_used),
        );
        entries
            .into_iter()
            .map(|entry| {
                let key = crate::row_store::TypedKey::decode_canonical(&entry.key)
                    .map_err(|_| GraphError::NonCanonicalData)?;
                let value = self.decode_persisted_row_value(entry.value, scope, &mut objects)?;
                Ok(crate::row_store::AdmittedRow::issue(version, key, value))
            })
            .collect()
    }

    /// Reads only the logical keys in a bounded row-map interval. Row values
    /// are not decoded, so directory iteration can continue past one corrupt
    /// value and defer its error until that row is opened.
    pub fn range_row_keys(
        &self,
        range: &crate::row_store::KeyRange,
        scope: &RepositoryReadScope,
    ) -> Result<Vec<crate::row_store::TypedKey>, GraphError> {
        scope.authorize(self)?;
        let version = self.row_snapshot.version();
        let expected_count = version.row_count().ok_or(GraphError::ContextMismatch)?;
        let root = version.primary_root().clone();
        let domain = rows_domain_for_version(version);
        let mut objects = ObjectBudget::new(
            scope.max_objects.min(MAX_RANGE_GRAPH_OBJECTS),
            scope.max_objects,
            Arc::clone(&scope.objects_used),
            scope.metadata_quota.min(MAX_RANGE_METADATA_BYTES),
            Arc::clone(&scope.metadata_used),
        );
        let mut read_node = |oid: &NativeOid| self.read_native_node(oid, scope, &mut objects);
        let root_node = read_node(&root)?;
        validate_ordered_root_node(&root_node, &domain, expected_count)?;
        let mut entries = Vec::with_capacity(range.limit());
        collect_ordered_range_with(
            &mut read_node,
            root_node,
            &domain,
            None,
            None,
            range.lower_inclusive(),
            range.resume_after(),
            range.upper_exclusive(),
            range.limit(),
            &mut entries,
        )?;
        entries
            .into_iter()
            .map(|entry| {
                crate::row_store::TypedKey::decode_canonical(&entry.key)
                    .map_err(|_| GraphError::NonCanonicalData)
            })
            .collect()
    }
    /// Applies a batch of row mutations to the pinned primary ORP-1 root and
    /// returns the new root together with the object accounting of the pass.
    ///
    /// Mutations apply in the canonical order: deletes, then rekeys (a delete
    /// of the old key plus an insert of the new one), then inserts. Each
    /// mutation is checked against the pinned root, so a delete of an absent
    /// key or an insert of a present one is rejected instead of silently
    /// changing the row set. The batch may arrive in any order: publication
    /// sends it in canonical key byte order, which for Text keys is not
    /// logical order, and every key it names resolves to one slot applied in
    /// logical key order. A row whose final stored value is the exact
    /// value the pinned root holds is no change at all (ORNA-ROW-010): the
    /// writer's encoding is canonical and names overflow values and Blob
    /// descriptors by content address, so equal bytes are the same row.
    ///
    /// The pass is incremental (store-format-3 §3): it reads the paths to the
    /// touched keys and the pages around them, rewrites only those pages and
    /// the branches above them, and keeps every other page by object ID
    /// without reading it. The new root is the canonical complete build of
    /// the new row set. `index_objects_created` counts the new index nodes
    /// (the measured index churn), `index_objects_reused` names the old nodes
    /// the new root still names unchanged at the edge of the rewritten region,
    /// and `payload_objects_reused` the stored value and Blob-descriptor
    /// objects the rewritten pages name without this pass writing them. The
    /// new root stays unreachable until the caller names it from a row-map
    /// version or publication candidate.
    pub fn maintain_row_index(
        &self,
        mutations: &[RowIndexMutation],
        scope: &RepositoryReadScope,
    ) -> Result<IndexMaintenance, GraphError> {
        scope.authorize(self)?;
        let version = self.row_snapshot.version();
        let expected_count = version.row_count().ok_or(GraphError::ContextMismatch)?;
        let root = version.primary_root().clone();
        let domain = rows_domain_for_version(version);
        let mut objects = ObjectBudget::new(
            scope.max_objects.min(MAX_FULL_VERIFY_OBJECTS),
            scope.max_objects,
            Arc::clone(&scope.objects_used),
            scope.metadata_quota.min(MAX_FULL_METADATA_BYTES),
            Arc::clone(&scope.metadata_used),
        );
        // A pass revisits nodes (the lookup of the touched keys, then the
        // rewrite around them), so each old node is read once.
        let mut old_nodes = HashMap::<NativeOid, NodeData>::new();
        let mut read_node = |oid: &NativeOid| -> Result<NodeData, GraphError> {
            if let Some(data) = old_nodes.get(oid) {
                return Ok(data.clone());
            }
            let data = self.read_native_node(oid, scope, &mut objects)?;
            old_nodes.insert(oid.clone(), data.clone());
            Ok(data)
        };
        let root_node = read_node(&root)?;
        validate_ordered_root_node(&root_node, &domain, expected_count)?;

        let mut written = BTreeSet::new();
        let RowMapPass {
            maintained,
            row_count,
            logical_rekeys,
        } = maintain_row_map(
            &domain,
            root.clone(),
            root_node,
            expected_count,
            mutations,
            &mut read_node,
            &mut crate::overflow::CaptureStore::new(self, scope, &mut written, scope.max_objects),
        )?;
        // Every object the overflow writer and the page rewrite wrote joins
        // one closure for the durability barrier.
        self.sync_object_closure(&written, scope)?;

        let index_objects_created: Vec<NativeOid> = maintained
            .written
            .iter()
            .filter(|oid| !old_nodes.contains_key(*oid))
            .cloned()
            .collect();
        let mut index_objects_reused = maintained.reused;
        index_objects_reused.extend(
            maintained
                .written
                .iter()
                .filter(|oid| old_nodes.contains_key(*oid))
                .cloned(),
        );
        let mut payload_objects_reused = BTreeSet::new();
        for leaf in &maintained.written_leaves {
            for dependency in leaf.dependencies()? {
                if !written.contains(&dependency.oid) {
                    payload_objects_reused.insert(dependency.oid);
                }
            }
        }
        Ok(IndexMaintenance {
            previous_root: root,
            primary_root: maintained.root,
            row_count,
            logical_rekeys,
            objects_written: written.into_iter().collect(),
            index_objects_created,
            index_objects_reused: index_objects_reused.into_iter().collect(),
            payload_objects_reused: payload_objects_reused.into_iter().collect(),
        })
    }

    /// Applies typed row operations to one exact pinned relation version and
    /// returns the OID of the complete successor `.orna/store` StoreRoot.
    /// A stale or substituted version is rejected before writes.
    pub fn maintain_relation_rows(
        &self,
        expected: &crate::row_store::RowMapVersion,
        operations: &[RowIndexMutation],
        scope: &RepositoryReadScope,
    ) -> Result<NativeOid, GraphError> {
        self.maintain_relation_row_candidate(expected, operations, scope)
            .map(|candidate| candidate.store_root)
    }

    /// Publishes row-index candidates for several relations in one successor
    /// StoreRoot. Every version and index candidate must have been derived
    /// from this graph's exact source StoreRoot; no worktree row files are
    /// created or consulted.
    ///
    /// The candidates may be produced by separate relation graph contexts
    /// opened at the same snapshot. Duplicate relation IDs and stale roots
    /// are rejected. The returned root is the only root callers need to name
    /// from their publication commit.
    pub fn maintain_relations_rows(
        &self,
        candidates: &[(&crate::row_store::RowMapVersion, &IndexMaintenance)],
        scope: &RepositoryReadScope,
    ) -> Result<NativeOid, GraphError> {
        scope.authorize(self)?;
        let mut seen = BTreeSet::new();
        let mut store_root = self.store_root.clone();
        for (version, maintenance) in candidates {
            if version.database_id() != &self.database_id
                || version.store_root() != &self.store_root
                || maintenance.previous_root() != version.primary_root()
                || !seen.insert(*version.relation_id())
            {
                return Err(GraphError::ContextMismatch);
            }
            let mut objects = ObjectBudget::new(
                scope.max_objects.min(MAX_RANGE_GRAPH_OBJECTS),
                scope.max_objects,
                Arc::clone(&scope.objects_used),
                scope.metadata_quota.min(MAX_RANGE_METADATA_BYTES),
                Arc::clone(&scope.metadata_used),
            );
            let root = self.read_native_node(maintenance.primary_root(), scope, &mut objects)?;
            validate_ordered_root_node(
                &root,
                &rows_domain_for_version(version),
                maintenance.row_count(),
            )?;
            store_root = self.replace_relation_primary_root(
                &store_root,
                version,
                maintenance.primary_root(),
                maintenance.row_count(),
                scope,
            )?;
        }
        for (version, maintenance) in candidates {
            let successor = load_format3_row_map(
                &self.repository,
                self.algorithm,
                store_root.clone(),
                self.database_id,
                self.snapshot_id,
                *version.relation_id(),
            )?;
            let successor = successor.version();
            if successor.database_id() != version.database_id()
                || successor.relation_id() != version.relation_id()
                || successor.schema() != version.schema()
                || successor.primary_root() != maintenance.primary_root()
                || successor.row_count() != Some(maintenance.row_count())
                || successor.store_root() != &store_root
            {
                return Err(GraphError::ContextMismatch);
            }
        }
        Ok(store_root)
    }

    fn maintain_relation_row_candidate(
        &self,
        expected: &crate::row_store::RowMapVersion,
        operations: &[RowIndexMutation],
        scope: &RepositoryReadScope,
    ) -> Result<RelationIndexCandidate, GraphError> {
        scope.authorize(self)?;
        if expected != self.row_snapshot.version() || expected.store_root() != &self.store_root {
            return Err(GraphError::ContextMismatch);
        }

        let maintenance = self.maintain_row_index(operations, scope)?;
        let store_root = self.replace_relation_primary_root(
            &self.store_root,
            expected,
            maintenance.primary_root(),
            maintenance.row_count(),
            scope,
        )?;
        let successor = load_format3_row_map(
            &self.repository,
            self.algorithm,
            store_root.clone(),
            *expected.database_id(),
            self.snapshot_id,
            *expected.relation_id(),
        )?;
        let successor_version = successor.version();
        if successor_version.database_id() != expected.database_id()
            || successor_version.relation_id() != expected.relation_id()
            || successor_version.schema() != expected.schema()
            || successor_version.primary_root() != maintenance.primary_root()
            || successor_version.row_count() != Some(maintenance.row_count())
            || successor_version.store_root() != &store_root
        {
            return Err(GraphError::ContextMismatch);
        }

        Ok(RelationIndexCandidate {
            primary_root: maintenance.primary_root().clone(),
            row_count: maintenance.row_count(),
            store_root,
        })
    }

    /// Builds one canonical ORP-1 ordered map over `entries`, which must be in
    /// strictly ascending logical key order (store-format-3 §3), writing every
    /// node through `write_node` and returning the root. Relation admission
    /// builds complete maps here; row maintenance rewrites the pages around
    /// its changes through `crate::orp::maintenance` to the same pages. Both
    /// cut pages by exact node bytes and distinct native refs.
    pub(crate) fn build_canonical_ordered_map(
        domain: &[u8],
        entries: &[crate::row_store::RowEntry],
        write_node: &mut impl FnMut(&NodeData) -> Result<NativeOid, GraphError>,
    ) -> Result<NativeOid, GraphError> {
        let entries = entries
            .iter()
            .map(|entry| {
                Ok(OrderedLeafEntry {
                    key: entry
                        .key()
                        .canonical_bytes()
                        .map_err(|_| GraphError::NonCanonicalData)?,
                    value: entry
                        .value()
                        .canonical_page_bytes()
                        .map_err(|_| GraphError::NonCanonicalData)?,
                })
            })
            .collect::<Result<Vec<_>, GraphError>>()?;
        crate::orp::build_ordered_map(domain, entries, write_node)
    }

    /// Reads every relation entry of one `.orna/store` root in ascending
    /// relation-UUID order. The store root node, the relation map's
    /// `["relations", database_uuid]` domain, every page's key order, fences
    /// and exact counts, and each relation value's shape are validated
    /// before any entry is returned (store-format-3 §2).
    pub(crate) fn read_store_relations(
        repository: &crate::Repository,
        algorithm: GitHashAlgorithm,
        database_id: [u8; 16],
        store_root: &NativeOid,
    ) -> Result<Vec<StoreRelationEntry>, GraphError> {
        if store_root.algorithm() != algorithm {
            return Err(GraphError::InvalidOidWidth {
                expected: algorithm.width(),
                actual: store_root.as_bytes().len(),
            });
        }
        let mut reader = PinnedNativeGraphReader::new(repository.clone(), algorithm);
        let store = reader.read_native_node(store_root)?;
        let NodeData::StoreRoot { relation_map } = store else {
            return Err(GraphError::WrongNodeKind {
                expected: NodeKind::StoreRoot,
                actual: store.kind(),
            });
        };
        let domain = canonical_domain(vec![
            CborValue::Text("relations".to_owned()),
            CborValue::Bytes(database_id.to_vec()),
        ]);
        let root = reader.read_native_node(&relation_map)?;
        let count = match &root {
            NodeData::OrderedLeaf { entries, .. } => entries.len() as u64,
            NodeData::OrderedBranch { entries, .. } => {
                entries.iter().try_fold(0u64, |sum, entry| {
                    sum.checked_add(entry.row_count)
                        .filter(|count| *count <= MAX_SIGNED_LENGTH)
                        .ok_or(GraphError::InvalidCount(usize::MAX))
                })?
            }
            other => {
                return Err(GraphError::WrongNodeKind {
                    expected: NodeKind::OrderedLeaf,
                    actual: other.kind(),
                });
            }
        };
        validate_ordered_root_node(&root, &domain, count)?;
        let limit = usize::try_from(count).map_err(|_| GraphError::InvalidCount(usize::MAX))?;
        let mut read_node = |oid: &NativeOid| reader.read_native_node(oid);
        let mut nodes = BTreeSet::from([relation_map.clone()]);
        let mut payload = BTreeSet::new();
        let mut validated_contexts = BTreeSet::new();
        collect_ordered_index_objects_with(
            &mut read_node,
            &root,
            &relation_map,
            &domain,
            None,
            &mut nodes,
            &mut payload,
            &mut validated_contexts,
        )?;
        let mut raw = Vec::new();
        collect_ordered_range_with(
            &mut read_node,
            root,
            &domain,
            None,
            None,
            None,
            None,
            None,
            limit,
            &mut raw,
        )?;
        if raw.len() != limit {
            return Err(GraphError::InvalidCount(raw.len()));
        }
        raw.into_iter()
            .map(|entry| {
                let crate::row_store::TypedKey::Bytes(relation_id) =
                    crate::row_store::TypedKey::decode_canonical(&entry.key)
                        .map_err(|_| GraphError::NonCanonicalData)?
                else {
                    return Err(GraphError::InvalidDomain);
                };
                let relation_id = relation_id
                    .as_slice()
                    .try_into()
                    .map_err(|_| GraphError::InvalidDomain)?;
                let (schema_oid, primary_root, secondary_root, row_count) =
                    decode_relation_value(&entry.value, algorithm)?;
                Ok(StoreRelationEntry {
                    relation_id,
                    schema_oid,
                    primary_root,
                    secondary_root,
                    row_count,
                })
            })
            .collect()
    }


    /// Projects the payload-free Blob metadata of one admitted row field.
    ///
    /// This is the measured metadata-only read path a query over media rows
    /// takes. The row's own field tuple carries the length, SHA-256 and
    /// MIME-1 annotation of each Blob reference, so the projection returns
    /// without entering a descriptor or content read.
    /// `scope.payload_bytes_read()` therefore stays at zero for the whole
    /// projection, while `scope.objects_read()` counts the row pages the
    /// lookup walked and any overflow graph the row's fields live in.
    ///
    /// `field` is the index of the Blob field in the stored tuple; a field
    /// that is absent or not a Blob is an error.
    pub fn project_blob_metadata(
        &self,
        key: &crate::row_store::TypedKey,
        field: usize,
        scope: &RepositoryReadScope,
    ) -> Result<Option<(crate::row_store::TypedKey, BlobMetadata)>, GraphError> {
        let Some(row) = self.lookup_row(key, scope)? else {
            return Ok(None);
        };
        let metadata = self
            .row_blob_fields(&row, scope)?
            .into_iter()
            .find_map(|(index, metadata)| (index == field).then_some(metadata))
            .ok_or(GraphError::NonCanonicalData)?;
        Ok(Some((row.key().clone(), metadata)))
    }

    /// Projects the payload-free Blob metadata of a bounded row interval.
    ///
    /// Rows are returned in canonical key order, exactly as
    /// [`Self::range_rows`] orders them, but only the interval's own field
    /// tuples are decoded: no descriptor, byte index or payload chunk is read.
    /// A field that is absent from a row's tuple is skipped rather than
    /// fabricated; a row stored through the overflow graph is expanded as
    /// [`Self::project_blob_metadata`] expands it.
    pub fn project_blob_metadata_range(
        &self,
        range: &crate::row_store::KeyRange,
        field: usize,
        scope: &RepositoryReadScope,
    ) -> Result<Vec<(crate::row_store::TypedKey, BlobMetadata)>, GraphError> {
        scope.authorize(self)?;
        let version = self.row_snapshot.version();
        let expected_count = version.row_count().ok_or(GraphError::ContextMismatch)?;
        let root = version.primary_root().clone();
        let domain = rows_domain_for_version(version);
        let entries = {
            let mut objects = ObjectBudget::new(
                scope.max_objects.min(MAX_RANGE_GRAPH_OBJECTS),
                scope.max_objects,
                Arc::clone(&scope.objects_used),
                scope.metadata_quota.min(MAX_RANGE_METADATA_BYTES),
                Arc::clone(&scope.metadata_used),
            );
            let mut read_node = |oid: &NativeOid| self.read_native_node(oid, scope, &mut objects);
            let root_node = read_node(&root)?;
            validate_ordered_root_node(&root_node, &domain, expected_count)?;
            let mut entries = Vec::with_capacity(range.limit());
            collect_ordered_range_with(
                &mut read_node,
                root_node,
                &domain,
                None,
                None,
                range.lower_inclusive(),
                range.resume_after(),
                range.upper_exclusive(),
                range.limit(),
                &mut entries,
            )?;
            entries
        };
        let mut objects = ObjectBudget::new(
            scope.max_objects.min(MAX_RANGE_GRAPH_OBJECTS),
            scope.max_objects,
            Arc::clone(&scope.objects_used),
            scope.metadata_quota.min(MAX_RANGE_METADATA_BYTES),
            Arc::clone(&scope.metadata_used),
        );
        let mut projected = Vec::with_capacity(entries.len());
        for entry in entries {
            let key = crate::row_store::TypedKey::decode_canonical(&entry.key)
                .map_err(|_| GraphError::NonCanonicalData)?;
            let value = self.decode_persisted_row_value(entry.value, scope, &mut objects)?;
            let row = crate::row_store::AdmittedRow::issue(version, key, value);
            let Some(metadata) = self
                .row_blob_fields(&row, scope)?
                .into_iter()
                .find_map(|(index, metadata)| (index == field).then_some(metadata))
            else {
                continue;
            };
            projected.push((row.key().clone(), metadata));
        }
        Ok(projected)
    }

    /// Decodes the complete canonical ROV3 tuple using the committed,
    /// source-qualified descriptor and this graph's owner-issued read scope.
    /// Overflow tuple bytes are structured metadata; Blob payload chunks are
    /// never fetched by this operation.
    pub fn decode_admitted_row_fields(
        &self,
        row: &crate::row_store::AdmittedRow,
        binding: &crate::CommittedTableBinding,
        scope: &RepositoryReadScope,
    ) -> Result<AdmittedRowFields, GraphError> {
        scope.authorize(self)?;
        let version = self.row_snapshot.version();
        if row.version() != version
            || binding.snapshot_id() != &self.snapshot_id
            || binding.relation_id() != version.relation_id()
            || binding.schema_oid() != version.schema().schema_oid()
            || binding.schema_digest() != version.schema().schema_digest()
            || binding.module_name().is_none()
            || binding.source_path().is_none()
            || binding.declaration_name().is_none()
        {
            return Err(GraphError::ContextMismatch);
        }

        let canonical_key = row
            .key()
            .canonical_bytes()
            .map_err(|_| GraphError::NonCanonicalData)?;
        let canonical_fields = self.stored_row_fields(row, scope)?;

        binding
            .descriptor()
            .validate_stored_row(*version.relation_id(), &canonical_key, &canonical_fields)
            .map_err(|_| GraphError::NonCanonicalData)?;
        let CborValue::Array(values) = decode_canonical_content(&canonical_fields)? else {
            return Err(GraphError::NonCanonicalData);
        };
        let field_ids = stored_field_object_ids(binding.descriptor())?;
        if values.len() != field_ids.len() {
            return Err(GraphError::NonCanonicalData);
        }

        let mut fields = Vec::with_capacity(values.len());
        for (field_id, value) in field_ids.into_iter().zip(values) {
            let blob_metadata = crate::row_store::blob_field_metadata(&value)
                .map_err(|_| GraphError::NonCanonicalData)?;
            fields.push(AdmittedRowField {
                field_id,
                blob_metadata,
            });
        }

        Ok(AdmittedRowFields {
            version: version.clone(),
            key: row.key().clone(),
            canonical_fields,
            fields,
        })
    }

    /// Projects every Blob field of an admitted row of this graph to
    /// payload-free metadata, in field order. The fields come from the row's
    /// own tuple; a row or a field stored through the overflow graph is
    /// expanded from its value graph first. Neither path reads Blob payload.
    pub fn row_blob_fields(
        &self,
        row: &crate::row_store::AdmittedRow,
        scope: &RepositoryReadScope,
    ) -> Result<Vec<(usize, BlobMetadata)>, GraphError> {
        let fields = self.stored_row_fields(row, scope)?;
        let CborValue::Array(values) = decode_canonical_content(&fields)? else {
            return Err(GraphError::NonCanonicalData);
        };
        let mut projected = Vec::new();
        for (index, value) in values.iter().enumerate() {
            if let Some(metadata) = crate::row_store::blob_field_metadata(value)
                .map_err(|_| GraphError::NonCanonicalData)?
            {
                projected.push((index, metadata));
            }
        }
        Ok(projected)
    }

    /// The complete logical ROV-3 field tuple of one row of this graph. An
    /// inline tuple is the row's own bytes and an overflowed row is read from
    /// its verified kind-3 node; each nested overflow reference is then
    /// replaced by the value it stores, so the tuple has the shape its schema
    /// describes (store-format-3 section 5). The bytes read are charged as
    /// structured metadata; no Blob payload is read.
    pub(crate) fn stored_row_fields(
        &self,
        row: &crate::row_store::AdmittedRow,
        scope: &RepositoryReadScope,
    ) -> Result<Vec<u8>, GraphError> {
        scope.authorize(self)?;
        if row.version() != self.row_snapshot.version() {
            return Err(GraphError::ContextMismatch);
        }
        let mut objects = ObjectBudget::new(
            scope.max_objects.min(MAX_FULL_VERIFY_OBJECTS),
            scope.max_objects,
            Arc::clone(&scope.objects_used),
            scope.metadata_quota.min(MAX_FULL_METADATA_BYTES),
            Arc::clone(&scope.metadata_used),
        );
        let encoded = match row.value() {
            crate::row_store::RowValue::Inline { encoded, .. } => {
                scope.charge_structured_metadata_read(encoded.len() as u64)?;
                encoded.clone()
            }
            crate::row_store::RowValue::Overflow(reference) => {
                let root = reference
                    .native_root()
                    .ok_or(GraphError::UnsupportedRowValueForm)?;
                self.verify_value_overflow(root, scope, &mut objects, &mut BTreeSet::new(), true)?
                    .encoded_value
                    .ok_or(GraphError::ContextMismatch)?
            }
        };
        let tuple = decode_canonical_content(&encoded)?;
        let mut nested = BTreeSet::new();
        collect_overflow_oids(&tuple, self.algorithm, &mut nested)?;
        if nested.is_empty() {
            return Ok(encoded);
        }
        let expanded = self.expand_overflow(tuple, scope, &mut objects, &mut BTreeSet::new())?;
        Ok(canonical_value_bytes(&expanded))
    }

    /// Replaces each overflow reference in `value` by the ROV-3 value its
    /// verified kind-3 node stores, recursively.
    fn expand_overflow(
        &self,
        value: CborValue,
        scope: &RepositoryReadScope,
        objects: &mut ObjectBudget,
        active: &mut BTreeSet<NativeOid>,
    ) -> Result<CborValue, GraphError> {
        match value {
            CborValue::Tag(60113, payload) => {
                let oid = overflow_tag_oid(&payload, self.algorithm)?;
                if !active.insert(oid.clone()) {
                    return Err(GraphError::ContextMismatch);
                }
                let encoded = self
                    .verify_value_overflow(&oid, scope, objects, &mut BTreeSet::new(), true)?
                    .encoded_value
                    .ok_or(GraphError::ContextMismatch)?;
                let stored = self.expand_overflow(
                    decode_canonical_content(&encoded)?,
                    scope,
                    objects,
                    active,
                )?;
                active.remove(&oid);
                Ok(stored)
            }
            CborValue::Array(values) => Ok(CborValue::Array(
                values
                    .into_iter()
                    .map(|value| self.expand_overflow(value, scope, objects, active))
                    .collect::<Result<Vec<_>, _>>()?,
            )),
            CborValue::Map(entries) => Ok(CborValue::Map(
                entries
                    .into_iter()
                    .map(|(key, value)| {
                        Ok((
                            self.expand_overflow(key, scope, objects, active)?,
                            self.expand_overflow(value, scope, objects, active)?,
                        ))
                    })
                    .collect::<Result<Vec<_>, GraphError>>()?,
            )),
            // A stored Blob reference and a table reference hold no overflow.
            tagged @ CborValue::Tag(60111 | 60021, _) => Ok(tagged),
            CborValue::Tag(tag, payload) => Ok(CborValue::Tag(
                tag,
                Box::new(self.expand_overflow(*payload, scope, objects, active)?),
            )),
            scalar => Ok(scalar),
        }
    }
    fn decode_persisted_row_value(
        &self,
        encoded: Vec<u8>,
        scope: &RepositoryReadScope,
        objects: &mut ObjectBudget,
    ) -> Result<crate::row_store::RowValue, GraphError> {
        let value = decode_canonical_cbor(&encoded)?;
        let row_value = match &value {
            CborValue::Tag(60113, payload) => {
                let oid = overflow_tag_oid(payload, self.algorithm)?;
                let verified =
                    self.verify_value_overflow(&oid, scope, objects, &mut BTreeSet::new(), false)?;
                crate::row_store::RowValue::overflow(verified.reference)
            }
            CborValue::Array(_) => {
                let mut overflow_oids = BTreeSet::new();
                collect_overflow_oids(&value, self.algorithm, &mut overflow_oids)?;
                for oid in overflow_oids {
                    self.verify_value_overflow(&oid, scope, objects, &mut BTreeSet::new(), false)?;
                }
                crate::row_store::RowValue::decode_canonical_fields(encoded)
                    .map_err(|_| GraphError::NonCanonicalData)?
            }
            _ => return Err(GraphError::NonCanonicalData),
        };

        if matches!(&row_value, crate::row_store::RowValue::Overflow(_)) {
            return Ok(row_value);
        }

        for dependency in row_value.dependencies() {
            if dependency.kind() != NativeObjectKind::Tree {
                return Err(GraphError::WrongReferenceKind);
            }
            let node = self.read_native_node(dependency.oid(), scope, objects)?;
            match node {
                NodeData::BlobDescriptor { .. } => {}
                NodeData::ValueOverflow { .. } => {} // Nested overflow was verified above.
                other => {
                    return Err(GraphError::WrongNodeKind {
                        expected: NodeKind::BlobDescriptor,
                        actual: other.kind(),
                    });
                }
            }
        }
        Ok(row_value)
    }

    fn verify_value_overflow(
        &self,
        oid: &NativeOid,
        scope: &RepositoryReadScope,
        objects: &mut ObjectBudget,
        active: &mut BTreeSet<NativeOid>,
        retain_encoded: bool,
    ) -> Result<VerifiedValueOverflow, GraphError> {
        if !active.insert(oid.clone()) {
            return Err(GraphError::ContextMismatch);
        }
        let result = (|| {
            let node = self.read_native_node(oid, scope, objects)?;
            let NodeData::ValueOverflow {
                encoded_length,
                semantic_digest,
                byte_root,
                dependency_root,
            } = node
            else {
                return Err(GraphError::WrongNodeKind {
                    expected: NodeKind::ValueOverflow,
                    actual: node.kind(),
                });
            };
            if encoded_length > crate::row_store::ROW_CANONICAL_LIMIT as u64 {
                return Err(GraphError::InvalidLength(encoded_length));
            }
            objects.record_metadata(encoded_length)?;
            let capacity = usize::try_from(encoded_length)
                .map_err(|_| GraphError::InvalidLength(encoded_length))?;
            let mut encoded = Vec::new();
            encoded
                .try_reserve_exact(capacity)
                .map_err(|_| GraphError::ReadQuotaExceeded)?;
            self.read_full_index_bytes(
                &byte_root,
                encoded_length,
                None,
                scope,
                objects,
                &mut encoded,
            )?;
            if encoded.len() as u64 != encoded_length {
                return Err(GraphError::ContentIdentityMismatch);
            }
            let rov_value = decode_canonical_content(&encoded)?;
            let sov_value = self.to_sov_value(rov_value.clone(), scope, objects, active)?;
            let semantic_bytes = canonical_value_bytes(&sov_value);
            let actual_digest: [u8; 32] = Sha256::digest(&semantic_bytes).into();
            if actual_digest != semantic_digest {
                return Err(GraphError::ContentIdentityMismatch);
            }

            let mut expected_dependencies = BTreeMap::new();
            collect_row_dependencies(&rov_value, &mut expected_dependencies)?;
            let actual_dependencies = match &dependency_root {
                Some(root) => self.read_dependency_index(root, scope, objects)?,
                None => BTreeSet::new(),
            };
            if expected_dependencies
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>()
                != actual_dependencies
            {
                return Err(GraphError::ContextMismatch);
            }
            if encoded_length <= crate::row_store::ROW_INLINE_LIMIT as u64
                && direct_blob_reference_count(&rov_value)? <= crate::row_store::ROW_INLINE_REF_LIMIT
            {
                return Err(GraphError::NonCanonicalData);
            }
            for dependency in actual_dependencies {
                let dependency_node = self.read_native_node(&dependency, scope, objects)?;
                match dependency_node {
                    NodeData::BlobDescriptor { .. } => {}
                    NodeData::ValueOverflow { .. } => {
                        self.verify_value_overflow(&dependency, scope, objects, active, false)?;
                    }
                    other => {
                        return Err(GraphError::WrongNodeKind {
                            expected: NodeKind::BlobDescriptor,
                            actual: other.kind(),
                        });
                    }
                }
            }
            let reference = crate::row_store::ValueOverflowRef::from_native_node(
                oid.clone(),
                encoded_length,
                semantic_digest,
                byte_root,
                dependency_root,
            )
            .map_err(|_| GraphError::NonCanonicalData)?;
            Ok(VerifiedValueOverflow {
                reference,
                semantic_value: sov_value,
                encoded_value: retain_encoded.then_some(encoded),
            })
        })();
        active.remove(oid);
        result
    }

    fn to_sov_value(
        &self,
        value: CborValue,
        scope: &RepositoryReadScope,
        objects: &mut ObjectBudget,
        active: &mut BTreeSet<NativeOid>,
    ) -> Result<CborValue, GraphError> {
        sov3_value(value, self.algorithm.width(), &mut |payload| {
            let oid = overflow_tag_oid(&payload, self.algorithm)?;
            Ok(self
                .verify_value_overflow(&oid, scope, objects, active, false)?
                .semantic_value)
        })
    }

    fn read_dependency_index(
        &self,
        root: &NativeOid,
        scope: &RepositoryReadScope,
        objects: &mut ObjectBudget,
    ) -> Result<BTreeSet<NativeOid>, GraphError> {
        let mut output = BTreeSet::new();
        let mut previous = None;
        self.walk_dependency_index(root, None, scope, objects, &mut previous, &mut output)?;
        if output.is_empty() {
            return Err(GraphError::ContextMismatch);
        }
        Ok(output)
    }

    /// Lifts a stored row value into the canonical values it may hold.
    ///
    /// An inline value is its own encoding; a canonical overflow value names
    /// the node that holds it and is dereferenced through the same verified
    /// path the dependency check uses, so a row too large to inline is read
    /// here too.
    fn stored_row_values(
        &self,
        value: &crate::row_store::RowValue,
        scope: &RepositoryReadScope,
        objects: &mut ObjectBudget,
    ) -> Result<Vec<CborValue>, GraphError> {
        match value {
            crate::row_store::RowValue::Inline { encoded, .. } => {
                let parsed = decode_canonical_cbor(encoded)?;
                if !matches!(parsed, CborValue::Array(_) | CborValue::Tag(60113, _)) {
                    return Err(GraphError::NonCanonicalData);
                }
                Ok(vec![parsed])
            }
            crate::row_store::RowValue::Overflow(reference) => {
                let root = reference
                    .native_root()
                    .ok_or(GraphError::UnsupportedRowValueForm)?;
                let verified =
                    self.verify_value_overflow(root, scope, objects, &mut BTreeSet::new(), false)?;
                Ok(vec![verified.semantic_value])
            }
        }
    }

    fn walk_dependency_index(
        &self,
        oid: &NativeOid,
        expected_height: Option<u8>,
        scope: &RepositoryReadScope,
        objects: &mut ObjectBudget,
        previous: &mut Option<NativeOid>,
        output: &mut BTreeSet<NativeOid>,
    ) -> Result<(), GraphError> {
        let node = self.read_native_node(oid, scope, objects)?;
        let NodeData::DependencyIndex { height, children } = node else {
            return Err(GraphError::WrongNodeKind {
                expected: NodeKind::DependencyIndex,
                actual: node.kind(),
            });
        };
        if expected_height.is_some_and(|expected| expected != height) {
            return Err(GraphError::InvalidDomain);
        }
        for child in children {
            if height == 0 {
                if previous.as_ref().is_some_and(|last| last >= &child) {
                    return Err(GraphError::NonCanonicalData);
                }
                *previous = Some(child.clone());
                output.insert(child);
            } else {
                self.walk_dependency_index(
                    &child,
                    Some(height - 1),
                    scope,
                    objects,
                    previous,
                    output,
                )?;
            }
        }
        Ok(())
    }

    // This full reader is used for schema descriptors and ORP overflow tuples;
    // their chunk blobs are structured metadata, not OVB-2 media payload.
    fn read_full_index_bytes(
        &self,
        oid: &NativeOid,
        expected_length: u64,
        expected_height: Option<u8>,
        scope: &RepositoryReadScope,
        objects: &mut ObjectBudget,
        output: &mut Vec<u8>,
    ) -> Result<u8, GraphError> {
        let node = self.read_native_node(oid, scope, objects)?;
        let NodeData::ByteIndex {
            height,
            total_length,
            entries,
        } = node
        else {
            return Err(GraphError::WrongNodeKind {
                expected: NodeKind::ByteIndex,
                actual: node.kind(),
            });
        };
        if total_length != expected_length
            || expected_height.is_some_and(|expected| expected != height)
        {
            return Err(GraphError::InvalidByteIndex);
        }
        let mut sum = 0u64;
        for entry in entries {
            scope.check_cancelled()?;
            sum = sum
                .checked_add(entry.span_length)
                .ok_or(GraphError::InvalidByteIndex)?;
            if height == 0 {
                if entry.span_length > crate::blob_store::GEAR_MAXIMUM as u64 {
                    return Err(GraphError::InvalidByteIndex);
                }
                let chunk = self.read_git_object(
                    NativeObjectKind::Blob,
                    &entry.target,
                    crate::blob_store::GEAR_MAXIMUM as u64,
                    scope,
                    objects,
                )?;
                if chunk.len() as u64 != entry.span_length
                    || entry.chunk_sha256 != Some(crate::blob_store::digest_bytes(&chunk).sha256())
                {
                    return Err(GraphError::ChunkDigestMismatch);
                }
                scope.charge_structured_metadata_read(chunk.len() as u64)?;
                output
                    .try_reserve(chunk.len())
                    .map_err(|_| GraphError::ReadQuotaExceeded)?;
                output.extend_from_slice(&chunk);
            } else {
                self.read_full_index_bytes(
                    &entry.target,
                    entry.span_length,
                    Some(height - 1),
                    scope,
                    objects,
                    output,
                )?;
            }
        }
        if sum != total_length {
            return Err(GraphError::InvalidByteIndex);
        }
        Ok(height)
    }

    pub const fn algorithm(&self) -> GitHashAlgorithm {
        self.algorithm
    }

    /// The admitted `.orna/store` root this context pinned. A candidate commit
    /// that rewrites the store must nest the replacement tree in its own
    /// private index rather than reusing this identity.
    pub fn store_root(&self) -> &NativeOid {
        &self.store_root
    }

    /// Counts the format-3 nodes reachable from the admitted store root by
    /// kind, and the distinct Blob OIDs those nodes name. Node data is decoded
    /// and blob contents are never read, so the stats cost only metadata.
    pub fn object_stats(&self, scope: &RepositoryReadScope) -> Result<ObjectStats, GraphError> {
        scope.authorize(self)?;
        let mut objects = self.stats_budget(scope);
        let (stats, _) = self.walk_stats(
            &self.store_root,
            scope,
            &mut objects,
            &Reachable::default(),
            u64::MAX,
        )?;
        Ok(stats)
    }

    /// Like [`Self::object_stats`], but fails with `InventoryQuotaExceeded`
    /// before reading a node beyond `max_nodes`, so a caller can bound the
    /// walk. A walk of exactly `max_nodes` nodes succeeds.
    pub fn object_stats_limited(
        &self,
        scope: &RepositoryReadScope,
        max_nodes: u64,
    ) -> Result<ObjectStats, GraphError> {
        scope.authorize(self)?;
        let mut objects = self.stats_budget(scope);
        let (stats, _) = self.walk_stats(
            &self.store_root,
            scope,
            &mut objects,
            &Reachable::default(),
            max_nodes,
        )?;
        Ok(stats)
    }

    /// Counts only the objects reachable from the store root that were not
    /// already reachable from `since_root`, an earlier store root of this
    /// graph. Subtrees shared with `since_root` are pruned, not walked.
    pub fn object_stats_since(
        &self,
        scope: &RepositoryReadScope,
        since_root: &NativeOid,
    ) -> Result<ObjectStats, GraphError> {
        scope.authorize(self)?;
        let mut objects = self.stats_budget(scope);
        let (_, earlier) =
            self.walk_stats(since_root, scope, &mut objects, &Reachable::default(), u64::MAX)?;
        let (stats, _) =
            self.walk_stats(&self.store_root, scope, &mut objects, &earlier, u64::MAX)?;
        Ok(stats)
    }

    /// The local availability class of one native object id for this graph's
    /// repository, without fetching or hydrating it. `git` runs with lazy fetch
    /// disabled; a payload a partial clone defers is classified as
    /// `ObjectAvailability::Promised` through local promisor evidence, so it is
    /// never conflated with an object the repository does not know at all
    /// (ORNA-GIT-002). An unreadable or malformed object database is reported as
    /// `ObjectAvailability::Unknown`, never as absence.
    pub fn observe_object_availability(
        &self,
        scope: &RepositoryReadScope,
        oid: &NativeOid,
    ) -> Result<ObjectAvailability, GraphError> {
        scope.authorize(self)?;
        if oid.algorithm() != self.algorithm {
            return Err(GraphError::InvalidOidWidth {
                expected: self.algorithm.width(),
                actual: oid.as_bytes().len(),
            });
        }
        match self.git_object_kind(oid) {
            Ok(kind) => {
                // The owner context issues the observation result, so a kind
                // the probe reports is validated against the repository's own
                // object format before it is returned.
                let observed = ObservedNativeObject {
                    oid: oid.clone(),
                    kind,
                    availability: ObjectAvailability::Materialized,
                };
                let context = ValidatedRepositoryContext {
                    algorithm: self.algorithm,
                };
                Ok(validate_observed_object(&context, &observed, kind)?.availability)
            }
            Err(GraphError::PromisedObjectNotMaterialized { .. }) => {
                Ok(ObjectAvailability::Promised)
            }
            Err(GraphError::MissingObject { .. }) | Err(GraphError::UnknownObjectAvailability) => {
                Ok(observed_object_availability(
                    &self.repository,
                    &oid.to_hex(),
                ))
            }
            Err(error) => Err(error),
        }
    }

    /// Lists the candidate ids that this repository cannot resolve, in input
    /// order. An id git can resolve is left out. An id of another object
    /// format is an error, not a missing object, and any other git failure is
    /// returned as-is rather than reported as missing. Absence and a payload
    /// deferred by a partial clone are both unresolved here; use
    /// [`Self::observe_object_availability`] to tell those two apart.
    pub fn unresolved_object_ids(
        &self,
        scope: &RepositoryReadScope,
        candidates: &[NativeOid],
    ) -> Result<Vec<NativeOid>, GraphError> {
        scope.authorize(self)?;
        let mut unresolved = Vec::new();
        for candidate in candidates {
            if candidate.algorithm() != self.algorithm {
                return Err(GraphError::InvalidOidWidth {
                    expected: self.algorithm.width(),
                    actual: candidate.algorithm().width(),
                });
            }
            match self.git_object_kind(candidate) {
                Ok(_) => {}
                Err(error) if error.is_unresolved_object() => unresolved.push(candidate.clone()),
                Err(error) => return Err(error),
            }
        }
        Ok(unresolved)
    }

    fn stats_budget(&self, scope: &RepositoryReadScope) -> ObjectBudget {
        ObjectBudget::new(
            scope.max_objects.min(MAX_RANGE_GRAPH_OBJECTS),
            scope.max_objects,
            Arc::clone(&scope.objects_used),
            scope.metadata_quota.min(MAX_RANGE_METADATA_BYTES),
            Arc::clone(&scope.metadata_used),
        )
    }

    /// Walks the trees reachable from `root`, skipping any tree or blob in
    /// `skip`, and returns the counts together with every tree and blob the
    /// walk reached (skipped objects are not reached).
    fn walk_stats(
        &self,
        root: &NativeOid,
        scope: &RepositoryReadScope,
        objects: &mut ObjectBudget,
        skip: &Reachable,
        max_nodes: u64,
    ) -> Result<(ObjectStats, Reachable), GraphError> {
        let mut stats = ObjectStats::default();
        let mut reached = Reachable::default();
        let mut pending = vec![root.clone()];
        let mut visited = 0u64;
        while let Some(oid) = pending.pop() {
            if skip.trees.contains(&oid) || !reached.trees.insert(oid.clone()) {
                continue;
            }
            if visited == max_nodes {
                return Err(GraphError::InventoryQuotaExceeded);
            }
            visited += 1;
            let node = self.read_native_node(&oid, scope, objects)?;
            *stats.nodes.entry(node.kind()).or_insert(0) += 1;
            for dependency in node.dependencies()? {
                match dependency.kind() {
                    NativeObjectKind::Tree => pending.push(dependency.oid().clone()),
                    NativeObjectKind::Blob => {
                        let blob = dependency.oid().clone();
                        if !skip.blobs.contains(&blob) && reached.blobs.insert(blob) {
                            stats.blob_references += 1;
                        }
                    }
                }
            }
        }
        Ok((stats, reached))
    }

    /// Lists up to `max_commits` commits reachable from `start`, newest first,
    /// each paired with its root tree snapshot. Only commit headers and tree
    /// object kinds are read; no blob or OGB-2 chunk is opened, so history can
    /// be listed without materializing content.
    pub fn list_revision_snapshots(
        &self,
        start: &str,
        max_commits: usize,
    ) -> Result<Vec<RevisionSnapshot>, GraphError> {
        if max_commits > MAX_REVISION_WALK {
            return Err(GraphError::InventoryQuotaExceeded);
        }
        if max_commits == 0 {
            return Ok(Vec::new());
        }
        let limit = format!("--max-count={max_commits}");
        // Fields after the object ids are tab-separated. The committed seconds
        // lead because a numeric field can never be mistaken for part of the
        // author, then the author (`%an <%ae>`, so a name with spaces stays
        // intact), then the trailer block: a migration commit states its
        // coordinate there and an ordinary commit renders an empty field.
        let output = self.git_output(&[
            "log",
            "--format=%H %T%x09%ct%x09%an <%ae>%x09%(trailers:key=Orna-Migration,valueonly)",
            &limit,
            start,
        ])?;
        let text = std::str::from_utf8(&output).map_err(|_| GraphError::GitObjectMalformed)?;
        let mut snapshots = Vec::new();
        for line in text.lines() {
            let Some((header, rest)) = line.split_once('\t') else {
                return Err(GraphError::GitObjectMalformed);
            };
            let Some((committed_at, rest)) = rest.split_once('\t') else {
                return Err(GraphError::GitObjectMalformed);
            };
            let committed_at = committed_at
                .parse::<u64>()
                .map_err(|_| GraphError::GitObjectMalformed)?;
            // A trailing newline inside the trailer block would coincide with
            // the record separator, so a marker value never carries one.
            let (author, migration) = split_revision_record(rest);
            let mut fields = header.split(' ');
            let (Some(commit), Some(tree), None) = (fields.next(), fields.next(), fields.next())
            else {
                return Err(GraphError::GitObjectMalformed);
            };
            let commit = NativeOid::from_hex(self.algorithm, commit)?;
            let tree = NativeOid::from_hex(self.algorithm, tree)?;
            let actual = self.git_object_kind(&tree)?;
            if actual != NativeObjectKind::Tree {
                return Err(GraphError::WrongObjectKind {
                    expected: NativeObjectKind::Tree,
                    actual,
                });
            }
            snapshots.push(RevisionSnapshot {
                commit,
                tree,
                author: author.to_owned(),
                committed_at,
                migration,
            });
        }
        Ok(snapshots)
    }

    /// Lists up to `max_commits` revisions of one admitted row from `start`,
    /// newest first. Each revision reports whether the row's descriptor tree
    /// is reachable from that revision's root tree. The walk reads commit
    /// headers and tree objects only (`ls-tree` never opens blob contents), so
    /// no payload is materialized and no blob pin is taken.
    pub fn list_row_revisions(
        &self,
        row: &crate::row_store::AdmittedRow,
        start: &str,
        max_commits: usize,
        scope: &RepositoryReadScope,
    ) -> Result<Vec<RowRevision>, GraphError> {
        self.require_committed_row(row, scope)?;
        let descriptors: Vec<NativeObjectId> = row
            .value()
            .dependencies()
            .iter()
            .filter(|dependency| dependency.kind() == NativeObjectKind::Tree)
            .map(|dependency| dependency.oid().clone())
            .collect();
        if descriptors.is_empty() {
            return Err(GraphError::DescriptorNotInRow);
        }
        let snapshots = self.list_revision_snapshots(start, max_commits)?;
        let mut revisions = Vec::with_capacity(snapshots.len());
        for snapshot in snapshots {
            let tree = snapshot.tree().to_hex();
            self.repository
                .objects()
                .check_expansion(&tree, "")
                .map_err(|error| match error {
                    crate::RepositoryError::SourceTreeLimit => GraphError::SourceTreeLimit,
                    _ => GraphError::GitCommandFailed,
                })?;
            let listing = self.git_output(&["ls-tree", "-r", "-t", "--full-tree", &tree])?;
            let text = std::str::from_utf8(&listing).map_err(|_| GraphError::GitObjectMalformed)?;
            // Each entry is `<mode> SP <type> SP <oid> TAB <path>`.
            let present = text
                .lines()
                .filter_map(|line| line.split_once('\t'))
                .filter_map(|(meta, _)| meta.split(' ').nth(2))
                .any(|object| descriptors.iter().any(|oid| oid.to_hex() == object));
            revisions.push(RowRevision {
                commit: snapshot.commit().clone(),
                tree: snapshot.tree().clone(),
                author: snapshot.author().to_owned(),
                committed_at: snapshot.committed_at(),
                migration: snapshot.migration().map(str::to_owned),
                present,
            });
        }
        Ok(revisions)
    }

    pub const fn database_id(&self) -> &[u8; 16] {
        &self.database_id
    }

    pub const fn snapshot_id(&self) -> &[u8; 32] {
        &self.snapshot_id
    }

    pub const fn schema_digest(&self) -> &[u8; 32] {
        &self.schema_digest
    }

    /// Creates a shared read budget and cancellation binding for this exact
    /// repository owner.  Repository context, not a caller-supplied resolver,
    /// owns construction of this scope.
    pub(crate) fn issue_read_scope(
        &self,
        owner_id: [u8; 16],
        cancelled: Arc<AtomicBool>,
        byte_quota: u64,
        max_single_read: u64,
        max_objects: u64,
        metadata_quota: u64,
    ) -> Result<RepositoryReadScope, GraphError> {
        if owner_id != self.owner_id
            || byte_quota == 0
            || byte_quota > MAX_SIGNED_LENGTH
            || max_single_read == 0
            || max_single_read > MAX_RANGE_READ_BYTES
            || max_objects == 0
            || max_objects > MAX_FULL_VERIFY_OBJECTS
            || metadata_quota == 0
            || metadata_quota > MAX_FULL_METADATA_BYTES
        {
            return Err(GraphError::InvalidReadScope);
        }
        Ok(RepositoryReadScope {
            context: self.identity,
            owner_id,
            cancelled,
            bytes_used: Arc::new(AtomicU64::new(0)),
            structured_metadata_bytes_read: Arc::new(AtomicU64::new(0)),
            payload_bytes_written: Arc::new(AtomicU64::new(0)),
            objects_used: Arc::new(AtomicU64::new(0)),
            metadata_used: Arc::new(AtomicU64::new(0)),
            byte_quota,
            max_single_read,
            max_objects,
            metadata_quota,
        })
    }

    /// Admits a descriptor only when an owner-issued row contains that native
    /// tree dependency and the descriptor bytes agree with the format-3 row
    /// reference. The capability is then tied to this owner and snapshot.
    pub fn admit_blob_reference(
        &self,
        row: &crate::row_store::AdmittedRow,
        descriptor_oid: &NativeOid,
        scope: &RepositoryReadScope,
    ) -> Result<AdmittedBlobReference, GraphError> {
        self.require_row_dependency(row, descriptor_oid, scope)?;
        let mut objects = ObjectBudget::new(
            scope.max_objects.min(MAX_RANGE_GRAPH_OBJECTS),
            scope.max_objects,
            Arc::clone(&scope.objects_used),
            scope.metadata_quota.min(MAX_RANGE_METADATA_BYTES),
            Arc::clone(&scope.metadata_used),
        );
        let descriptor = self.read_blob_descriptor(descriptor_oid, scope, &mut objects)?;
        self.admit_blob(descriptor)
    }

    /// Resolves the accepted pin ref that `owner_id` holds for `pin_id`. Git is
    /// the authority, so a missing ref is `None` rather than an error.
    fn accepted_pin_target(
        &self,
        owner_id: &[u8; 16],
        pin_id: &[u8; 16],
    ) -> Result<Option<String>, GraphError> {
        let reference = accepted_content_ref(owner_id, pin_id);
        let listing = self.git_output(&[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            reference.as_str(),
        ])?;
        let listing = String::from_utf8(listing).map_err(|_| GraphError::InvalidProtectedRef)?;
        Ok(listing.lines().find_map(|line| {
            let (name, value) = line.split_once(' ')?;
            (name == reference).then(|| value.to_owned())
        }))
    }

    /// Admits the Blob that a graph-issued pin accepted. Git is the authority:
    /// the pin's accepted ref must still resolve to `descriptor_oid`, the
    /// descriptor must carry `identity`, and the scope must belong to this
    /// owner and snapshot. Callers supply the pin id, descriptor and identity
    /// from their own metadata; nothing is admitted unless Git confirms them.
    /// The accepted ref is read under this graph's owner, and under the
    /// fallback owner only when that owner holds no ref for the pin.
    pub fn admit_accepted_blob_pin(
        &self,
        pin_id: &[u8; 16],
        descriptor_oid: &NativeOid,
        identity: crate::blob_store::ContentIdentity,
        scope: &RepositoryReadScope,
    ) -> Result<AdmittedBlobReference, GraphError> {
        scope.authorize(self)?;
        if descriptor_oid.algorithm() != self.algorithm {
            return Err(GraphError::InvalidOidWidth {
                expected: self.algorithm.width(),
                actual: descriptor_oid.as_bytes().len(),
            });
        }
        let resolved = match self.accepted_pin_target(&self.owner_id, pin_id)? {
            Some(resolved) => resolved,
            None => {
                let fallback = self
                    .accepted_pin_fallback_owner_id
                    .ok_or(GraphError::InvalidProtectedRef)?;
                self.accepted_pin_target(&fallback, pin_id)?
                    .ok_or(GraphError::InvalidProtectedRef)?
            }
        };
        if NativeOid::from_hex(self.algorithm, &resolved)? != *descriptor_oid {
            return Err(GraphError::InvalidProtectedRef);
        }
        let mut objects = ObjectBudget::new(
            scope.max_objects.min(MAX_RANGE_GRAPH_OBJECTS),
            scope.max_objects,
            Arc::clone(&scope.objects_used),
            scope.metadata_quota.min(MAX_RANGE_METADATA_BYTES),
            Arc::clone(&scope.metadata_used),
        );
        let descriptor = self.read_blob_descriptor(descriptor_oid, scope, &mut objects)?;
        if descriptor.identity != identity {
            return Err(GraphError::ContentIdentityMismatch);
        }
        self.admit_blob(descriptor)
    }

    /// Reads the Blob a committed row stores, including its canonical MIME-1
    /// annotation.
    ///
    /// A Blob row's value names its own descriptor, so a reader holding only
    /// the row must not guess that OID. The row value is resolved here — an
    /// inline value directly, a canonical overflow value by dereferencing it
    /// through the verified path the dependency check already uses — and the
    /// Blob field whose recorded content identity is the descriptor's is
    /// selected. A row that stores the same content twice under different
    /// annotations is ambiguous and is refused rather than answered wrongly.
    ///
    /// The descriptor must still be one of the row's dependencies, and the
    /// returned capability is bound to this owner and snapshot. It yields the
    /// annotation and content identity alone; payload bytes stay behind
    /// [`Self::read_blob_range`].
    pub fn read_stored_blob_value(
        &self,
        row: &crate::row_store::AdmittedRow,
        descriptor_oid: &NativeOid,
        scope: &RepositoryReadScope,
    ) -> Result<AdmittedStoredBlob, GraphError> {
        self.require_committed_row(row, scope)?;
        if descriptor_oid.algorithm() != self.algorithm {
            return Err(GraphError::InvalidOidWidth {
                expected: self.algorithm.width(),
                actual: descriptor_oid.as_bytes().len(),
            });
        }
        let mut objects = ObjectBudget::new(
            scope.max_objects.min(MAX_RANGE_GRAPH_OBJECTS),
            scope.max_objects,
            Arc::clone(&scope.objects_used),
            scope.metadata_quota.min(MAX_RANGE_METADATA_BYTES),
            Arc::clone(&scope.metadata_used),
        );
        if !row.value().dependencies().iter().any(|dependency| {
            dependency.kind() == NativeObjectKind::Tree && dependency.oid() == descriptor_oid
        }) {
            return Err(GraphError::DescriptorNotInRow);
        }
        let mut candidates = Vec::new();
        for row_value in self.stored_row_values(row.value(), scope, &mut objects)? {
            let value = self.to_sov_value(row_value, scope, &mut objects, &mut BTreeSet::new())?;
            collect_blob_value_fields(&value, &mut candidates)?;
        }
        let descriptor = self.read_blob_descriptor(descriptor_oid, scope, &mut objects)?;
        let mut matching = candidates
            .into_iter()
            .filter(|(_, identity)| *identity == descriptor.identity);
        let Some((annotation, identity)) = matching.next() else {
            return Err(GraphError::DescriptorNotInRow);
        };
        if matching.next().is_some() {
            return Err(GraphError::UnsupportedRowValueForm);
        }
        debug_assert_eq!(identity, descriptor.identity);
        let reference = self.admit_blob(descriptor)?;
        Ok(AdmittedStoredBlob {
            annotation,
            reference,
        })
    }

    /// Resolves a native tree object only when an owner-issued row names it as
    /// a direct tree dependency. Objects that are merely reachable through the
    /// graph are not admitted here; callers must enter through a committed row.
    pub fn resolve_row_node(
        &self,
        row: &crate::row_store::AdmittedRow,
        oid: &NativeOid,
        scope: &RepositoryReadScope,
    ) -> Result<NodeData, GraphError> {
        self.require_row_dependency(row, oid, scope)?;
        let mut objects = ObjectBudget::new(
            scope.max_objects.min(MAX_RANGE_GRAPH_OBJECTS),
            scope.max_objects,
            Arc::clone(&scope.objects_used),
            scope.metadata_quota.min(MAX_RANGE_METADATA_BYTES),
            Arc::clone(&scope.metadata_used),
        );
        self.read_native_node(oid, scope, &mut objects)
    }

    /// Checks that `row` is still the committed row for this context.
    fn require_committed_row(
        &self,
        row: &crate::row_store::AdmittedRow,
        scope: &RepositoryReadScope,
    ) -> Result<(), GraphError> {
        scope.authorize(self)?;
        let version = row.version();
        if version != self.row_snapshot.version() {
            return Err(GraphError::ContextMismatch);
        }
        if self.lookup_row(row.key(), scope)?.as_ref() != Some(row) {
            return Err(GraphError::ContextMismatch);
        }
        Ok(())
    }

    /// Checks that `row` is still the committed row for this context and that
    /// `oid` is one of its direct tree dependencies.
    fn require_row_dependency(
        &self,
        row: &crate::row_store::AdmittedRow,
        oid: &NativeOid,
        scope: &RepositoryReadScope,
    ) -> Result<(), GraphError> {
        self.require_committed_row(row, scope)?;
        if oid.algorithm() != self.algorithm {
            return Err(GraphError::ContextMismatch);
        }
        let dependency_present = row.value().dependencies().iter().any(|dependency| {
            dependency.kind() == NativeObjectKind::Tree && dependency.oid() == oid
        });
        if !dependency_present {
            return Err(GraphError::DescriptorNotInRow);
        }
        Ok(())
    }

    /// Captures an already-authorized, stable input stream into native OGB-2
    /// objects and returns a context-bound candidate rooted by a provisional
    /// private ref. `max_bytes` is an explicit per-capture ceiling; the source
    /// is never reopened by path. Before publishing its descriptor in an ORP
    /// row, promote the candidate with [`Self::protect_captured_blob`]; the
    /// resulting pin's transfer record carries the descriptor and content
    /// identity into the row's durable acceptance transaction.
    pub fn capture_blob_candidate<R: Read>(
        &self,
        mut authorized_source: R,
        max_bytes: u64,
        scope: &RepositoryReadScope,
    ) -> Result<CapturedBlobCandidate, GraphError> {
        scope.authorize(self)?;
        if max_bytes > MAX_SIGNED_LENGTH {
            return Err(GraphError::InvalidLength(max_bytes));
        }
        if max_bytes > scope.byte_quota {
            return Err(GraphError::ReadQuotaExceeded);
        }
        let mut digest = crate::blob_store::ContentDigest::new();
        let mut chunker = StreamingGearChunker::new();
        let mut written = BTreeSet::new();
        let mut index =
            CaptureIndexBuilder::new(self, &mut written, scope.max_objects, scope);
        let mut total = 0u64;
        let mut input = [0u8; 64 * 1024];

        loop {
            scope.check_cancelled()?;
            let count = match authorized_source.read(&mut input) {
                Ok(count) => count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(GraphError::CaptureReadFailed),
            };
            if count == 0 {
                break;
            }
            let next_total = total
                .checked_add(count as u64)
                .ok_or(GraphError::InvalidLength(u64::MAX))?;
            if next_total > max_bytes {
                return Err(GraphError::ReadQuotaExceeded);
            }
            let reservation = scope.reserve_payload(count as u64)?;
            digest
                .update(&input[..count])
                .map_err(|_| GraphError::InvalidLength(next_total))?;
            for byte in &input[..count] {
                if let Some(chunk) = chunker.push(*byte) {
                    index.push_chunk_bytes(chunk)?;
                }
            }
            total = next_total;
            reservation.commit(count as u64);
            crate::publication_crash_barrier("capture-read");
        }
        if let Some(chunk) = chunker.finish() {
            index.push_chunk_bytes(chunk)?;
        }

        let identity = digest.finish();
        let byte_root = index.finish()?;
        let descriptor_oid = self.write_capture_node(
            &NodeData::BlobDescriptor {
                length: identity.length(),
                raw_sha256: identity.sha256(),
                byte_root,
            },
            &mut written,
            scope.max_objects,
        )?;
        // Root the finished graph before the potentially long full-closure
        // verification and durability pass. Git GC must not collect a child
        // edge after it has been written but before the candidate is issued.
        let pin_id = *crate::Uuid::new_v4().as_bytes();
        let provisional_ref = scratch_content_ref(&self.owner_id, &pin_id);
        if let Err(error) = self.create_protected_ref(&provisional_ref, &descriptor_oid) {
            let _ = self.delete_protected_ref(&provisional_ref, &descriptor_oid);
            return Err(error);
        }
        let cleanup = PendingPrivateRefCleanup::new(
            Arc::clone(&self.private_ref_owner),
            provisional_ref,
            descriptor_oid.clone(),
        );

        let descriptor = self.read_blob_descriptor(
            &descriptor_oid,
            scope,
            &mut ObjectBudget::new(
                scope.max_objects,
                scope.max_objects,
                Arc::clone(&scope.objects_used),
                scope.metadata_quota,
                Arc::clone(&scope.metadata_used),
            ),
        )?;
        let reference = self.admit_blob(descriptor)?;
        let objects = self.verify_full_blob(&reference, scope)?;
        self.sync_object_closure(&objects, scope)?;
        crate::publication_crash_barrier("pack-install");

        Ok(CapturedBlobCandidate {
            context: self.identity,
            repository_id: self.repository_id,
            database_id: self.database_id,
            owner_id: self.owner_id,
            snapshot_id: self.snapshot_id,
            algorithm: self.algorithm,
            pin_id,
            descriptor_oid,
            identity,
            cleanup,
        })
    }

    /// Reads one bounded half-open byte range from the descriptor's native Git
    /// graph. Returned bytes are unavailable until every intersecting chunk's
    /// native Git ID, length and raw SHA-256 have been checked.
    pub fn read_blob_range(
        &self,
        reference: &AdmittedBlobReference,
        range: Range<u64>,
        scope: &RepositoryReadScope,
    ) -> Result<VerifiedBlobRange, GraphError> {
        scope.authorize(self)?;
        if !reference.matches_context(self) {
            return Err(GraphError::ContextMismatch);
        }
        if range.start > range.end || range.end > reference.identity.length() {
            return Err(GraphError::InvalidRange);
        }
        let requested_len = range.end - range.start;
        if requested_len > scope.max_single_read {
            return Err(GraphError::ReadQuotaExceeded);
        }
        let mut objects = ObjectBudget::new(
            scope.max_objects.min(MAX_RANGE_GRAPH_OBJECTS),
            scope.max_objects,
            Arc::clone(&scope.objects_used),
            scope.metadata_quota.min(MAX_RANGE_METADATA_BYTES),
            Arc::clone(&scope.metadata_used),
        );
        let descriptor =
            self.read_blob_descriptor(&reference.descriptor_oid, scope, &mut objects)?;
        if descriptor.identity != reference.identity {
            return Err(GraphError::ContentIdentityMismatch);
        }
        let mut bytes = Vec::with_capacity(requested_len as usize);
        let verification = if requested_len == 0 {
            if descriptor.identity.length() == 0
                && descriptor.identity.sha256() == crate::blob_store::digest_bytes(&[]).sha256()
            {
                RangeVerification::FullBlob
            } else {
                RangeVerification::ReturnedChunks
            }
        } else {
            let root = descriptor
                .byte_root
                .as_ref()
                .ok_or(GraphError::InvalidEmptyBlob)?;
            self.read_index_range(
                root,
                descriptor.identity.length(),
                None,
                0,
                descriptor.identity.length(),
                &range,
                scope,
                &mut objects,
                &mut bytes,
            )?;
            if bytes.len() as u64 != requested_len {
                return Err(GraphError::InvalidByteIndex);
            }
            if range.start == 0 && range.end == descriptor.identity.length() {
                if crate::blob_store::digest_bytes(&bytes) != descriptor.identity {
                    return Err(GraphError::ContentIdentityMismatch);
                }
                RangeVerification::FullBlob
            } else {
                RangeVerification::ReturnedChunks
            }
        };
        scope.check_cancelled()?;
        let output =
            VerifiedBlobRange::after_chunk_verification(reference, range, bytes, verification)?;
        Ok(output)
    }

    /// Verifies, flushes, then returns an opaque private-ref-backed content
    /// pin. A provisional private ref is installed first so concurrent Git GC
    /// cannot collect the closure while verification is in progress.
    pub fn protect_verified_blob(
        &self,
        reference: &AdmittedBlobReference,
        scope: &RepositoryReadScope,
    ) -> Result<ProtectedContentPin, GraphError> {
        scope.authorize(self)?;
        if !reference.matches_context(self) {
            return Err(GraphError::ContextMismatch);
        }
        if reference.identity.length() > scope.byte_quota {
            return Err(GraphError::ReadQuotaExceeded);
        }
        let pin_id = *crate::Uuid::new_v4().as_bytes();
        let protected_ref = accepted_content_ref(&self.owner_id, &pin_id);
        let provisional_ref = scratch_content_ref(&self.owner_id, &pin_id);
        let cleanup = PendingPrivateRefCleanup::new(
            Arc::clone(&self.private_ref_owner),
            provisional_ref,
            reference.descriptor_oid.clone(),
        );
        self.create_protected_ref(&cleanup.reference, &reference.descriptor_oid)?;
        self.finish_protection(reference, pin_id, cleanup, protected_ref, scope)
    }

    /// Durably pins an already-admitted Blob descriptor without reading or
    /// hashing its payload. Use this only when a row save preserves that exact
    /// Blob reference; new or replaced content must use `protect_verified_blob`
    /// or `protect_captured_blob` so its bytes are verified before acceptance.
    pub fn protect_admitted_blob_reference(
        &self,
        reference: &AdmittedBlobReference,
        scope: &RepositoryReadScope,
    ) -> Result<ProtectedContentPin, GraphError> {
        scope.authorize(self)?;
        if !reference.matches_context(self) {
            return Err(GraphError::ContextMismatch);
        }

        let pin_id = *crate::Uuid::new_v4().as_bytes();
        let protected_ref = accepted_content_ref(&self.owner_id, &pin_id);
        let provisional_ref = scratch_content_ref(&self.owner_id, &pin_id);
        let mut cleanup = PendingPrivateRefCleanup::new(
            Arc::clone(&self.private_ref_owner),
            provisional_ref,
            reference.descriptor_oid.clone(),
        );
        self.create_protected_ref(&cleanup.reference, &reference.descriptor_oid)?;
        scope.check_cancelled()?;
        self.promote_protected_ref(
            &cleanup.reference,
            &protected_ref,
            &reference.descriptor_oid,
        )?;
        cleanup.disarm();
        // Preserve a possibly durable root if syncing its ref fails.
        self.sync_git_ref(&protected_ref)?;
        ProtectedContentPin::issue_after_durable_ref(
            self.repository_id,
            self.database_id,
            pin_id,
            reference,
            protected_ref,
        )
    }

    /// Verifies and promotes a candidate produced by this exact graph owner.
    /// The candidate is consumed, so it cannot be accepted twice.
    pub fn protect_captured_blob(
        &self,
        candidate: CapturedBlobCandidate,
        scope: &RepositoryReadScope,
    ) -> Result<ProtectedContentPin, GraphError> {
        let CapturedBlobCandidate {
            context,
            repository_id,
            database_id,
            owner_id,
            snapshot_id,
            algorithm,
            pin_id,
            descriptor_oid,
            identity,
            cleanup,
        } = candidate;
        if context != self.identity
            || repository_id != self.repository_id
            || database_id != self.database_id
            || owner_id != self.owner_id
            || snapshot_id != self.snapshot_id
            || algorithm != self.algorithm
            || !Arc::ptr_eq(&cleanup.owner, &self.private_ref_owner)
            || cleanup.reference != scratch_content_ref(&owner_id, &pin_id)
            || cleanup.oid != descriptor_oid
        {
            return Err(GraphError::ContextMismatch);
        }
        let reference = AdmittedBlobReference {
            context,
            database_id,
            owner_id,
            snapshot_id,
            descriptor_oid,
            identity,
        };
        scope.authorize(self)?;
        let protected_ref = accepted_content_ref(&owner_id, &pin_id);
        self.finish_protection(&reference, pin_id, cleanup, protected_ref, scope)
    }

    /// Accepts a graph-issued pin into the canonical format-3 ORP Blob value.
    /// The pin is usable only with the graph owner and snapshot that issued it,
    /// and its durable protected ref must still resolve to the verified OGB-2
    /// descriptor. The returned value is ready to place in an ORP row; its
    /// transfer evidence remains available for the durable row-acceptance
    /// transaction.
    pub fn accept_protected_blob_pin(
        &self,
        pin: ProtectedContentPin,
    ) -> Result<OrpBlobBinding, GraphError> {
        self.accept_protected_blob_pin_with_annotation(pin, "application/octet-stream", None)
    }

    /// Accepts a graph-issued pin with a MIME-1 annotation. Caller spelling is
    /// canonicalized before the canonical OVB-2/ORP Blob value is written.
    pub fn accept_protected_blob_pin_with_annotation(
        &self,
        pin: ProtectedContentPin,
        media_type: &str,
        suffix: Option<&str>,
    ) -> Result<OrpBlobBinding, GraphError> {
        let expected_ref = accepted_content_ref(&pin.owner_id, &pin.pin_id);
        if pin.context != self.identity
            || pin.repository_id != self.repository_id
            || pin.database_id != self.database_id
            || pin.owner_id != self.owner_id
            || pin.snapshot_id != self.snapshot_id
            || pin.descriptor_oid.algorithm() != self.algorithm
            || pin.protected_ref != expected_ref
        {
            return Err(GraphError::ContextMismatch);
        }

        let output = self.git_output(&["rev-parse", "--verify", &pin.protected_ref])?;
        let resolved = std::str::from_utf8(&output)
            .map_err(|_| GraphError::InvalidProtectedRef)?
            .trim();
        let resolved = NativeOid::from_hex(self.algorithm, resolved)?;
        if resolved != pin.descriptor_oid {
            return Err(GraphError::InvalidProtectedRef);
        }

        let annotation = MimeRegistry::mime1()
            .canonical_annotation(media_type, suffix)
            .map_err(|_| GraphError::InvalidBlobAnnotation)?;

        let encoded_value = canonical_value_bytes(&CborValue::Tag(
            60111,
            Box::new(CborValue::Array(vec![
                CborValue::Unsigned(pin.identity.length()),
                CborValue::Bytes(pin.identity.sha256().to_vec()),
                CborValue::Text(annotation.media_type().to_owned()),
                annotation
                    .suffix()
                    .map_or(CborValue::Null, |suffix| CborValue::Text(suffix.to_owned())),
                CborValue::Bytes(pin.descriptor_oid.as_bytes().to_vec()),
            ])),
        ));
        Ok(OrpBlobBinding { encoded_value, pin })
    }

    fn replace_relation_primary_root(
        &self,
        store_root: &NativeOid,
        version: &crate::row_store::RowMapVersion,
        primary_root: &NativeOid,
        row_count: u64,
        scope: &RepositoryReadScope,
    ) -> Result<NativeOid, GraphError> {
        let mut objects = ObjectBudget::new(
            scope.max_objects.min(MAX_RANGE_GRAPH_OBJECTS),
            scope.max_objects,
            Arc::clone(&scope.objects_used),
            scope.metadata_quota.min(MAX_RANGE_METADATA_BYTES),
            Arc::clone(&scope.metadata_used),
        );
        let root = self.read_native_node(store_root, scope, &mut objects)?;
        let NodeData::StoreRoot { relation_map } = root else {
            return Err(GraphError::WrongNodeKind {
                expected: NodeKind::StoreRoot,
                actual: root.kind(),
            });
        };
        let relation_node = self.read_native_node(&relation_map, scope, &mut objects)?;
        let domain = canonical_value_bytes(&CborValue::Array(vec![
            CborValue::Text("relations".to_owned()),
            CborValue::Bytes(version.database_id().to_vec()),
        ]));
        let key = crate::row_store::TypedKey::Bytes(version.relation_id().to_vec());
        let relation = {
            let mut read_node = |oid: &NativeOid| self.read_native_node(oid, scope, &mut objects);
            find_ordered_entry_with(&mut read_node, relation_node.clone(), &domain, &key)?
                .ok_or(GraphError::ContextMismatch)?
        };
        let (schema_oid, old_primary, secondary, old_count) =
            decode_relation_value(&relation.value, self.algorithm)?;
        // A primary-only rewrite would stale any independent secondary index.
        if schema_oid != *version.schema().schema_oid()
            || old_primary != *version.primary_root()
            || secondary.is_some()
            || old_count != version.row_count().ok_or(GraphError::ContextMismatch)?
        {
            return Err(GraphError::ContextMismatch);
        }
        let value = canonical_value_bytes(&CborValue::Array(vec![
            CborValue::Bytes(schema_oid.as_bytes().to_vec()),
            CborValue::Bytes(primary_root.as_bytes().to_vec()),
            CborValue::Null,
            CborValue::Unsigned(row_count),
        ]));
        let mut written = BTreeSet::new();
        let new_relation_root = {
            let mut read_node = |oid: &NativeOid| self.read_native_node(oid, scope, &mut objects);
            let mut write_node =
                |node: &NodeData| self.write_capture_node(node, &mut written, scope.max_objects);
            replace_ordered_entry_with(
                &mut read_node,
                &mut write_node,
                relation_node,
                &domain,
                None,
                None,
                &key,
                value,
            )?
        };
        let store_root = self.write_capture_node(
            &NodeData::StoreRoot {
                relation_map: new_relation_root,
            },
            &mut written,
            scope.max_objects,
        )?;
        self.sync_object_closure(&written, scope)?;
        Ok(store_root)
    }

    /// Prepares one graph-backed ORP row insertion. The transaction module
    /// keeps the resulting root protected until its caller-owned durable
    /// publication/runtime-intent callback returns.
    pub(crate) fn prepare_protected_blob_row(
        &self,
        pin: ProtectedContentPin,
        media_type: &str,
        suffix: Option<&str>,
        mutation: crate::publication_transaction::ProtectedBlobRowInsert,
    ) -> Result<PreparedOrpGraphCandidate, ProtectedBlobRowPreparationError> {
        let protected_pin_ref = pin.protected_ref.clone();
        let protected_pin_oid = pin.descriptor_oid.clone();
        let mut pin_cleanup = Some(PendingPrivateRefCleanup::new(
            Arc::clone(&self.private_ref_owner),
            protected_pin_ref,
            protected_pin_oid,
        ));
        let mut candidate_cleanup = None;
        let preparation = (|| -> Result<PreparedOrpGraphCandidate, GraphError> {
            let binding =
                self.accept_protected_blob_pin_with_annotation(pin, media_type, suffix)?;
            let version = self.row_snapshot.version();

            let scope = self.open_read_scope()?;
            let key = mutation.key().clone();
            let mut encoded_row = Vec::new();
            array(&mut encoded_row, 1);
            encoded_row.extend_from_slice(binding.encoded_value());
            let candidate = self.maintain_relation_row_candidate(
                version,
                &[RowIndexMutation::Insert {
                    key: key.clone(),
                    fields: encoded_row,
                }],
                &scope,
            )?;
            let primary_root = candidate.primary_root;
            let row_count = candidate.row_count;
            let store_root = candidate.store_root;

            let provisional_ref = format!(
                "refs/orna/pins/{}/scratch/row-{}",
                hex_encode(&self.owner_id),
                hex_encode(crate::Uuid::new_v4().as_bytes())
            );
            candidate_cleanup = Some(PendingPrivateRefCleanup::new(
                Arc::clone(&self.private_ref_owner),
                provisional_ref,
                store_root.clone(),
            ));
            let cleanup = candidate_cleanup
                .as_ref()
                .expect("candidate cleanup remains armed during preparation");
            self.create_protected_ref(&cleanup.reference, &store_root)?;
            self.sync_git_ref(&cleanup.reference)?;

            let candidate = OrpGraphCandidate {
                context: self.identity,
                database_id: *version.database_id(),
                relation_id: *version.relation_id(),
                schema_digest: *version.schema().schema_digest(),
                previous_root: version.primary_root().clone(),
                primary_root,
                store_root,
                row_count,
                key,
                content_identity: binding.content_identity(),
                media_type: media_type.to_owned(),
                suffix: suffix.map(str::to_owned),
                transfer: binding.transfer_record(),
            };

            Ok(PreparedOrpGraphCandidate {
                candidate,
                _binding: binding,
                _cleanup: candidate_cleanup
                    .take()
                    .expect("candidate cleanup remains armed after preparation"),
                pin_cleanup: pin_cleanup
                    .take()
                    .expect("protected pin cleanup remains armed during preparation"),
            })
        })();

        match preparation {
            Ok(prepared) => Ok(prepared),
            Err(graph) => {
                let mut cleanup_failures = Vec::new();
                if let Some(candidate_cleanup) = candidate_cleanup.as_mut() {
                    if let Err(failure) = candidate_cleanup.cleanup_with_identity() {
                        cleanup_failures.push(failure);
                    }
                }
                if let Some(pin_cleanup) = pin_cleanup.as_mut() {
                    if let Err(failure) = pin_cleanup.cleanup_with_identity() {
                        cleanup_failures.push(failure);
                    }
                }
                if cleanup_failures.is_empty() {
                    Err(ProtectedBlobRowPreparationError::Graph(graph))
                } else {
                    Err(ProtectedBlobRowPreparationError::Cleanup {
                        graph,
                        cleanup: cleanup_failures,
                    })
                }
            }
        }
    }

    /// Looks up the inserted row's Blob metadata from an uncommitted or
    /// successfully committed candidate. This follows ORP metadata only and
    /// never reads the protected Blob payload.
    pub fn lookup_candidate_blob_metadata(
        &self,
        candidate: &OrpGraphCandidate,
        key: &crate::row_store::TypedKey,
        scope: &RepositoryReadScope,
    ) -> Result<Option<ProtectedBlobMetadata>, GraphError> {
        scope.authorize(self)?;
        if candidate.context != self.identity
            || candidate.database_id != self.database_id
            || candidate.relation_id != *self.row_snapshot.version().relation_id()
            || candidate.schema_digest != *self.row_snapshot.version().schema().schema_digest()
            || candidate.primary_root.algorithm() != self.algorithm
            || candidate.store_root.algorithm() != self.algorithm
        {
            return Err(GraphError::ContextMismatch);
        }
        if key != &candidate.key {
            return Ok(None);
        }
        let mut objects = ObjectBudget::new(
            scope.max_objects.min(MAX_RANGE_GRAPH_OBJECTS),
            scope.max_objects,
            Arc::clone(&scope.objects_used),
            scope.metadata_quota.min(MAX_RANGE_METADATA_BYTES),
            Arc::clone(&scope.metadata_used),
        );
        let domain = rows_domain_for_parts(&candidate.relation_id, &candidate.schema_digest);
        let node = self.read_native_node(&candidate.primary_root, scope, &mut objects)?;
        validate_ordered_root_node(&node, &domain, candidate.row_count)?;
        let mut read_node = |oid: &NativeOid| self.read_native_node(oid, scope, &mut objects);
        let entry = find_ordered_entry_with(&mut read_node, node, &domain, key)?;
        entry
            .map(|entry| {
                decode_candidate_blob_metadata(
                    &entry.value,
                    &candidate.transfer,
                    &candidate.media_type,
                    candidate.suffix.as_deref(),
                )
            })
            .transpose()
    }

    fn finish_protection(
        &self,
        reference: &AdmittedBlobReference,
        pin_id: [u8; 16],
        mut cleanup: PendingPrivateRefCleanup,
        protected_ref: String,
        scope: &RepositoryReadScope,
    ) -> Result<ProtectedContentPin, GraphError> {
        let objects = self.verify_full_blob(reference, scope)?;
        self.sync_object_closure(&objects, scope)?;
        scope.check_cancelled()?;
        self.promote_protected_ref(
            &cleanup.reference,
            &protected_ref,
            &reference.descriptor_oid,
        )?;
        cleanup.disarm();
        // If syncing the promoted ref fails, leave it rooted. Returning no pin
        // is safe; deleting a possibly durable root could expose objects to GC.
        self.sync_git_ref(&protected_ref)?;
        crate::publication_crash_barrier("pin");
        ProtectedContentPin::issue_after_durable_ref(
            self.repository_id,
            self.database_id,
            pin_id,
            reference,
            protected_ref,
        )
    }

    /// Removes a private ref only with a PUB-3 receipt for this exact pin.
    /// Dropping a pin handle alone never invokes this operation.
    pub fn release_published_pin(
        &self,
        pin: ProtectedContentPin,
        receipt: Pub3ReleaseReceipt,
    ) -> Result<(), GraphError> {
        if !receipt.authorizes(&pin) || pin.repository_id != self.repository_id {
            return Err(GraphError::PinReceiptMismatch);
        }
        self.delete_protected_ref(&pin.protected_ref, &pin.descriptor_oid)
    }

    /// Admits a Blob only from already-verified descriptor evidence in this
    /// exact owner and snapshot.  The evidence constructor is private to this
    /// module's future Git reader, so public OIDs cannot mint capabilities.
    pub(crate) fn admit_blob(
        &self,
        descriptor: VerifiedBlobDescriptor,
    ) -> Result<AdmittedBlobReference, GraphError> {
        if descriptor.descriptor_oid.algorithm() != self.algorithm {
            return Err(GraphError::InvalidOidWidth {
                expected: self.algorithm.width(),
                actual: descriptor.descriptor_oid.as_bytes().len(),
            });
        }
        Ok(AdmittedBlobReference {
            context: self.identity,
            database_id: self.database_id,
            owner_id: self.owner_id,
            snapshot_id: self.snapshot_id,
            descriptor_oid: descriptor.descriptor_oid,
            identity: descriptor.identity,
        })
    }

    fn read_blob_descriptor(
        &self,
        descriptor_oid: &NativeOid,
        scope: &RepositoryReadScope,
        objects: &mut ObjectBudget,
    ) -> Result<VerifiedBlobDescriptor, GraphError> {
        let node = self.read_native_node(descriptor_oid, scope, objects)?;
        let NodeData::BlobDescriptor {
            length,
            raw_sha256,
            byte_root,
        } = node
        else {
            return Err(GraphError::WrongNodeKind {
                expected: NodeKind::BlobDescriptor,
                actual: node.kind(),
            });
        };
        if length > MAX_SIGNED_LENGTH || (length == 0) != byte_root.is_none() {
            return Err(GraphError::InvalidEmptyBlob);
        }
        let identity = crate::blob_store::ContentIdentity::new(length, raw_sha256)
            .map_err(|_| GraphError::InvalidLength(length))?;
        if length == 0 && identity.sha256() != crate::blob_store::digest_bytes(&[]).sha256() {
            return Err(GraphError::InvalidEmptyBlob);
        }
        if let Some(root) = &byte_root {
            if root.algorithm() != self.algorithm {
                return Err(GraphError::InvalidOidWidth {
                    expected: self.algorithm.width(),
                    actual: root.as_bytes().len(),
                });
            }
        }
        Ok(VerifiedBlobDescriptor {
            descriptor_oid: descriptor_oid.clone(),
            identity,
            byte_root,
        })
    }

    fn read_native_node(
        &self,
        oid: &NativeOid,
        scope: &RepositoryReadScope,
        objects: &mut ObjectBudget,
    ) -> Result<NodeData, GraphError> {
        if oid.algorithm() != self.algorithm {
            return Err(GraphError::InvalidOidWidth {
                expected: self.algorithm.width(),
                actual: oid.as_bytes().len(),
            });
        }
        let raw_tree = self.read_git_object(
            NativeObjectKind::Tree,
            oid,
            MAX_INDEXED_TREE_BYTES,
            scope,
            objects,
        )?;
        let entries = parse_git_tree(&raw_tree, self.algorithm, 2)?;
        let mut data_oid = None;
        let mut refs_oid = None;
        for entry in entries {
            match entry.name.as_slice() {
                b"data" if entry.mode.as_slice() == b"100644" => data_oid = Some(entry.oid),
                b"refs" if entry.mode.as_slice() == b"40000" => refs_oid = Some(entry.oid),
                _ => return Err(GraphError::InvalidTreeEnvelope),
            }
        }
        let data_oid = data_oid.ok_or(GraphError::MissingNodeData)?;
        let data = self.read_git_object(
            NativeObjectKind::Blob,
            &data_oid,
            NODE_DATA_LIMIT as u64,
            scope,
            objects,
        )?;
        let refs = if let Some(refs_oid) = refs_oid {
            let raw_refs = self.read_git_object(
                NativeObjectKind::Tree,
                &refs_oid,
                MAX_INDEXED_TREE_BYTES,
                scope,
                objects,
            )?;
            let entries = parse_git_tree(&raw_refs, self.algorithm, MAX_REFS)?;
            let mut refs = Vec::with_capacity(entries.len());
            for entry in entries {
                let oid_from_name = NativeOid::from_hex(
                    self.algorithm,
                    std::str::from_utf8(&entry.name).map_err(|_| GraphError::MalformedOid)?,
                )?;
                if oid_from_name != entry.oid {
                    return Err(GraphError::MalformedReferenceName);
                }
                let kind = match entry.mode.as_slice() {
                    b"100644" => NativeObjectKind::Blob,
                    b"40000" => NativeObjectKind::Tree,
                    _ => return Err(GraphError::WrongReferenceKind),
                };
                objects.edge()?;
                refs.push(NativeRefEntry::new(oid_from_name, kind));
            }
            refs
        } else {
            Vec::new()
        };
        let envelope = NativeNodeEnvelope::new(data, refs);
        validate_native_node(
            &ValidatedRepositoryContext {
                algorithm: self.algorithm,
            },
            &envelope,
        )
    }

    fn read_index_range(
        &self,
        oid: &NativeOid,
        expected_length: u64,
        expected_height: Option<u8>,
        base_offset: u64,
        blob_length: u64,
        requested: &Range<u64>,
        scope: &RepositoryReadScope,
        objects: &mut ObjectBudget,
        output: &mut Vec<u8>,
    ) -> Result<u8, GraphError> {
        scope.check_cancelled()?;
        let node = self.read_native_node(oid, scope, objects)?;
        let NodeData::ByteIndex {
            height,
            total_length,
            entries,
        } = node
        else {
            return Err(GraphError::WrongNodeKind {
                expected: NodeKind::ByteIndex,
                actual: node.kind(),
            });
        };
        if total_length != expected_length
            || expected_height.is_some_and(|expected| expected != height)
        {
            return Err(GraphError::InvalidByteIndex);
        }
        let mut local = 0u64;
        for entry in entries {
            let child_start = base_offset
                .checked_add(local)
                .ok_or(GraphError::InvalidByteIndex)?;
            let child_end = child_start
                .checked_add(entry.span_length)
                .ok_or(GraphError::InvalidByteIndex)?;
            local = local
                .checked_add(entry.span_length)
                .ok_or(GraphError::InvalidByteIndex)?;
            let overlap_start = child_start.max(requested.start);
            let overlap_end = child_end.min(requested.end);
            if overlap_start >= overlap_end {
                continue;
            }
            if height == 0 {
                if entry.span_length > crate::blob_store::GEAR_MAXIMUM as u64
                    || (child_end != blob_length
                        && entry.span_length < crate::blob_store::GEAR_MINIMUM as u64)
                {
                    return Err(GraphError::InvalidByteIndex);
                }
                let reservation = scope.reserve_payload(entry.span_length)?;
                let chunk = self.read_git_object(
                    NativeObjectKind::Blob,
                    &entry.target,
                    crate::blob_store::GEAR_MAXIMUM as u64,
                    scope,
                    objects,
                )?;
                if chunk.len() as u64 != entry.span_length
                    || entry.chunk_sha256 != Some(crate::blob_store::digest_bytes(&chunk).sha256())
                {
                    return Err(GraphError::ChunkDigestMismatch);
                }
                let from = usize::try_from(overlap_start - child_start)
                    .map_err(|_| GraphError::InvalidRange)?;
                let to = usize::try_from(overlap_end - child_start)
                    .map_err(|_| GraphError::InvalidRange)?;
                output.extend_from_slice(chunk.get(from..to).ok_or(GraphError::InvalidRange)?);
                reservation.commit(entry.span_length);
            } else {
                self.read_index_range(
                    &entry.target,
                    entry.span_length,
                    Some(height - 1),
                    child_start,
                    blob_length,
                    requested,
                    scope,
                    objects,
                    output,
                )?;
            }
        }
        if local != total_length {
            return Err(GraphError::InvalidByteIndex);
        }
        Ok(height)
    }

    fn verify_full_blob(
        &self,
        reference: &AdmittedBlobReference,
        scope: &RepositoryReadScope,
    ) -> Result<BTreeSet<NativeOid>, GraphError> {
        scope.check_cancelled()?;
        let mut objects = ObjectBudget::new(
            scope.max_objects,
            scope.max_objects,
            Arc::clone(&scope.objects_used),
            scope.metadata_quota,
            Arc::clone(&scope.metadata_used),
        );
        let descriptor =
            self.read_blob_descriptor(&reference.descriptor_oid, scope, &mut objects)?;
        if descriptor.identity != reference.identity {
            return Err(GraphError::ContentIdentityMismatch);
        }
        if descriptor.identity.length() > scope.byte_quota {
            return Err(GraphError::ReadQuotaExceeded);
        }
        let mut digest = crate::blob_store::ContentDigest::new();
        let mut chunk_boundaries = GearBoundaryVerifier::new();
        if let Some(root) = descriptor.byte_root.as_ref() {
            let height = self.walk_full_index(
                root,
                descriptor.identity.length(),
                None,
                scope,
                &mut objects,
                &mut digest,
                &mut chunk_boundaries,
            )?;
            if height > MAX_GRAPH_HEIGHT {
                return Err(GraphError::HeightExceeded(height));
            }
        }
        if digest.finish() != descriptor.identity {
            return Err(GraphError::ContentIdentityMismatch);
        }
        Ok(objects.oids)
    }

    fn walk_full_index(
        &self,
        oid: &NativeOid,
        expected_length: u64,
        expected_height: Option<u8>,
        scope: &RepositoryReadScope,
        objects: &mut ObjectBudget,
        digest: &mut crate::blob_store::ContentDigest,
        chunk_boundaries: &mut GearBoundaryVerifier,
    ) -> Result<u8, GraphError> {
        scope.check_cancelled()?;
        let node = self.read_native_node(oid, scope, objects)?;
        let NodeData::ByteIndex {
            height,
            total_length,
            entries,
        } = node
        else {
            return Err(GraphError::WrongNodeKind {
                expected: NodeKind::ByteIndex,
                actual: node.kind(),
            });
        };
        if total_length != expected_length
            || expected_height.is_some_and(|expected| expected != height)
        {
            return Err(GraphError::InvalidByteIndex);
        }
        let mut sum = 0u64;
        for entry in entries {
            scope.check_cancelled()?;
            sum = sum
                .checked_add(entry.span_length)
                .ok_or(GraphError::InvalidByteIndex)?;
            if height == 0 {
                if entry.span_length > crate::blob_store::GEAR_MAXIMUM as u64 {
                    return Err(GraphError::InvalidByteIndex);
                }
                let reservation = scope.reserve_payload(entry.span_length)?;
                let chunk = self.read_git_object(
                    NativeObjectKind::Blob,
                    &entry.target,
                    crate::blob_store::GEAR_MAXIMUM as u64,
                    scope,
                    objects,
                )?;
                if chunk.len() as u64 != entry.span_length
                    || entry.chunk_sha256 != Some(crate::blob_store::digest_bytes(&chunk).sha256())
                {
                    return Err(GraphError::ChunkDigestMismatch);
                }
                chunk_boundaries.push_chunk(&chunk)?;
                digest
                    .update(&chunk)
                    .map_err(|_| GraphError::InvalidLength(u64::MAX))?;
                reservation.commit(entry.span_length);
            } else {
                self.walk_full_index(
                    &entry.target,
                    entry.span_length,
                    Some(height - 1),
                    scope,
                    objects,
                    digest,
                    chunk_boundaries,
                )?;
            }
        }
        if sum != total_length {
            return Err(GraphError::InvalidByteIndex);
        }
        Ok(height)
    }

    /// How many reads this context has answered from its node cache.
    #[cfg(test)]
    pub(crate) fn node_cache_hits(&self) -> usize {
        self.node_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .hits
    }

    fn read_git_object(
        &self,
        kind: NativeObjectKind,
        oid: &NativeOid,
        maximum: u64,
        scope: &RepositoryReadScope,
        objects: &mut ObjectBudget,
    ) -> Result<Vec<u8>, GraphError> {
        scope.check_cancelled()?;
        objects.visit(oid)?;
        let admit = |size: u64| {
            if size > maximum {
                return Err(
                    if kind == NativeObjectKind::Blob
                        && maximum == crate::blob_store::GEAR_MAXIMUM as u64
                    {
                        GraphError::ChunkTooLarge(size)
                    } else {
                        GraphError::NodeDataTooLarge(usize::try_from(size).unwrap_or(usize::MAX))
                    },
                );
            }
            if maximum != crate::blob_store::GEAR_MAXIMUM as u64 {
                objects.record_metadata(size)?;
            }
            Ok(())
        };
        let cached = self
            .node_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(kind, oid);
        let data = match cached {
            Some(data) => {
                admit(data.len() as u64)?;
                data.as_ref().clone()
            }
            None => {
                let data =
                    read_checked_object(&self.repository, self.algorithm, kind, oid, admit)?;
                self.node_cache
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(kind, oid, data.clone());
                data
            }
        };
        scope.check_cancelled()?;
        Ok(data)
    }

    fn git_object_kind(&self, oid: &NativeOid) -> Result<NativeObjectKind, GraphError> {
        let hex = oid.to_hex();
        let output = self.git_output(&["cat-file", "-t", &hex])?;
        match output.as_slice() {
            b"tree\n" => Ok(NativeObjectKind::Tree),
            b"blob\n" => Ok(NativeObjectKind::Blob),
            _ => Err(GraphError::UnsupportedGitObjectType),
        }
    }

    fn git_output(&self, args: &[&str]) -> Result<Vec<u8>, GraphError> {
        let output = self
            .git_command()
            .args(args)
            .output()
            .map_err(|_| GraphError::GitCommandFailed)?;
        if !output.status.success() {
            return Err(match cat_file_object_id(args) {
                Some(object_id) => absent_object_error(&self.repository, object_id),
                None => GraphError::GitCommandFailed,
            });
        }
        Ok(output.stdout)
    }

    pub(crate) fn write_capture_object(
        &self,
        kind: &str,
        bytes: &[u8],
        written: &mut BTreeSet<NativeOid>,
        object_limit: u64,
    ) -> Result<NativeOid, GraphError> {
        let mut child = self
            .git_command()
            .args(["hash-object", "-w", "-t", kind, "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| GraphError::GitCommandFailed)?;
        child
            .stdin
            .take()
            .ok_or(GraphError::GitCommandFailed)?
            .write_all(bytes)
            .map_err(|_| GraphError::GitCommandFailed)?;
        let output = child
            .wait_with_output()
            .map_err(|_| GraphError::GitCommandFailed)?;
        if !output.status.success() {
            return Err(GraphError::GitCommandFailed);
        }
        let hex = std::str::from_utf8(&output.stdout)
            .map_err(|_| GraphError::GitObjectMalformed)?
            .trim();
        let oid = NativeOid::from_hex(self.algorithm, hex)?;
        if !written.contains(&oid) && written.len() as u64 >= object_limit {
            return Err(GraphError::InventoryQuotaExceeded);
        }
        written.insert(oid.clone());
        Ok(oid)
    }

    pub(crate) fn write_capture_node(
        &self,
        node: &NodeData,
        written: &mut BTreeSet<NativeOid>,
        object_limit: u64,
    ) -> Result<NativeOid, GraphError> {
        self.write_capture_envelope(node, written, object_limit)
    }

    /// Writes one complete OGS-1 node envelope (its canonical `data` blob, its
    /// `refs` tree when the node has dependencies, and the envelope tree that
    /// names both) into the repository's single object store.
    ///
    /// The returned object ID covers the whole envelope, so a later
    /// `read_native_node` of it re-derives and verifies every dependency edge.
    /// Objects stay unreachable until a caller-issued protected ref or commit
    /// names them, which is the durable writer a publication candidate needs
    /// for its encoded ORP nodes. `written` accumulates the exact closure so
    /// the caller can run its own durability barrier before advancing a ref.
    pub(crate) fn write_native_node(
        &self,
        node: &NodeData,
        written: &mut BTreeSet<NativeOid>,
    ) -> Result<NativeOid, GraphError> {
        self.write_capture_node(node, written, MAX_FULL_VERIFY_OBJECTS)
    }

    /// Records the fixed `.orna/store` tree position in a caller-owned private
    /// index, so a candidate commit built from that index exposes the encoded
    /// store root at the exact path the format-3 reader resolves.
    ///
    /// The store root is an OGS-1 tree, not a regular file, so the ordinary
    /// managed-file path cannot express it. This checks the supplied object is
    /// a real, fully decodable `StoreRoot` node first: a raw OID cannot be
    /// planted at the store position. The base commit's own store entries are
    /// removed before the new subtree is bound, because a directory entry may
    /// not overlap the file entries the base contributed. The caller keeps
    /// ownership of the index file and of any later ref advancement.
    pub(crate) fn pin_store_subtree_in_index(
        &self,
        index: &std::path::Path,
        store_root: &NativeOid,
    ) -> Result<(), GraphError> {
        if store_root.algorithm() != self.algorithm {
            return Err(GraphError::InvalidOidWidth {
                expected: self.algorithm.width(),
                actual: store_root.as_bytes().len(),
            });
        }
        let scope = self.open_read_scope()?;
        let mut objects = ObjectBudget::new(
            scope.max_objects.min(MAX_RANGE_GRAPH_OBJECTS),
            scope.max_objects,
            Arc::clone(&scope.objects_used),
            scope.metadata_quota.min(MAX_RANGE_METADATA_BYTES),
            Arc::clone(&scope.metadata_used),
        );
        let node = self.read_native_node(store_root, &scope, &mut objects)?;
        if node.kind() != NodeKind::StoreRoot {
            return Err(GraphError::WrongNodeKind {
                expected: NodeKind::StoreRoot,
                actual: node.kind(),
            });
        }
        // The base commit's own store entries are removed before the new
        // subtree is bound, because a directory entry may not overlap the file
        // entries the base contributed.
        crate::bind_tree_entry_in_index(
            || self.git_command(),
            index,
            crate::init::format_context::STORE_PATH,
            "040000",
            &store_root.to_hex(),
        )
        .map_err(|_| GraphError::GitCommandFailed)?;
        Ok(())
    }

    fn write_capture_envelope(
        &self,
        node: &NodeData,
        written: &mut BTreeSet<NativeOid>,
        object_limit: u64,
    ) -> Result<NativeOid, GraphError> {
        let envelope = NativeNodeEnvelope::from_node(node)?;
        if envelope.data.len() > NODE_DATA_LIMIT {
            return Err(GraphError::NodeDataTooLarge(envelope.data.len()));
        }
        let data_oid = self.write_capture_object("blob", &envelope.data, written, object_limit)?;
        let mut root_entries = vec![ParsedGitTreeEntry {
            mode: b"100644".to_vec(),
            name: b"data".to_vec(),
            oid: data_oid,
        }];
        if !envelope.refs.is_empty() {
            let mut refs_entries = Vec::with_capacity(envelope.refs.len());
            for reference in envelope.refs {
                refs_entries.push(ParsedGitTreeEntry {
                    mode: match reference.kind {
                        NativeObjectKind::Blob => b"100644".to_vec(),
                        NativeObjectKind::Tree => b"40000".to_vec(),
                    },
                    name: reference.oid.to_hex().into_bytes(),
                    oid: reference.oid,
                });
            }
            let refs_bytes = encode_git_tree(refs_entries)?;
            let refs_oid = self.write_capture_object("tree", &refs_bytes, written, object_limit)?;
            root_entries.push(ParsedGitTreeEntry {
                mode: b"40000".to_vec(),
                name: b"refs".to_vec(),
                oid: refs_oid,
            });
        }
        let tree = encode_git_tree(root_entries)?;
        self.write_capture_object("tree", &tree, written, object_limit)
    }

    fn git_command(&self) -> Command {
        self.private_ref_owner.git_command()
    }

    fn create_protected_ref(&self, reference: &str, oid: &NativeOid) -> Result<(), GraphError> {
        let zero = "0".repeat(self.algorithm.width() * 2);
        self.git_output(&["update-ref", reference, &oid.to_hex(), &zero])?;
        self.sync_git_ref(reference)
    }

    fn promote_protected_ref(
        &self,
        provisional: &str,
        protected: &str,
        oid: &NativeOid,
    ) -> Result<(), GraphError> {
        let zero = "0".repeat(self.algorithm.width() * 2);
        let commands = format!(
            "start\nupdate {protected} {} {zero}\ndelete {provisional} {}\nprepare\ncommit\n",
            oid.to_hex(),
            oid.to_hex()
        );
        let mut child = self
            .git_command()
            .args(["update-ref", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| GraphError::GitCommandFailed)?;
        child
            .stdin
            .take()
            .ok_or(GraphError::GitCommandFailed)?
            .write_all(commands.as_bytes())
            .map_err(|_| GraphError::GitCommandFailed)?;
        let output = child
            .wait_with_output()
            .map_err(|_| GraphError::GitCommandFailed)?;
        if !output.status.success() {
            return Err(GraphError::GitCommandFailed);
        }
        Ok(())
    }

    fn delete_protected_ref(&self, reference: &str, oid: &NativeOid) -> Result<(), GraphError> {
        self.private_ref_owner
            .delete_protected_ref(reference, oid)
    }

    fn sync_git_ref(&self, reference: &str) -> Result<(), GraphError> {
        let output = self.git_output(&["rev-parse", "--git-path", reference])?;
        let path = resolve_git_path(self.repository.worktree(), &output)?;
        let metadata = fs::symlink_metadata(&path).map_err(|_| GraphError::DurabilityFailed)?;
        if !metadata.file_type().is_file() {
            return Err(GraphError::DurabilityFailed);
        }
        File::open(&path)
            .and_then(|file| file.sync_all())
            .map_err(|_| GraphError::DurabilityFailed)?;
        self.sync_ref_directories(reference)
    }

    fn sync_ref_directories(&self, reference: &str) -> Result<(), GraphError> {
        self.private_ref_owner.sync_ref_directories(reference)
    }

    fn sync_object_closure(
        &self,
        objects: &BTreeSet<NativeOid>,
        scope: &RepositoryReadScope,
    ) -> Result<(), GraphError> {
        let objects_output = self.git_output(&["rev-parse", "--git-path", "objects"])?;
        let objects_dir = resolve_git_path(self.repository.worktree(), &objects_output)?;
        sync_object_files(&objects_dir, objects, || scope.check_cancelled())
    }
}

impl Drop for NativeGraphContext {
    fn drop(&mut self) {
        self.cancel_reads();
    }
}

/// Read authority minted by the repository owner for one graph context.
/// Clones share cancellation and cumulative byte quota, so concurrent resolver
/// reads cannot evade the owner's limits.
#[derive(Clone, Debug)]
pub struct RepositoryReadScope {
    context: ContextIdentity,
    owner_id: [u8; 16],
    cancelled: Arc<AtomicBool>,
    bytes_used: Arc<AtomicU64>,
    structured_metadata_bytes_read: Arc<AtomicU64>,
    payload_bytes_written: Arc<AtomicU64>,
    objects_used: Arc<AtomicU64>,
    metadata_used: Arc<AtomicU64>,
    byte_quota: u64,
    max_single_read: u64,
    max_objects: u64,
    metadata_quota: u64,
}

impl RepositoryReadScope {
    fn authorize(&self, context: &NativeGraphContext) -> Result<(), GraphError> {
        if self.context != context.identity || self.owner_id != context.owner_id {
            return Err(GraphError::ContextMismatch);
        }
        self.check_cancelled()
    }

    fn check_cancelled(&self) -> Result<(), GraphError> {
        if self.cancelled.load(AtomicOrdering::Acquire) {
            Err(GraphError::ReadCancelled)
        } else {
            Ok(())
        }
    }

    fn reserve_payload(&self, bytes: u64) -> Result<PayloadReservation, GraphError> {
        self.check_cancelled()?;
        let mut used = self.bytes_used.load(AtomicOrdering::Relaxed);
        loop {
            let next = used
                .checked_add(bytes)
                .ok_or(GraphError::ReadQuotaExceeded)?;
            if next > self.byte_quota {
                return Err(GraphError::ReadQuotaExceeded);
            }
            match self.bytes_used.compare_exchange_weak(
                used,
                next,
                AtomicOrdering::AcqRel,
                AtomicOrdering::Relaxed,
            ) {
                Ok(_) => {
                    return Ok(PayloadReservation {
                        used: Arc::clone(&self.bytes_used),
                        reserved: bytes,
                        settled: false,
                    });
                }
                Err(actual) => used = actual,
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn objects_read_for_test(&self) -> u64 {
        self.objects_used.load(AtomicOrdering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn payload_bytes_read_for_test(&self) -> u64 {
        self.bytes_used.load(AtomicOrdering::Acquire)
    }

    /// Cumulative OVB-2 media payload bytes this scope has charged to its
    /// owner.
    ///
    /// Every payload byte that crosses the graph boundary is reserved here
    /// before it is returned, so this counter is the reader-seam measurement
    /// of "did this operation fetch media bytes". Structured metadata, index
    /// nodes and descriptors are charged as objects, never as payload, so a
    /// metadata-only query reports zero while still having read the graph.
    pub fn payload_bytes_read(&self) -> u64 {
        self.bytes_used.load(AtomicOrdering::Acquire)
    }
    /// Cumulative bytes read from canonical inline/overflow ORP field tuples
    /// and full structured descriptors. These are row/schema metadata, not
    /// OVB-2 media payload.
    pub fn structured_metadata_bytes_read(&self) -> u64 {
        self.structured_metadata_bytes_read
            .load(AtomicOrdering::Acquire)
    }

    fn charge_structured_metadata_read(&self, bytes: u64) -> Result<(), GraphError> {
        self.structured_metadata_bytes_read
            .fetch_update(
                AtomicOrdering::AcqRel,
                AtomicOrdering::Acquire,
                |used| used.checked_add(bytes),
            )
            .map(|_| ())
            .map_err(|_| GraphError::ReadQuotaExceeded)
    }

    /// Cumulative native Git objects this scope has charged to its owner,
    /// including row pages, index nodes, descriptors and any payload chunks.
    pub fn objects_read(&self) -> u64 {
        self.objects_used.load(AtomicOrdering::Acquire)
    }

    /// Cumulative OVB-2 media payload bytes this scope has written to the
    /// graph.
    ///
    /// Only the payload chunks of an OGB-2 closure are charged, never the byte
    /// index nodes, the descriptor or the tree objects around them. A
    /// metadata-only update therefore writes zero here however many structural
    /// objects it commits, and this is the write-seam counterpart of
    /// [`Self::payload_bytes_read`].
    pub fn payload_bytes_written(&self) -> u64 {
        self.payload_bytes_written.load(AtomicOrdering::Acquire)
    }

    fn charge_payload_written(&self, bytes: u64) {
        self.payload_bytes_written
            .fetch_add(bytes, AtomicOrdering::AcqRel);
    }
}

struct PayloadReservation {
    used: Arc<AtomicU64>,
    reserved: u64,
    settled: bool,
}

impl PayloadReservation {
    fn commit(mut self, actual: u64) {
        if actual < self.reserved {
            self.used
                .fetch_sub(self.reserved - actual, AtomicOrdering::AcqRel);
        }
        self.settled = true;
    }
}

impl Drop for PayloadReservation {
    fn drop(&mut self) {
        if !self.settled {
            self.used.fetch_sub(self.reserved, AtomicOrdering::AcqRel);
        }
    }
}

struct ObjectBudget {
    limit: u64,
    scope_limit: u64,
    edges: u64,
    oids: BTreeSet<NativeOid>,
    objects_used: Arc<AtomicU64>,
    metadata_limit: u64,
    metadata_used: Arc<AtomicU64>,
}

impl ObjectBudget {
    fn new(
        limit: u64,
        scope_limit: u64,
        objects_used: Arc<AtomicU64>,
        metadata_limit: u64,
        metadata_used: Arc<AtomicU64>,
    ) -> Self {
        Self {
            limit,
            scope_limit,
            edges: 0,
            oids: BTreeSet::new(),
            objects_used,
            metadata_limit,
            metadata_used,
        }
    }

    fn visit(&mut self, oid: &NativeOid) -> Result<(), GraphError> {
        if !self.oids.contains(oid) {
            if self.oids.len() as u64 >= self.limit {
                return Err(GraphError::InventoryQuotaExceeded);
            }
            reserve_atomic(&self.objects_used, 1, self.scope_limit)
                .map_err(|_| GraphError::InventoryQuotaExceeded)?;
            self.oids.insert(oid.clone());
        }
        Ok(())
    }

    fn record_metadata(&self, bytes: u64) -> Result<(), GraphError> {
        reserve_atomic(&self.metadata_used, bytes, self.metadata_limit)
            .map_err(|_| GraphError::MetadataQuotaExceeded)
    }

    fn edge(&mut self) -> Result<(), GraphError> {
        self.edges = self
            .edges
            .checked_add(1)
            .ok_or(GraphError::InventoryQuotaExceeded)?;
        if self.edges > MAX_VERIFY_EDGES {
            return Err(GraphError::InventoryQuotaExceeded);
        }
        Ok(())
    }
}

fn reserve_atomic(counter: &AtomicU64, amount: u64, limit: u64) -> Result<(), ()> {
    let mut current = counter.load(AtomicOrdering::Relaxed);
    loop {
        let next = current.checked_add(amount).ok_or(())?;
        if next > limit {
            return Err(());
        }
        match counter.compare_exchange_weak(
            current,
            next,
            AtomicOrdering::AcqRel,
            AtomicOrdering::Relaxed,
        ) {
            Ok(_) => return Ok(()),
            Err(observed) => current = observed,
        }
    }
}

#[derive(Clone, Debug)]
struct ParsedGitTreeEntry {
    mode: Vec<u8>,
    name: Vec<u8>,
    oid: NativeOid,
}

struct GearCutState {
    table: [u64; 256],
    accumulator: u64,
    length: usize,
}

impl GearCutState {
    fn new() -> Self {
        let mut table = [0u64; 256];
        for (byte, slot) in table.iter_mut().enumerate() {
            let mut hasher = Sha256::new();
            hasher.update(crate::blob_store::GEAR_SEED);
            hasher.update([byte as u8]);
            *slot = u64::from_be_bytes(hasher.finalize()[..8].try_into().unwrap());
        }
        Self {
            table,
            accumulator: 0,
            length: 0,
        }
    }

    fn push(&mut self, byte: u8) -> bool {
        self.accumulator = self
            .accumulator
            .wrapping_shl(1)
            .wrapping_add(self.table[byte as usize]);
        self.length += 1;
        if self.length >= crate::blob_store::GEAR_MINIMUM
            && (self.accumulator & crate::blob_store::GEAR_MASK == 0
                || self.length == crate::blob_store::GEAR_MAXIMUM)
        {
            self.accumulator = 0;
            self.length = 0;
            true
        } else {
            false
        }
    }
}

struct GearBoundaryVerifier {
    state: GearCutState,
    chunks_seen: bool,
}

impl GearBoundaryVerifier {
    fn new() -> Self {
        Self {
            state: GearCutState::new(),
            chunks_seen: false,
        }
    }

    fn push_chunk(&mut self, chunk: &[u8]) -> Result<(), GraphError> {
        if chunk.is_empty() || (self.chunks_seen && self.state.length != 0) {
            return Err(GraphError::InvalidByteIndex);
        }
        for (index, byte) in chunk.iter().copied().enumerate() {
            if self.state.push(byte) && index + 1 != chunk.len() {
                return Err(GraphError::InvalidByteIndex);
            }
        }
        self.chunks_seen = true;
        Ok(())
    }

}

struct StreamingGearChunker {
    state: GearCutState,
    chunk: Vec<u8>,
}

impl StreamingGearChunker {
    fn new() -> Self {
        Self {
            state: GearCutState::new(),
            chunk: Vec::with_capacity(crate::blob_store::GEAR_MAXIMUM),
        }
    }

    fn push(&mut self, byte: u8) -> Option<Vec<u8>> {
        self.chunk.push(byte);
        if self.state.push(byte) {
            Some(std::mem::replace(
                &mut self.chunk,
                Vec::with_capacity(crate::blob_store::GEAR_MAXIMUM),
            ))
        } else {
            None
        }
    }

    fn finish(self) -> Option<Vec<u8>> {
        (!self.chunk.is_empty()).then_some(self.chunk)
    }
}

pub(crate) struct CaptureIndexBuilder<'a> {
    graph: &'a NativeGraphContext,
    written: &'a mut BTreeSet<NativeOid>,
    object_limit: u64,
    scope: &'a RepositoryReadScope,
    levels: Vec<Vec<ByteIndexEntry>>,
}

impl<'a> CaptureIndexBuilder<'a> {
    pub(crate) fn new(
        graph: &'a NativeGraphContext,
        written: &'a mut BTreeSet<NativeOid>,
        object_limit: u64,
        scope: &'a RepositoryReadScope,
    ) -> Self {
        Self {
            graph,
            written,
            object_limit,
            scope,
            levels: vec![Vec::new()],
        }
    }

    pub(crate) fn push_chunk(
        &mut self,
        length: u64,
        target: NativeOid,
        chunk_sha256: [u8; 32],
    ) -> Result<(), GraphError> {
        self.push_entry(
            0,
            ByteIndexEntry {
                span_length: length,
                target,
                chunk_sha256: Some(chunk_sha256),
            },
        )
    }

    fn push_chunk_bytes(&mut self, chunk: Vec<u8>) -> Result<(), GraphError> {
        let length = chunk.len() as u64;
        let chunk_sha256 = crate::blob_store::digest_bytes(&chunk).sha256();
        let target = self.graph.write_capture_object(
            "blob",
            &chunk,
            self.written,
            self.object_limit,
        )?;
        // The write-seam measurement: only an actual payload chunk is charged,
        // so a metadata-only commit through this scope reports zero.
        self.scope.charge_payload_written(length);
        self.push_chunk(length, target, chunk_sha256)
    }

    fn push_entry(&mut self, level: usize, entry: ByteIndexEntry) -> Result<(), GraphError> {
        self.scope.check_cancelled()?;
        if level > MAX_GRAPH_HEIGHT as usize {
            return Err(GraphError::HeightExceeded(level as u8));
        }
        if self.levels.len() <= level {
            self.levels.resize_with(level + 1, Vec::new);
        }
        let entries = &mut self.levels[level];
        entries.push(entry);
        let anchor = entries
            .last()
            .ok_or(GraphError::InvalidByteIndex)
            .and_then(|entry| byte_index_anchor(level as u32, entry))?;
        if entries.len() == MAX_REFS || (entries.len() >= 16 && anchor) {
            self.flush_level(level)?;
        }
        Ok(())
    }

    fn flush_level(&mut self, level: usize) -> Result<(), GraphError> {
        self.scope.check_cancelled()?;
        let entries = std::mem::take(
            self.levels
                .get_mut(level)
                .ok_or(GraphError::InvalidByteIndex)?,
        );
        if entries.is_empty() {
            return Ok(());
        }
        let total_length = entries.iter().try_fold(0u64, |total, entry| {
            total
                .checked_add(entry.span_length)
                .ok_or(GraphError::InvalidByteIndex)
        })?;
        let oid = self.graph.write_capture_node(
            &NodeData::ByteIndex {
                height: level as u8,
                total_length,
                entries,
            },
            self.written,
            self.object_limit,
        )?;
        self.push_entry(
            level + 1,
            ByteIndexEntry {
                span_length: total_length,
                target: oid,
                chunk_sha256: None,
            },
        )
    }

    pub(crate) fn finish(mut self) -> Result<Option<NativeOid>, GraphError> {
        loop {
            let Some(level) = self.levels.iter().position(|entries| !entries.is_empty()) else {
                return Ok(None);
            };
            let only_top_entry = level > 0
                && self.levels[level].len() == 1
                && self.levels.iter().skip(level + 1).all(Vec::is_empty);
            if only_top_entry {
                return Ok(self.levels[level].first().map(|entry| entry.target.clone()));
            }
            self.flush_level(level)?;
        }
    }
}

fn byte_index_anchor(level: u32, entry: &ByteIndexEntry) -> Result<bool, GraphError> {
    let mut token = Vec::new();
    if level == 0 {
        array(&mut token, 2);
        uint(&mut token, entry.span_length);
        bytes(
            &mut token,
            entry
                .chunk_sha256
                .as_ref()
                .ok_or(GraphError::InvalidByteIndex)?,
        );
    } else {
        if entry.chunk_sha256.is_some() {
            return Err(GraphError::InvalidByteIndex);
        }
        token.extend_from_slice(entry.target.as_bytes());
    }
    let mut hasher = Sha256::new();
    hasher.update(b"orna.ogb.index.v2\0");
    hasher.update(level.to_be_bytes());
    hasher.update(token);
    let digest = hasher.finalize();
    Ok(digest[31] & 0x3f == 0)
}

fn encode_git_tree(mut entries: Vec<ParsedGitTreeEntry>) -> Result<Vec<u8>, GraphError> {
    entries.sort_by(git_tree_cmp);
    if entries.windows(2).any(|pair| {
        git_tree_cmp(&pair[0], &pair[1]) != std::cmp::Ordering::Less
    }) {
        return Err(GraphError::InvalidTreeEnvelope);
    }
    let mut output = Vec::new();
    for entry in entries {
        if !matches!(entry.mode.as_slice(), b"100644" | b"40000")
            || entry.name.is_empty()
            || entry.name.contains(&0)
            || entry.name.contains(&b'/')
        {
            return Err(GraphError::InvalidTreeEnvelope);
        }
        output.extend_from_slice(&entry.mode);
        output.push(b' ');
        output.extend_from_slice(&entry.name);
        output.push(0);
        output.extend_from_slice(entry.oid.as_bytes());
    }
    Ok(output)
}

const MAX_VERIFY_EDGES: u64 = 4_000_000;

fn parse_git_tree(
    bytes: &[u8],
    algorithm: GitHashAlgorithm,
    maximum_entries: usize,
) -> Result<Vec<ParsedGitTreeEntry>, GraphError> {
    let mut cursor = 0usize;
    let mut entries = Vec::new();
    while cursor < bytes.len() {
        if entries.len() == maximum_entries {
            return Err(GraphError::FanoutExceeded(entries.len() + 1));
        }
        let mode_end = bytes[cursor..]
            .iter()
            .position(|byte| *byte == b' ')
            .map(|offset| cursor + offset)
            .ok_or(GraphError::InvalidTreeEnvelope)?;
        let mode = bytes[cursor..mode_end].to_vec();
        cursor = mode_end + 1;
        let name_end = bytes[cursor..]
            .iter()
            .position(|byte| *byte == 0)
            .map(|offset| cursor + offset)
            .ok_or(GraphError::InvalidTreeEnvelope)?;
        if name_end == cursor {
            return Err(GraphError::InvalidTreeEnvelope);
        }
        let name = bytes[cursor..name_end].to_vec();
        cursor = name_end + 1;
        let oid_end = cursor
            .checked_add(algorithm.width())
            .ok_or(GraphError::InvalidTreeEnvelope)?;
        let oid = NativeOid::new(
            algorithm,
            bytes
                .get(cursor..oid_end)
                .ok_or(GraphError::InvalidTreeEnvelope)?,
        )?;
        cursor = oid_end;
        entries.push(ParsedGitTreeEntry { mode, name, oid });
    }
    if entries
        .windows(2)
        .any(|pair| git_tree_cmp(&pair[0], &pair[1]) != std::cmp::Ordering::Less)
    {
        return Err(GraphError::InvalidTreeEnvelope);
    }
    Ok(entries)
}

fn git_tree_cmp(left: &ParsedGitTreeEntry, right: &ParsedGitTreeEntry) -> std::cmp::Ordering {
    let left_directory = left.mode.as_slice() == b"40000";
    let right_directory = right.mode.as_slice() == b"40000";
    let mut left_name = left.name.as_slice();
    let mut right_name = right.name.as_slice();
    let left_suffix = if left_directory { b'/' } else { 0 };
    let right_suffix = if right_directory { b'/' } else { 0 };
    let common = left_name.len().min(right_name.len());
    let order = left_name[..common].cmp(&right_name[..common]);
    if order != std::cmp::Ordering::Equal {
        return order;
    }
    left_name = &left_name[common..];
    right_name = &right_name[common..];
    match (left_name.first(), right_name.first()) {
        (Some(left), Some(right)) => left.cmp(right),
        (Some(left), None) => left.cmp(&right_suffix),
        (None, Some(right)) => left_suffix.cmp(right),
        (None, None) => std::cmp::Ordering::Equal,
    }
}

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

/// The inverse of [`hex_encode`]: exactly its lowercase, even-length
/// spelling, so every byte string has one accepted form.
pub(crate) fn hex_decode(text: &str) -> Option<Vec<u8>> {
    let digit = |byte: u8| match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    };
    let (pairs, []) = text.as_bytes().as_chunks::<2>() else {
        return None;
    };
    pairs
        .iter()
        .map(|[high, low]| Some((digit(*high)? << 4) | digit(*low)?))
        .collect()
}

fn scratch_content_ref(owner_id: &[u8; 16], pin_id: &[u8; 16]) -> String {
    format!(
        "refs/orna/pins/{}/scratch/{}",
        hex_encode(owner_id),
        hex_encode(pin_id)
    )
}

fn accepted_content_ref(owner_id: &[u8; 16], pin_id: &[u8; 16]) -> String {
    format!(
        "refs/orna/pins/{}/pending/{}",
        hex_encode(owner_id),
        hex_encode(pin_id)
    )
}

/// Native hash algorithm for one hexadecimal object id, by its exact width.
fn algorithm_for_hex(hex: &str) -> Option<GitHashAlgorithm> {
    match hex.len() {
        40 => Some(GitHashAlgorithm::Sha1),
        64 => Some(GitHashAlgorithm::Sha256),
        _ => None,
    }
}

/// RAII handle for the same coordination lock used by repository publishers.
/// Reconciliation must hold this lock from the journal check through the last
/// private-ref deletion, otherwise a publisher can journal a root between them.
struct NativeGraphCoordinationLock {
    _file: File,
}

impl NativeGraphCoordinationLock {
    fn acquire(repository: &crate::Repository) -> Result<Self, GraphError> {
        let locks = repository.runtime_paths().locks();
        fs::create_dir_all(&locks).map_err(|_| GraphError::DurabilityFailed)?;
        let file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(locks.join("coordination.lock"))
            .map_err(|_| GraphError::DurabilityFailed)?;
        file.try_lock_exclusive()
            .map_err(|_| GraphError::PublicationNotTerminal)?;
        Ok(Self { _file: file })
    }
}
impl Drop for NativeGraphCoordinationLock {
    fn drop(&mut self) {
        let _ = self._file.unlock();
    }
}

/// Reclaims a departed runtime owner's private refs after durable runtime
/// state has been recovered. Accepted pending pins are retained only when the
/// recovered manifest names both their pin IDs and descriptor OIDs.
///
/// Capture, promotion and row-candidate preparation root OGB-2 closures under
/// `refs/orna/pins/<owner>/...`. A killed owner never runs drop-time cleanup.
/// Recovery must therefore reconcile those refs against durable accepted-pin
/// evidence before deleting anything; an absent in-memory handle is not proof
/// that a pending pin is orphaned.
///
/// Only refs under the departed owner's namespace are considered. Every
/// retained pending ref must still resolve to its manifest's descriptor OID;
/// other refs are deleted only after their native object IDs are resolved.
pub(crate) fn reclaim_dead_owner_pin_refs(
    repository: &crate::Repository,
    dead_owner: [u8; 16],
    accepted_pending: &BTreeMap<[u8; 16], NativeOid>,
) -> Result<usize, GraphError> {
    reclaim_departed_owner_pin_refs(repository, &[(dead_owner, accepted_pending)])
}

/// Reclaims every listed departed owner in one pass under one coordination
/// lock. The journal check and each owner's deletions all run while that lock
/// is held, so a publisher cannot journal a root between owners. A pass that
/// cannot take the lock deletes nothing.
pub(crate) fn reclaim_departed_owner_pin_refs(
    repository: &crate::Repository,
    departed: &[([u8; 16], &BTreeMap<[u8; 16], NativeOid>)],
) -> Result<usize, GraphError> {
    let _coordination_lock = NativeGraphCoordinationLock::acquire(repository)?;
    if repository
        .read_publication_journal_locked()
        .map_err(|_| GraphError::DurabilityFailed)?
        .is_some()
    {
        return Err(GraphError::PublicationNotTerminal);
    }
    let owner = PrivateRefCleanupOwner {
        repository: repository.clone(),
    };
    let mut reclaimed = 0;
    for (dead_owner, accepted_pending) in departed {
        reclaimed += reclaim_owner_pin_refs_locked(&owner, *dead_owner, accepted_pending)?;
    }
    Ok(reclaimed)
}

/// Deletes one departed owner's unaccepted private pin refs. The caller must
/// hold the coordination lock and have confirmed that no publication journal
/// is pending.
fn reclaim_owner_pin_refs_locked(
    owner: &PrivateRefCleanupOwner,
    dead_owner: [u8; 16],
    accepted_pending: &BTreeMap<[u8; 16], NativeOid>,
) -> Result<usize, GraphError> {
    let prefix = format!("refs/orna/pins/{}/", hex_encode(&dead_owner));
    let listing = owner.git_output(&["for-each-ref", "--format=%(refname)", &prefix])?;
    let listing = String::from_utf8(listing).map_err(|_| GraphError::MalformedReferenceName)?;

    let accepted_refs = accepted_pending
        .iter()
        .map(|(pin_id, oid)| (accepted_content_ref(&dead_owner, pin_id), oid))
        .collect::<BTreeMap<_, _>>();
    for (reference, expected_oid) in &accepted_refs {
        let resolved = owner
            .git_output(&["rev-parse", "--verify", reference])
            .map_err(|_| GraphError::InvalidProtectedRef)?;
        let resolved = String::from_utf8(resolved).map_err(|_| GraphError::MalformedReferenceName)?;
        let resolved = resolved.trim();
        let algorithm = algorithm_for_hex(resolved).ok_or(GraphError::MalformedReferenceName)?;
        let oid = NativeOid::from_hex(algorithm, resolved)?;
        if &oid != *expected_oid {
            return Err(GraphError::InvalidProtectedRef);
        }
    }

    let mut reclaimed = 0;
    for name in listing.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if !name.starts_with(&prefix) {
            continue;
        }
        if accepted_refs.contains_key(name) {
            continue;
        }
        let resolved = owner.git_output(&["rev-parse", "--verify", name])?;
        let resolved = String::from_utf8(resolved).map_err(|_| GraphError::MalformedReferenceName)?;
        let resolved = resolved.trim();
        let algorithm = algorithm_for_hex(resolved).ok_or(GraphError::MalformedReferenceName)?;
        let oid = NativeOid::from_hex(algorithm, resolved)?;
        owner.delete_protected_ref(name, &oid)?;
        reclaimed += 1;
    }
    Ok(reclaimed)
}

fn mount_snapshot_pin_ref(
    registry_owner_id: &[u8; 16],
    owner_id: &[u8; 16],
    view_id: &[u8; 32],
) -> String {
    format!(
        "refs/orna/pins/{}/mounts/{}/{}",
        hex_encode(registry_owner_id),
        hex_encode(owner_id),
        hex_encode(view_id)
    )
}

pub(crate) struct MountSnapshotPin {
    pub(crate) owner_id: [u8; 16],
    pub(crate) view_id: [u8; 32],
    pub(crate) oid_hex: String,
}

pub(crate) fn list_mount_snapshot_pins(
    repository: &crate::Repository,
    registry_owner_id: &[u8; 16],
) -> Result<Vec<MountSnapshotPin>, GraphError> {
    let owner = PrivateRefCleanupOwner {
        repository: repository.clone(),
    };
    let prefix = format!(
        "refs/orna/pins/{}/mounts/",
        hex_encode(registry_owner_id)
    );
    let listing = owner.git_output(&[
        "for-each-ref",
        "--format=%(refname) %(objectname)",
        &prefix,
    ])?;
    let listing = String::from_utf8(listing).map_err(|_| GraphError::MalformedReferenceName)?;
    let mut pins = Vec::new();
    for line in listing.lines() {
        let (reference, oid_hex) = line
            .split_once(' ')
            .ok_or(GraphError::MalformedReferenceName)?;
        let Some(suffix) = reference.strip_prefix(&prefix) else {
            continue;
        };
        let Some((owner_hex, view_hex)) = suffix.split_once('/') else {
            return Err(GraphError::MalformedReferenceName);
        };
        if view_hex.contains('/') {
            return Err(GraphError::MalformedReferenceName);
        }
        let owner_id = decode_mount_pin_component::<16>(owner_hex)?;
        let view_id = decode_mount_pin_component::<32>(view_hex)?;
        let algorithm = algorithm_for_hex(oid_hex).ok_or(GraphError::MalformedReferenceName)?;
        let oid = NativeOid::from_hex(algorithm, oid_hex)?;
        pins.push(MountSnapshotPin {
            owner_id,
            view_id,
            oid_hex: oid.to_hex(),
        });
    }
    Ok(pins)
}

pub(crate) fn mount_snapshot_pin_oid(
    repository: &crate::Repository,
    registry_owner_id: &[u8; 16],
    owner_id: &[u8; 16],
    view_id: &[u8; 32],
) -> Result<Option<String>, GraphError> {
    let owner = PrivateRefCleanupOwner {
        repository: repository.clone(),
    };
    let reference = mount_snapshot_pin_ref(registry_owner_id, owner_id, view_id);
    let listing = owner.git_output(&[
        "for-each-ref",
        "--format=%(refname) %(objectname)",
        &reference,
    ])?;
    let listing = String::from_utf8(listing).map_err(|_| GraphError::MalformedReferenceName)?;
    let Some(oid_hex) = listing.lines().find_map(|line| {
        let (name, value) = line.split_once(' ')?;
        (name == reference).then_some(value)
    }) else {
        return Ok(None);
    };
    let algorithm = algorithm_for_hex(oid_hex).ok_or(GraphError::MalformedReferenceName)?;
    let oid = NativeOid::from_hex(algorithm, oid_hex)?;
    Ok(Some(oid.to_hex()))
}

fn decode_mount_pin_component<const N: usize>(value: &str) -> Result<[u8; N], GraphError> {
    if value.len() != N * 2 {
        return Err(GraphError::MalformedReferenceName);
    }
    let mut bytes = [0; N];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| GraphError::MalformedReferenceName)?;
    }
    Ok(bytes)
}

pub(crate) fn create_mount_snapshot_pin(
    repository: &crate::Repository,
    registry_owner_id: &[u8; 16],
    owner_id: &[u8; 16],
    view_id: &[u8; 32],
    commit_oid_hex: &str,
) -> Result<String, GraphError> {
    let algorithm = algorithm_for_hex(commit_oid_hex).ok_or(GraphError::MalformedOid)?;
    let oid = NativeOid::from_hex(algorithm, commit_oid_hex)?;
    let reference = mount_snapshot_pin_ref(registry_owner_id, owner_id, view_id);
    let owner = PrivateRefCleanupOwner {
        repository: repository.clone(),
    };
    owner.create_protected_ref(&reference, &oid)?;
    Ok(oid.to_hex())
}

pub(crate) fn verify_mount_snapshot_pin(
    repository: &crate::Repository,
    registry_owner_id: &[u8; 16],
    owner_id: &[u8; 16],
    view_id: &[u8; 32],
    commit_oid_hex: &str,
) -> Result<(), GraphError> {
    let algorithm = algorithm_for_hex(commit_oid_hex).ok_or(GraphError::MalformedOid)?;
    let expected = NativeOid::from_hex(algorithm, commit_oid_hex)?;
    let actual_hex = mount_snapshot_pin_oid(repository, registry_owner_id, owner_id, view_id)?
        .ok_or(GraphError::InvalidProtectedRef)?;
    let algorithm = algorithm_for_hex(&actual_hex).ok_or(GraphError::MalformedReferenceName)?;
    let actual = NativeOid::from_hex(algorithm, &actual_hex)?;
    if actual != expected {
        return Err(GraphError::InvalidProtectedRef);
    }
    Ok(())
}

pub(crate) fn delete_mount_snapshot_pin(
    repository: &crate::Repository,
    registry_owner_id: &[u8; 16],
    owner_id: &[u8; 16],
    view_id: &[u8; 32],
    commit_oid_hex: &str,
) -> Result<(), GraphError> {
    let algorithm = algorithm_for_hex(commit_oid_hex).ok_or(GraphError::MalformedOid)?;
    let oid = NativeOid::from_hex(algorithm, commit_oid_hex)?;
    let Some(actual) = mount_snapshot_pin_oid(repository, registry_owner_id, owner_id, view_id)? else {
        return Ok(());
    };
    if actual != oid.to_hex() {
        return Err(GraphError::InvalidProtectedRef);
    }
    let owner = PrivateRefCleanupOwner {
        repository: repository.clone(),
    };
    owner.delete_protected_ref(
        &mount_snapshot_pin_ref(registry_owner_id, owner_id, view_id),
        &oid,
    )
}
fn resolve_git_path(worktree: &Path, output: &[u8]) -> Result<PathBuf, GraphError> {
    let value = std::str::from_utf8(output)
        .map_err(|_| GraphError::GitObjectMalformed)?
        .trim();
    let path = PathBuf::from(value);
    Ok(if path.is_absolute() {
        path
    } else {
        worktree.join(path)
    })
}

pub(crate) fn sync_directory(path: &Path) -> Result<(), GraphError> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|_| GraphError::DurabilityFailed)
}

/// Persists the named objects before a ref may name them: each loose object
/// file and its shard directory, the whole pack set when an object is not
/// loose, then the object directory. An object that is not loose may live in
/// another repository's store, so a non-empty `objects/info/alternates` is
/// then refused rather than half-synced. `before_each` runs ahead of every
/// object.
pub(crate) fn sync_object_files<'a>(
    objects_dir: &Path,
    objects: impl IntoIterator<Item = &'a NativeOid>,
    mut before_each: impl FnMut() -> Result<(), GraphError>,
) -> Result<(), GraphError> {
    let mut needs_pack_sync = false;
    for oid in objects {
        before_each()?;
        let hex = oid.to_hex();
        let loose = objects_dir.join(&hex[..2]).join(&hex[2..]);
        match fs::symlink_metadata(&loose) {
            Ok(metadata) => {
                if !metadata.file_type().is_file() {
                    return Err(GraphError::DurabilityFailed);
                }
                File::open(&loose)
                    .and_then(|file| file.sync_all())
                    .map_err(|_| GraphError::DurabilityFailed)?;
                sync_directory(loose.parent().ok_or(GraphError::DurabilityFailed)?)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                needs_pack_sync = true;
            }
            Err(_) => return Err(GraphError::DurabilityFailed),
        }
    }
    if needs_pack_sync {
        let alternates = objects_dir.join("info").join("alternates");
        match fs::symlink_metadata(&alternates) {
            Ok(metadata) => {
                if !metadata.file_type().is_file() || metadata.len() != 0 {
                    return Err(GraphError::ObjectDurabilityUnavailable);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(GraphError::ObjectDurabilityUnavailable),
        }
        sync_all_pack_files(&objects_dir.join("pack"))?;
    }
    sync_directory(objects_dir)
}

/// Refuses a ref backend whose refs cannot be flushed as loose files or
/// through `packed-refs`. Only Git's `files` backend stores refs that way.
pub(crate) fn require_files_ref_backend(repository: &crate::Repository) -> Result<(), GraphError> {
    let owner = PrivateRefCleanupOwner {
        repository: repository.clone(),
    };
    let format = owner.git_output(&["rev-parse", "--show-ref-format"])?;
    if format.trim_ascii() == b"files" {
        Ok(())
    } else {
        Err(GraphError::DurabilityFailed)
    }
}

/// Persists `reference` after a ref transaction wrote it: the loose ref
/// file, each directory from it up to the common Git directory, and
/// `packed-refs`. Under the `files` backend, a ref that `git pack-refs` has
/// since moved has no loose file and is persisted through `packed-refs`, so
/// recovery after a `git gc` is not refused. Any other missing ref file is
/// refused.
pub(crate) fn sync_git_ref_durably(
    repository: &crate::Repository,
    reference: &str,
) -> Result<(), GraphError> {
    let owner = PrivateRefCleanupOwner {
        repository: repository.clone(),
    };
    let output = owner.git_output(&["rev-parse", "--git-path", reference])?;
    let path = resolve_git_path(repository.worktree(), &output)?;
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => File::open(&path)
            .and_then(|file| file.sync_all())
            .map_err(|_| GraphError::DurabilityFailed)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            require_files_ref_backend(repository)?;
            let packed = owner.git_output(&["rev-parse", "--git-path", "packed-refs"])?;
            let packed = resolve_git_path(repository.worktree(), &packed)?;
            if !fs::symlink_metadata(&packed).is_ok_and(|metadata| metadata.file_type().is_file()) {
                return Err(GraphError::DurabilityFailed);
            }
        }
        _ => return Err(GraphError::DurabilityFailed),
    }
    owner.sync_ref_directories(reference)
}

pub(crate) fn sync_all_pack_files(pack_dir: &Path) -> Result<(), GraphError> {
    let entries = fs::read_dir(pack_dir).map_err(|_| GraphError::ObjectDurabilityUnavailable)?;
    let mut indexes = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|_| GraphError::ObjectDurabilityUnavailable)?
            .path();
        if path.extension().is_some_and(|extension| extension == "idx") {
            indexes.push(path);
            if indexes.len() > MAX_PACK_INDEX_FILES {
                return Err(GraphError::ObjectDurabilityUnavailable);
            }
        }
    }
    if indexes.is_empty() {
        return Err(GraphError::ObjectDurabilityUnavailable);
    }
    let mut total_bytes = 0u64;
    for index in indexes {
        let pack = index.with_extension("pack");
        for path in [&index, &pack] {
            let metadata =
                fs::symlink_metadata(path).map_err(|_| GraphError::ObjectDurabilityUnavailable)?;
            total_bytes = total_bytes
                .checked_add(metadata.len())
                .ok_or(GraphError::ObjectDurabilityUnavailable)?;
            if !metadata.file_type().is_file()
                || metadata.len() > MAX_PACK_INDEX_SCAN_BYTES
                || total_bytes > MAX_PACK_SET_SYNC_BYTES
            {
                return Err(GraphError::ObjectDurabilityUnavailable);
            }
            File::open(path)
                .and_then(|file| file.sync_all())
                .map_err(|_| GraphError::DurabilityFailed)?;
        }
    }
    sync_directory(pack_dir)
}

/// Verified immutable descriptor evidence.  It is intentionally unconstructible
/// outside this module until a native Git closure verifier issues it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VerifiedBlobDescriptor {
    descriptor_oid: NativeOid,
    identity: crate::blob_store::ContentIdentity,
    byte_root: Option<NativeOid>,
}

/// One reachable commit and its root tree snapshot, as listed by
/// [`NativeGraphContext::list_revision_snapshots`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevisionSnapshot {
    commit: NativeOid,
    tree: NativeOid,
    author: String,
    committed_at: u64,
    migration: Option<String>,
}

impl RevisionSnapshot {
    pub const fn commit(&self) -> &NativeOid {
        &self.commit
    }

    pub const fn tree(&self) -> &NativeOid {
        &self.tree
    }

    /// Commit author as `Name <email>`, as git prints `%an <%ae>`.
    pub fn author(&self) -> &str {
        &self.author
    }

    /// The migration coordinate this commit records, when it is a journalled
    /// format-1/2 to format-3 migration commit rather than an ordinary one.
    pub fn migration(&self) -> Option<&str> {
        self.migration.as_deref()
    }

    /// When the commit was created, in seconds since the Unix epoch, as git
    /// prints `%ct`. The original commit keeps its own timestamp through a
    /// migration, so a history walk reports the author's time, not the time the
    /// migrated representation was established.
    pub const fn committed_at(&self) -> u64 {
        self.committed_at
    }
}

/// Splits one `git log` record's author-and-trailer field into the author and
/// the migration coordinate.
///
/// The author follows the first tab of the record and the trailer value follows
/// a second tab, so a commit that carries no marker renders an empty value that
/// is not the same as an absent one. An empty marker is therefore reported as
/// no migration rather than as an empty coordinate.
fn split_revision_record(record: &str) -> (&str, Option<String>) {
    match record.split_once('\t') {
        Some((author, marker)) if !marker.is_empty() => (author, Some(marker.to_owned())),
        Some((author, _)) => (author, None),
        None => (record, None),
    }
}

/// One reachable commit of a row's history, as listed by
/// [`NativeGraphContext::list_row_revisions`]. `present` is true when the
/// row's descriptor tree is reachable from the commit's root tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowRevision {
    commit: NativeOid,
    tree: NativeOid,
    author: String,
    committed_at: u64,
    migration: Option<String>,
    present: bool,
}

impl RowRevision {
    pub const fn commit(&self) -> &NativeOid {
        &self.commit
    }

    pub const fn tree(&self) -> &NativeOid {
        &self.tree
    }

    /// Commit author as `Name <email>`.
    pub fn author(&self) -> &str {
        &self.author
    }

    /// The migration coordinate this commit records, when it is the journalled
    /// format-1/2 to format-3 migration commit rather than an ordinary one.
    ///
    /// The coordinate is what lets a `history` walk distinguish the commit that
    /// established the migrated representation from the ordinary commits around
    /// it, whose older revisions predate the format-3 row store.
    pub fn migration(&self) -> Option<&str> {
        self.migration.as_deref()
    }

    /// When the commit was created, in seconds since the Unix epoch, as git
    /// prints `%ct`. Preserving the original author's time is what makes a
    /// migrated row's history read the same after the migration.
    pub const fn committed_at(&self) -> u64 {
        self.committed_at
    }

    pub const fn present(&self) -> bool {
        self.present
    }
}

/// One pending ordered-map mutation for [`NativeGraphContext::maintain_row_index`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RowIndexMutation {
    /// Removes one logical key. The pinned root must hold it.
    Delete {
        key: crate::row_store::TypedKey,
    },
    /// Moves one logical key and its stored value to a new logical key. The
    /// source must be present, the destination absent and the two distinct.
    Rekey {
        from: crate::row_store::TypedKey,
        to: crate::row_store::TypedKey,
    },
    /// Inserts one logical key with its canonical ROV-3 stored-field tuple.
    /// The key must be absent from the pinned root. The writer stores the
    /// tuple inline or lowers it, and any of its values, to the shared
    /// overflow graph (store-format-3 section 5).
    Insert {
        key: crate::row_store::TypedKey,
        fields: Vec<u8>,
    },
}

impl RowIndexMutation {
    /// Removes a key from its canonical encoded form. The key must be present
    /// in the pinned row map when the mutation is applied.
    pub fn delete_canonical(key: &[u8]) -> Result<Self, GraphError> {
        let key = crate::row_store::TypedKey::decode_canonical(key)
            .map_err(|_| GraphError::NonCanonicalData)?;
        Ok(Self::Delete { key })
    }

    /// Inserts a row from its canonical key and schema-independent field tuple.
    /// The key must be absent from the pinned row map when the mutation is
    /// applied. The tuple is a canonical CBOR array of at most 1 GiB that
    /// holds no overflow reference; how it is stored is the writer's choice.
    pub fn insert_canonical(key: &[u8], value: &[u8]) -> Result<Self, GraphError> {
        let key = crate::row_store::TypedKey::decode_canonical(key)
            .map_err(|_| GraphError::NonCanonicalData)?;
        if !matches!(decode_canonical_content(value)?, CborValue::Array(_)) {
            return Err(GraphError::NonCanonicalData);
        }
        Ok(Self::Insert {
            key,
            fields: value.to_vec(),
        })
    }
}
/// One explicit logical address change emitted by an ordered row-index apply.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogicalRowRekey {
    pub old_address: crate::row_store::TypedKey,
    pub new_address: crate::row_store::TypedKey,
}

/// The measured outcome of one ORP-1 index maintenance pass.
///
/// The object sets are disjoint by construction, so a caller can attribute new
/// tree objects to the index rewrite and prove unchanged media was reused
/// rather than rewritten: `index_objects_created` is the index churn the pass
/// actually caused, `index_objects_reused` names the previous root's nodes the
/// new root still reaches at the same object id, and `payload_objects_reused`
/// counts the stored value/Blob-descriptor objects the rewrite kept.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexMaintenance {
    previous_root: NativeOid,
    primary_root: NativeOid,
    row_count: u64,
    logical_rekeys: Vec<LogicalRowRekey>,
    objects_written: Vec<NativeOid>,
    index_objects_created: Vec<NativeOid>,
    index_objects_reused: Vec<NativeOid>,
    payload_objects_reused: Vec<NativeOid>,
}

impl IndexMaintenance {
    /// The pinned primary root this pass rewrote from.
    pub fn previous_root(&self) -> &NativeOid {
        &self.previous_root
    }

    /// The new primary root. It stays unreachable until a caller names it.
    pub fn primary_root(&self) -> &NativeOid {
        &self.primary_root
    }

    /// Logical rows the new root holds.
    pub const fn row_count(&self) -> u64 {
        self.row_count
    }

    /// Explicit logical row address changes produced by this apply, in mutation order.
    pub fn logical_rekeys(&self) -> &[LogicalRowRekey] {
        &self.logical_rekeys
    }

    /// Every object this pass wrote, for the caller’s durability barrier.
    pub fn objects_written(&self) -> &[NativeOid] {
        &self.objects_written
    }

    /// New index nodes: index churn, counted separately from media reuse.
    pub fn index_objects_created(&self) -> &[NativeOid] {
        &self.index_objects_created
    }

    /// Previous index nodes the new root still names unchanged.
    pub fn index_objects_reused(&self) -> &[NativeOid] {
        &self.index_objects_reused
    }

    /// Stored value/Blob-descriptor objects reused unchanged.
    pub fn payload_objects_reused(&self) -> &[NativeOid] {
        &self.payload_objects_reused
    }
}

struct RelationIndexCandidate {
    primary_root: NativeOid,
    row_count: u64,
    store_root: NativeOid,
}

/// One relation entry of a `.orna/store` relation map: the stable relation
/// UUID key and its `[schema_oid, primary_map_oid, secondary_map_or_null,
/// row_count]` value (store-format-3 §2).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoreRelationEntry {
    pub(crate) relation_id: [u8; 16],
    pub(crate) schema_oid: NativeOid,
    pub(crate) primary_root: NativeOid,
    pub(crate) secondary_root: Option<NativeOid>,
    pub(crate) row_count: u64,
}

/// Upper bound on commits one revision walk may list.
const MAX_REVISION_WALK: usize = 4096;

/// Opaque authority to one admitted descriptor, tied to one repository owner
/// and immutable snapshot.  It cannot be reconstructed from an OID or digest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmittedBlobReference {
    context: ContextIdentity,
    database_id: [u8; 16],
    owner_id: [u8; 16],
    snapshot_id: [u8; 32],
    descriptor_oid: NativeOid,
    identity: crate::blob_store::ContentIdentity,
}

impl AdmittedBlobReference {
    pub fn database_id(&self) -> &[u8; 16] {
        &self.database_id
    }

    pub fn snapshot_id(&self) -> &[u8; 32] {
        &self.snapshot_id
    }

    pub fn descriptor_oid(&self) -> &NativeOid {
        &self.descriptor_oid
    }

    pub const fn content_identity(&self) -> crate::blob_store::ContentIdentity {
        self.identity
    }

    pub(crate) fn matches_context(&self, context: &NativeGraphContext) -> bool {
        self.context == context.identity
            && self.database_id == context.database_id
            && self.owner_id == context.owner_id
            && self.snapshot_id == context.snapshot_id
    }
}

/// A Blob read out of a committed row: its canonical MIME-1 annotation and
/// the content capability that serves its bytes.
#[derive(Clone, Debug)]
pub struct AdmittedStoredBlob {
    annotation: MediaAnnotation,
    reference: AdmittedBlobReference,
}

impl AdmittedStoredBlob {
    /// The canonical declared metadata stored beside this reference.
    pub fn annotation(&self) -> &MediaAnnotation {
        &self.annotation
    }

    /// The content capability for the bytes this row names.
    pub fn reference(&self) -> &AdmittedBlobReference {
        &self.reference
    }

    /// Length and SHA-256 recorded by the row's Blob value.
    pub const fn content_identity(&self) -> crate::blob_store::ContentIdentity {
        self.reference.content_identity()
    }
}

/// Range verification state.  A range result never implies that the full
/// descriptor digest has been checked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RangeVerification {
    ReturnedChunks,
    FullBlob,
}

/// Bytes are exposed only by a result produced after native object, bounds,
/// and returned-chunk checks.  The constructor is crate-private.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedBlobRange {
    requested: Range<u64>,
    bytes: Vec<u8>,
    verification: RangeVerification,
    identity: crate::blob_store::ContentIdentity,
    context: ContextIdentity,
    owner_id: [u8; 16],
    snapshot_id: [u8; 32],
}

impl VerifiedBlobRange {
    fn after_chunk_verification(
        reference: &AdmittedBlobReference,
        requested: Range<u64>,
        bytes: Vec<u8>,
        verification: RangeVerification,
    ) -> Result<Self, GraphError> {
        if requested.start > requested.end
            || requested.end > reference.identity.length()
            || requested.end - requested.start != bytes.len() as u64
        {
            return Err(GraphError::InvalidRange);
        }
        Ok(Self {
            requested,
            bytes,
            verification,
            identity: reference.identity,
            context: reference.context,
            owner_id: reference.owner_id,
            snapshot_id: reference.snapshot_id,
        })
    }

    pub fn requested(&self) -> Range<u64> {
        self.requested.clone()
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub const fn verification(&self) -> RangeVerification {
        self.verification
    }

    pub const fn content_identity(&self) -> crate::blob_store::ContentIdentity {
        self.identity
    }
}

/// An opaque transfer record for activation state.  Persisting this value is
/// not itself a pin; the activation owner must atomically record it with the
/// accepted row state while the durable Git pin remains in place.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtectedContentTransfer {
    repository_id: [u8; 32],
    database_id: [u8; 16],
    owner_id: [u8; 16],
    pin_id: [u8; 16],
    descriptor_oid: NativeOid,
    identity: crate::blob_store::ContentIdentity,
}

impl ProtectedContentTransfer {
    pub const fn repository_id(&self) -> &[u8; 32] {
        &self.repository_id
    }

    pub const fn database_id(&self) -> &[u8; 16] {
        &self.database_id
    }

    pub const fn owner_id(&self) -> &[u8; 16] {
        &self.owner_id
    }

    pub const fn pin_id(&self) -> &[u8; 16] {
        &self.pin_id
    }

    pub fn descriptor_oid(&self) -> &NativeOid {
        &self.descriptor_oid
    }

    pub const fn content_identity(&self) -> crate::blob_store::ContentIdentity {
        self.identity
    }
}

/// An opaque, owner-issued pre-row Blob candidate. It retains its provisional
/// native ref until accepted through `protect_captured_blob`; dropping an
/// unaccepted candidate removes that ref. Raw OIDs are not exposed as
/// capture authority.
#[derive(Debug)]
pub struct CapturedBlobCandidate {
    context: ContextIdentity,
    repository_id: [u8; 32],
    database_id: [u8; 16],
    owner_id: [u8; 16],
    snapshot_id: [u8; 32],
    algorithm: GitHashAlgorithm,
    pin_id: [u8; 16],
    descriptor_oid: NativeOid,
    identity: crate::blob_store::ContentIdentity,
    cleanup: PendingPrivateRefCleanup,
}

impl CapturedBlobCandidate {
    pub const fn content_identity(&self) -> crate::blob_store::ContentIdentity {
        self.identity
    }
}

/// Proof that a complete content closure was verified, flushed and rooted by
/// a durable private Git ref.  It is non-cloneable and has no public
/// constructor.  Dropping it does not release that Git ref.
#[derive(Debug)]
pub struct ProtectedContentPin {
    context: ContextIdentity,
    repository_id: [u8; 32],
    database_id: [u8; 16],
    owner_id: [u8; 16],
    snapshot_id: [u8; 32],
    pin_id: [u8; 16],
    descriptor_oid: NativeOid,
    identity: crate::blob_store::ContentIdentity,
    protected_ref: String,
}

impl ProtectedContentPin {
    fn issue_after_durable_ref(
        repository_id: [u8; 32],
        database_id: [u8; 16],
        pin_id: [u8; 16],
        reference: &AdmittedBlobReference,
        protected_ref: String,
    ) -> Result<Self, GraphError> {
        if protected_ref.is_empty() || !protected_ref.starts_with("refs/orna/pins/") {
            return Err(GraphError::InvalidProtectedRef);
        }
        Ok(Self {
            context: reference.context,
            repository_id,
            database_id,
            owner_id: reference.owner_id,
            snapshot_id: reference.snapshot_id,
            pin_id,
            descriptor_oid: reference.descriptor_oid.clone(),
            identity: reference.identity,
            protected_ref,
        })
    }

    pub const fn pin_id(&self) -> &[u8; 16] {
        &self.pin_id
    }

    pub const fn content_identity(&self) -> crate::blob_store::ContentIdentity {
        self.identity
    }

    /// Safe, OID-free transfer evidence to be atomically accepted by the
    /// activation transaction together with its row state.
    pub fn transfer_record(&self) -> ProtectedContentTransfer {
        ProtectedContentTransfer {
            repository_id: self.repository_id,
            database_id: self.database_id,
            owner_id: self.owner_id,
            pin_id: self.pin_id,
            descriptor_oid: self.descriptor_oid.clone(),
            identity: self.identity,
        }
    }

    pub(crate) fn protected_ref(&self) -> &str {
        &self.protected_ref
    }
}

/// Canonical format-3 ORP Blob value produced only by accepting a durable,
/// graph-issued protected pin. The pin stays attached so its native transfer
/// evidence can accompany the row through durable acceptance.
#[derive(Debug)]
pub struct OrpBlobBinding {
    encoded_value: Vec<u8>,
    pin: ProtectedContentPin,
}

impl OrpBlobBinding {
    /// Canonical CBOR encoding of the format-3 Blob value (tag 60111).
    pub fn encoded_value(&self) -> &[u8] {
        &self.encoded_value
    }

    pub fn descriptor_oid(&self) -> &NativeOid {
        &self.pin.descriptor_oid
    }

    pub const fn content_identity(&self) -> crate::blob_store::ContentIdentity {
        self.pin.identity
    }

    pub fn transfer_record(&self) -> ProtectedContentTransfer {
        self.pin.transfer_record()
    }

    pub fn into_pin(self) -> ProtectedContentPin {
        self.pin
    }
}

/// ORP graph root prepared for the caller-owned durable publication commit.
///
/// The root remains a candidate until the shared callback succeeds. Its
/// transfer record is the evidence that must be committed with that root and
/// the runtime publication intent.
#[derive(Clone, Debug)]
pub struct OrpGraphCandidate {
    context: ContextIdentity,
    database_id: [u8; 16],
    relation_id: [u8; 16],
    schema_digest: [u8; 32],
    previous_root: NativeOid,
    primary_root: NativeOid,
    store_root: NativeOid,
    row_count: u64,
    key: crate::row_store::TypedKey,
    content_identity: crate::ContentIdentity,
    media_type: String,
    suffix: Option<String>,
    transfer: ProtectedContentTransfer,
}

impl OrpGraphCandidate {
    pub fn database_id(&self) -> &[u8; 16] {
        &self.database_id
    }

    pub fn relation_id(&self) -> &[u8; 16] {
        &self.relation_id
    }

    pub fn schema_digest(&self) -> &[u8; 32] {
        &self.schema_digest
    }

    pub fn previous_root(&self) -> &NativeOid {
        &self.previous_root
    }

    pub fn primary_root(&self) -> &NativeOid {
        &self.primary_root
    }

    pub fn store_root(&self) -> &NativeOid {
        &self.store_root
    }

    pub const fn row_count(&self) -> u64 {
        self.row_count
    }

    pub fn key(&self) -> &crate::row_store::TypedKey {
        &self.key
    }

    pub const fn content_identity(&self) -> crate::ContentIdentity {
        self.content_identity
    }

    pub fn media_type(&self) -> &str {
        &self.media_type
    }

    pub fn suffix(&self) -> Option<&str> {
        self.suffix.as_deref()
    }

    pub fn protected_content_transfer(&self) -> &ProtectedContentTransfer {
        &self.transfer
    }
}

/// Metadata returned by an ORP lookup of a protected Blob row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtectedBlobMetadata {
    content_identity: crate::ContentIdentity,
    media_type: String,
    suffix: Option<String>,
}

impl ProtectedBlobMetadata {
    pub const fn content_identity(&self) -> crate::ContentIdentity {
        self.content_identity
    }

    pub fn media_type(&self) -> &str {
        &self.media_type
    }

    pub fn suffix(&self) -> Option<&str> {
        self.suffix.as_deref()
    }
}

pub(crate) struct PreparedOrpGraphCandidate {
    candidate: OrpGraphCandidate,
    _binding: OrpBlobBinding,
    _cleanup: PendingPrivateRefCleanup,
    pin_cleanup: PendingPrivateRefCleanup,
}

#[derive(Debug)]
pub(crate) enum ProtectedBlobRowPreparationError {
    Graph(GraphError),
    Cleanup {
        graph: GraphError,
        cleanup: Vec<(String, GraphError)>,
    },
}

impl PreparedOrpGraphCandidate {
    pub(crate) fn candidate(&self) -> &OrpGraphCandidate {
        &self.candidate
    }

    pub(crate) fn cleanup_rejected_callback(
        mut self,
    ) -> Result<(), Vec<(String, GraphError)>> {
        let mut cleanup_failures = Vec::new();
        if let Err(failure) = self._cleanup.cleanup_with_identity() {
            cleanup_failures.push(failure);
        }
        if let Err(failure) = self.pin_cleanup.cleanup_with_identity() {
            cleanup_failures.push(failure);
        }
        if cleanup_failures.is_empty() {
            Ok(())
        } else {
            Err(cleanup_failures)
        }
    }

    pub(crate) fn into_candidate(mut self) -> OrpGraphCandidate {
        self.pin_cleanup.disarm();
        self.candidate
    }
}

/// Opaque PUB-3 evidence that the accepted content root is durable and the
/// publication journal has reached its terminal state.  Only publication
/// integration in this crate may issue one.
#[derive(Debug)]
pub struct Pub3ReleaseReceipt {
    repository_id: [u8; 32],
    pin_id: [u8; 16],
    descriptor_oid: NativeOid,
    durable_root: NativeOid,
    journal_terminal: bool,
}

impl Pub3ReleaseReceipt {
    pub(crate) fn issue_after_pub3(
        repository_id: [u8; 32],
        pin_id: [u8; 16],
        descriptor_oid: NativeOid,
        durable_root: NativeOid,
        journal_terminal: bool,
    ) -> Result<Self, GraphError> {
        if !journal_terminal || descriptor_oid.algorithm() != durable_root.algorithm() {
            return Err(GraphError::PublicationNotTerminal);
        }
        Ok(Self {
            repository_id,
            pin_id,
            descriptor_oid,
            durable_root,
            journal_terminal,
        })
    }

    pub(crate) fn authorizes(&self, pin: &ProtectedContentPin) -> bool {
        self.journal_terminal
            && self.repository_id == pin.repository_id
            && self.pin_id == pin.pin_id
            && self.descriptor_oid == pin.descriptor_oid
            && self.durable_root.algorithm() == pin.descriptor_oid.algorithm()
    }
}

/// The only Git tree entry kinds accepted for native graph references.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum NativeObjectKind {
    Tree,
    Blob,
}

/// A typed dependency named by a node's decoded `data` payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeDependency {
    oid: NativeObjectId,
    kind: NativeObjectKind,
}

impl NativeDependency {
    pub fn new(oid: NativeObjectId, kind: NativeObjectKind) -> Self {
        Self { oid, kind }
    }

    pub fn oid(&self) -> &NativeObjectId {
        &self.oid
    }

    pub const fn kind(&self) -> NativeObjectKind {
        self.kind
    }
}

/// The expected kind of one `refs` entry.  Git mode is represented by the
/// kind rather than by a caller-controlled path string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeRefEntry {
    oid: NativeObjectId,
    kind: NativeObjectKind,
}

impl NativeRefEntry {
    pub fn new(oid: NativeObjectId, kind: NativeObjectKind) -> Self {
        Self { oid, kind }
    }

    pub fn oid(&self) -> &NativeObjectId {
        &self.oid
    }

    pub const fn kind(&self) -> NativeObjectKind {
        self.kind
    }
}

/// Trees and blobs a stats walk reached, used to prune a later walk.
#[derive(Default)]
struct Reachable {
    trees: BTreeSet<NativeOid>,
    blobs: BTreeSet<NativeOid>,
}

/// Object counts reachable from one admitted store root, produced by
/// [`NativeGraphContext::object_stats`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ObjectStats {
    nodes: BTreeMap<NodeKind, u64>,
    blob_references: u64,
}

impl ObjectStats {
    /// Number of reachable nodes of `kind`; zero when none were reached.
    pub fn node_count(&self, kind: NodeKind) -> u64 {
        self.nodes.get(&kind).copied().unwrap_or(0)
    }

    pub const fn blob_references(&self) -> u64 {
        self.blob_references
    }

    pub fn nodes(&self) -> &BTreeMap<NodeKind, u64> {
        &self.nodes
    }

    /// True when no node was reached: the store has nothing to report.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Process exit code for `ogs stats`: 3 for an empty object store, so a
    /// script can tell "nothing stored" from a corrupt index (2) or a
    /// failure (1). Zero when the walk reached at least one node.
    pub fn exit_code(&self) -> i32 {
        if self.nodes.is_empty() { 3 } else { 0 }
    }

    /// Number of distinct node kinds reached, the `--count` form. Unlike
    /// [`Self::total_nodes`], each kind counts once however many nodes it has.
    pub fn kind_count(&self) -> usize {
        self.nodes.len()
    }

    /// Total reachable nodes across every kind.
    pub fn total_nodes(&self) -> u64 {
        self.nodes.values().sum()
    }

    /// The `--quiet` form: only the total node count, with no labels or
    /// per-kind rows, so scripts can read one number.
    pub fn to_quiet_line(&self) -> String {
        self.total_nodes().to_string()
    }

    /// Reached node kinds as `(name, count)` rows sorted by kind name, for
    /// listings that order by type name rather than format-3 order.
    pub fn rows_by_type_name(&self) -> Vec<(&'static str, u64)> {
        let mut rows: Vec<(&'static str, u64)> = self
            .nodes
            .iter()
            .map(|(kind, count)| (node_kind_name(*kind), *count))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(b.0));
        rows
    }

    /// Renders the counts as an aligned text table: one row per reached node
    /// kind sorted by name, then the blob reference count. The kind column is
    /// as wide as its longest name, so the counts line up.
    pub fn to_table(&self) -> String {
        let rows = self.rows_by_type_name();
        let width = rows
            .iter()
            .map(|(name, _)| name.len())
            .chain(["blob_references".len()])
            .max()
            .unwrap_or(0);
        let mut out = format!("{:<width$}  COUNT\n", "KIND");
        for (name, count) in rows {
            out.push_str(&format!("{name:<width$}  {count}\n"));
        }
        out.push_str(&format!("{:<width$}  {}\n", "blob_references", self.blob_references));
        out
    }

    /// Renders the counts as one line of JSON with a fixed key order: node
    /// kinds in format-3 order, then `blob_references`. Every key is a fixed
    /// identifier, so no escaping is needed.
    pub fn to_json(&self) -> String {
        let nodes = self
            .nodes
            .iter()
            .map(|(kind, count)| format!("\"{}\":{count}", node_kind_name(*kind)))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{{\"nodes\":{{{nodes}}},\"blob_references\":{},\"schema_version\":{STATS_SCHEMA_VERSION}}}",
            self.blob_references
        )
    }

    /// Version of the stats output schema, the value `ogs stats --version`
    /// reports. Bump it when a key or its meaning changes.
    pub const fn schema_version(&self) -> u32 {
        STATS_SCHEMA_VERSION
    }
}

/// Help text for the exit codes of `ogs stats`. Each line names one code the
/// stats exit with; the codes come from [`ObjectStats::exit_code`] and
/// [`GraphError::exit_code`].
pub const OGS_STATS_EXIT_CODES_HELP: &str = "\
Exit codes:
  0  the store has at least one reachable node
  1  any other failure
  2  corrupt index: bytes on disk do not match the format
  3  empty store, or an object id that does not resolve";

/// Output schema version for [`ObjectStats::to_json`].
pub const STATS_SCHEMA_VERSION: u32 = 1;

fn node_kind_name(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::StoreRoot => "StoreRoot",
        NodeKind::OrderedLeaf => "OrderedLeaf",
        NodeKind::OrderedBranch => "OrderedBranch",
        NodeKind::ValueOverflow => "ValueOverflow",
        NodeKind::ByteIndex => "ByteIndex",
        NodeKind::BlobDescriptor => "BlobDescriptor",
        NodeKind::Schema => "Schema",
        NodeKind::DependencyIndex => "DependencyIndex",
    }
}

/// The format-3 node kinds from `profiles/store-3.json`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum NodeKind {
    StoreRoot,
    OrderedLeaf,
    OrderedBranch,
    ValueOverflow,
    ByteIndex,
    BlobDescriptor,
    Schema,
    DependencyIndex,
}

impl NodeKind {
    pub const fn number(self) -> u8 {
        match self {
            Self::StoreRoot => 0,
            Self::OrderedLeaf => 1,
            Self::OrderedBranch => 2,
            Self::ValueOverflow => 3,
            Self::ByteIndex => 4,
            Self::BlobDescriptor => 5,
            Self::Schema => 6,
            Self::DependencyIndex => 7,
        }
    }

    fn from_number(number: u64) -> Result<Self, GraphError> {
        match number {
            0 => Ok(Self::StoreRoot),
            1 => Ok(Self::OrderedLeaf),
            2 => Ok(Self::OrderedBranch),
            3 => Ok(Self::ValueOverflow),
            4 => Ok(Self::ByteIndex),
            5 => Ok(Self::BlobDescriptor),
            6 => Ok(Self::Schema),
            7 => Ok(Self::DependencyIndex),
            _ => Err(GraphError::UnknownNodeKind(number)),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderedLeafEntry {
    /// Canonical CBOR encoding of the logical key value.
    pub key: Vec<u8>,
    /// Canonical CBOR encoding of the value (the ORP-1 `[key, value]` pair).
    pub value: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderedBranchEntry {
    /// Canonical CBOR encoding of the inclusive logical maximum key.
    pub inclusive_max_key: Vec<u8>,
    pub child: NativeObjectId,
    pub row_count: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ByteIndexEntry {
    pub span_length: u64,
    pub target: NativeObjectId,
    pub chunk_sha256: Option<[u8; 32]>,
}

/// Decoded OGS-1 node data.  Its encoder and decoder use definite, shortest
/// CBOR forms and retain the typed child roles needed to check `refs`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NodeData {
    StoreRoot {
        relation_map: NativeObjectId,
    },
    OrderedLeaf {
        domain: Vec<u8>,
        entries: Vec<OrderedLeafEntry>,
    },
    OrderedBranch {
        domain: Vec<u8>,
        height: u8,
        entries: Vec<OrderedBranchEntry>,
    },
    ValueOverflow {
        encoded_length: u64,
        semantic_digest: [u8; 32],
        byte_root: NativeObjectId,
        dependency_root: Option<NativeObjectId>,
    },
    ByteIndex {
        height: u8,
        total_length: u64,
        entries: Vec<ByteIndexEntry>,
    },
    BlobDescriptor {
        length: u64,
        raw_sha256: [u8; 32],
        byte_root: Option<NativeObjectId>,
    },
    Schema {
        encoded_length: u64,
        schema_digest: [u8; 32],
        byte_root: NativeObjectId,
    },
    DependencyIndex {
        height: u8,
        children: Vec<NativeObjectId>,
    },
}

impl NodeData {
    pub const fn kind(&self) -> NodeKind {
        match self {
            Self::StoreRoot { .. } => NodeKind::StoreRoot,
            Self::OrderedLeaf { .. } => NodeKind::OrderedLeaf,
            Self::OrderedBranch { .. } => NodeKind::OrderedBranch,
            Self::ValueOverflow { .. } => NodeKind::ValueOverflow,
            Self::ByteIndex { .. } => NodeKind::ByteIndex,
            Self::BlobDescriptor { .. } => NodeKind::BlobDescriptor,
            Self::Schema { .. } => NodeKind::Schema,
            Self::DependencyIndex { .. } => NodeKind::DependencyIndex,
        }
    }

    pub fn dependencies(&self) -> Result<Vec<NativeDependency>, GraphError> {
        let mut dependencies = Vec::new();
        match self {
            Self::StoreRoot { relation_map } => {
                dependencies.push(NativeDependency::new(
                    relation_map.clone(),
                    NativeObjectKind::Tree,
                ));
            }
            Self::OrderedLeaf { domain, entries } => {
                for entry in entries {
                    dependencies.extend(leaf_value_dependencies(domain, &entry.value)?);
                }
            }
            Self::OrderedBranch { entries, .. } => {
                for entry in entries {
                    dependencies.push(NativeDependency::new(
                        entry.child.clone(),
                        NativeObjectKind::Tree,
                    ));
                }
            }
            Self::ValueOverflow {
                byte_root,
                dependency_root,
                ..
            } => {
                dependencies.push(NativeDependency::new(
                    byte_root.clone(),
                    NativeObjectKind::Tree,
                ));
                if let Some(dependency_root) = dependency_root {
                    dependencies.push(NativeDependency::new(
                        dependency_root.clone(),
                        NativeObjectKind::Tree,
                    ));
                }
            }
            Self::ByteIndex {
                height, entries, ..
            } => {
                let expected = if *height == 0 {
                    NativeObjectKind::Blob
                } else {
                    NativeObjectKind::Tree
                };
                for entry in entries {
                    dependencies.push(NativeDependency::new(entry.target.clone(), expected));
                }
            }
            Self::BlobDescriptor { byte_root, .. } => {
                if let Some(byte_root) = byte_root {
                    dependencies.push(NativeDependency::new(
                        byte_root.clone(),
                        NativeObjectKind::Tree,
                    ));
                }
            }
            Self::Schema { byte_root, .. } => {
                dependencies.push(NativeDependency::new(
                    byte_root.clone(),
                    NativeObjectKind::Tree,
                ));
            }
            Self::DependencyIndex { height, children } => {
                let _ = height;
                for child in children {
                    dependencies.push(NativeDependency::new(child.clone(), NativeObjectKind::Tree));
                }
            }
        }
        let mut distinct = BTreeMap::new();
        for dependency in dependencies {
            if distinct
                .insert(dependency.oid.clone(), dependency.kind)
                .is_some_and(|previous| previous != dependency.kind)
            {
                return Err(GraphError::WrongReferenceKind);
            }
        }
        if distinct.len() > MAX_REFS {
            return Err(GraphError::FanoutExceeded(distinct.len()));
        }
        Ok(distinct
            .into_iter()
            .map(|(oid, kind)| NativeDependency::new(oid, kind))
            .collect())
    }

    pub fn encode_canonical(&self) -> Result<Vec<u8>, GraphError> {
        let mut output = Vec::new();
        match self {
            Self::StoreRoot { relation_map } => {
                array(&mut output, 3);
                uint(&mut output, 1);
                uint(&mut output, 0);
                bytes(&mut output, relation_map.as_bytes());
            }
            Self::OrderedLeaf { domain, entries } => {
                check_domain(domain)?;
                check_count(entries.len())?;
                check_leaf_key_order(domain, entries)?;
                array(&mut output, 4);
                uint(&mut output, 1);
                uint(&mut output, 1);
                output.extend_from_slice(domain);
                array(&mut output, entries.len() as u64);
                for entry in entries {
                    validate_canonical_value(&entry.key)?;
                    validate_canonical_value(&entry.value)?;
                    array(&mut output, 2);
                    output.extend_from_slice(&entry.key);
                    output.extend_from_slice(&entry.value);
                }
            }
            Self::OrderedBranch {
                domain,
                height,
                entries,
            } => {
                check_domain(domain)?;
                check_height(*height)?;
                check_count(entries.len())?;
                check_branch_fence_order(domain, entries).map_err(|error| match error {
                    GraphError::OverlappingChildRange => GraphError::NonCanonicalData,
                    other => other,
                })?;
                array(&mut output, 5);
                uint(&mut output, 1);
                uint(&mut output, 2);
                output.extend_from_slice(domain);
                uint(&mut output, u64::from(*height));
                array(&mut output, entries.len() as u64);
                for entry in entries {
                    check_length(entry.row_count)?;
                    validate_canonical_value(&entry.inclusive_max_key)?;
                    array(&mut output, 3);
                    output.extend_from_slice(&entry.inclusive_max_key);
                    bytes(&mut output, entry.child.as_bytes());
                    uint(&mut output, entry.row_count);
                }
            }
            Self::ValueOverflow {
                encoded_length,
                semantic_digest,
                byte_root,
                dependency_root,
            } => {
                check_length(*encoded_length)?;
                array(&mut output, 6);
                uint(&mut output, 1);
                uint(&mut output, 3);
                uint(&mut output, *encoded_length);
                bytes(&mut output, semantic_digest);
                bytes(&mut output, byte_root.as_bytes());
                match dependency_root {
                    Some(oid) => bytes(&mut output, oid.as_bytes()),
                    None => null(&mut output),
                }
            }
            Self::ByteIndex {
                height,
                total_length,
                entries,
            } => {
                check_height(*height)?;
                check_length(*total_length)?;
                validate_byte_entries(*height, *total_length, entries)?;
                array(&mut output, 5);
                uint(&mut output, 1);
                uint(&mut output, 4);
                uint(&mut output, u64::from(*height));
                uint(&mut output, *total_length);
                array(&mut output, entries.len() as u64);
                for entry in entries {
                    array(&mut output, 3);
                    uint(&mut output, entry.span_length);
                    bytes(&mut output, entry.target.as_bytes());
                    match entry.chunk_sha256 {
                        Some(digest) => bytes(&mut output, &digest),
                        None => null(&mut output),
                    }
                }
            }
            Self::BlobDescriptor {
                length,
                raw_sha256,
                byte_root,
            } => {
                check_length(*length)?;
                if (*length == 0) != byte_root.is_none() {
                    return Err(GraphError::InvalidEmptyBlob);
                }
                array(&mut output, 5);
                uint(&mut output, 1);
                uint(&mut output, 5);
                uint(&mut output, *length);
                bytes(&mut output, raw_sha256);
                match byte_root {
                    Some(oid) => bytes(&mut output, oid.as_bytes()),
                    None => null(&mut output),
                }
            }
            Self::Schema {
                encoded_length,
                schema_digest,
                byte_root,
            } => {
                check_length(*encoded_length)?;
                array(&mut output, 5);
                uint(&mut output, 1);
                uint(&mut output, 6);
                uint(&mut output, *encoded_length);
                bytes(&mut output, schema_digest);
                bytes(&mut output, byte_root.as_bytes());
            }
            Self::DependencyIndex { height, children } => {
                check_height(*height)?;
                check_count(children.len())?;
                // The inventory is sorted by native OID bytes (store-format-3
                // section 5), so a height-0 node's dependencies ascend. A node
                // above names its child indices in inventory order, which is
                // not the order of their own object IDs; it names each once.
                let canonical = if *height == 0 {
                    children
                        .windows(2)
                        .all(|pair| pair[0].as_bytes() < pair[1].as_bytes())
                } else {
                    children.iter().collect::<BTreeSet<_>>().len() == children.len()
                };
                if !canonical {
                    return Err(GraphError::NonCanonicalData);
                }
                array(&mut output, 4);
                uint(&mut output, 1);
                uint(&mut output, 7);
                uint(&mut output, u64::from(*height));
                array(&mut output, children.len() as u64);
                for child in children {
                    bytes(&mut output, child.as_bytes());
                }
            }
        }
        if output.len() > NODE_DATA_LIMIT {
            return Err(GraphError::NodeDataTooLarge(output.len()));
        }
        Ok(output)
    }

    /// Decodes the exact canonical subset emitted by [`Self::encode_canonical`].
    pub fn decode_canonical(bytes: &[u8], algorithm: GitHashAlgorithm) -> Result<Self, GraphError> {
        if bytes.len() > NODE_DATA_LIMIT {
            return Err(GraphError::NodeDataTooLarge(bytes.len()));
        }
        let mut reader = CborReader::new(bytes);
        let value = reader.value()?;
        if !reader.is_finished() {
            return Err(GraphError::NonCanonicalData);
        }
        let data = decode_node(value, algorithm)?;
        if let NodeData::OrderedBranch {
            domain, entries, ..
        } = &data
        {
            check_branch_fence_order(domain, entries)?;
        }
        if data.encode_canonical()?.as_slice() != bytes {
            return Err(GraphError::NonCanonicalData);
        }
        Ok(data)
    }
}

/// The complete `data` plus optional native Git `refs` tree entries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeNodeEnvelope {
    pub data: Vec<u8>,
    pub refs: Vec<NativeRefEntry>,
}

impl NativeNodeEnvelope {
    pub fn new(data: Vec<u8>, refs: Vec<NativeRefEntry>) -> Self {
        Self { data, refs }
    }

    pub fn from_node(data: &NodeData) -> Result<Self, GraphError> {
        let encoded = data.encode_canonical()?;
        let dependencies = data.dependencies()?;
        let refs = dependencies
            .iter()
            .map(|dependency| NativeRefEntry::new(dependency.oid.clone(), dependency.kind))
            .collect();
        Ok(Self::new(encoded, refs))
    }
}

/// A repository-owned format/hash binding.  There is intentionally no public
/// constructor: the eventual `Repository` integration must issue this after
/// validating the tracked format-3 metadata and native Git object format.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ValidatedRepositoryContext {
    algorithm: GitHashAlgorithm,
}

impl ValidatedRepositoryContext {
    pub(crate) const fn algorithm(self) -> GitHashAlgorithm {
        self.algorithm
    }
}

/// Test-only context creation keeps tests focused without creating a public
/// production admission route from arbitrary caller data.
#[cfg(test)]
pub(crate) const fn test_context(algorithm: GitHashAlgorithm) -> ValidatedRepositoryContext {
    ValidatedRepositoryContext { algorithm }
}

/// Local availability classification for an object observed by the owning
/// repository context.  Promised is not the same as materialized, and neither
/// is silently treated as success for full verification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectAvailability {
    Materialized,
    Promised,
    Unavailable,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedNativeObject {
    pub oid: NativeObjectId,
    pub kind: NativeObjectKind,
    pub availability: ObjectAvailability,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ValidatedNativeObject {
    pub(crate) oid: NativeObjectId,
    pub(crate) kind: NativeObjectKind,
    pub(crate) availability: ObjectAvailability,
}

/// Validate one observation through repository-owned format context.
pub(crate) fn validate_observed_object(
    context: &ValidatedRepositoryContext,
    observed: &ObservedNativeObject,
    expected_kind: NativeObjectKind,
) -> Result<ValidatedNativeObject, GraphError> {
    if observed.oid.algorithm() != context.algorithm {
        return Err(GraphError::InvalidOidWidth {
            expected: context.algorithm.width(),
            actual: observed.oid.as_bytes().len(),
        });
    }
    if observed.kind != expected_kind {
        return Err(GraphError::WrongObjectKind {
            expected: expected_kind,
            actual: observed.kind,
        });
    }
    match observed.availability {
        ObjectAvailability::Materialized | ObjectAvailability::Promised => {
            Ok(ValidatedNativeObject {
                oid: observed.oid.clone(),
                kind: observed.kind,
                availability: observed.availability,
            })
        }
        ObjectAvailability::Unavailable => Err(GraphError::UnavailableObject),
        ObjectAvailability::Unknown => Err(GraphError::UnknownObjectAvailability),
    }
}

/// Structural validation of one native graph node.  This is crate-private so
/// only the repository context owner can turn a validated Git observation into
/// a production admission result.
pub(crate) fn validate_native_node(
    context: &ValidatedRepositoryContext,
    envelope: &NativeNodeEnvelope,
) -> Result<NodeData, GraphError> {
    if envelope.data.len() > NODE_DATA_LIMIT {
        return Err(GraphError::NodeDataTooLarge(envelope.data.len()));
    }
    if envelope.refs.len() > MAX_REFS {
        return Err(GraphError::FanoutExceeded(envelope.refs.len()));
    }
    let data = NodeData::decode_canonical(&envelope.data, context.algorithm)?;
    let dependencies = data.dependencies()?;
    let mut expected = BTreeMap::new();
    for dependency in &dependencies {
        if let Some(kind) = expected.insert(dependency.oid.clone(), dependency.kind) {
            if kind != dependency.kind {
                return Err(GraphError::WrongReferenceKind);
            }
        }
    }
    let mut actual = BTreeMap::new();
    for entry in &envelope.refs {
        if actual.insert(entry.oid.clone(), entry.kind).is_some() {
            return Err(GraphError::DuplicateReference);
        }
    }
    for entry in &envelope.refs {
        if entry.oid.algorithm() != context.algorithm {
            return Err(GraphError::InvalidOidWidth {
                expected: context.algorithm.width(),
                actual: entry.oid.as_bytes().len(),
            });
        }
    }
    if expected != actual {
        if expected
            .keys()
            .any(|expected| !actual.contains_key(expected))
        {
            return Err(GraphError::MissingReference);
        }
        if actual.keys().any(|actual| !expected.contains_key(actual)) {
            return Err(GraphError::UnusedReference);
        }
        return Err(GraphError::WrongReferenceKind);
    }
    Ok(data)
}

/// Loads one relation's sealed ORP identity from the pinned format-3 store.
/// Rows are not materialized here: lookup/range operations traverse the
/// selected primary map lazily through the issued NativeGraphContext.
pub(crate) fn load_format3_row_map(
    repository: &crate::Repository,
    algorithm: GitHashAlgorithm,
    store_root: NativeObjectId,
    database_id: [u8; 16],
    snapshot_id: [u8; 32],
    relation_id: [u8; 16],
) -> Result<crate::row_store::RowMapSnapshot, GraphError> {
    if store_root.algorithm() != algorithm {
        return Err(GraphError::InvalidOidWidth {
            expected: algorithm.width(),
            actual: store_root.as_bytes().len(),
        });
    }
    let mut reader = PinnedNativeGraphReader::new(repository.clone(), algorithm);
    let root = reader.read_native_node(&store_root)?;
    let NodeData::StoreRoot { relation_map } = root else {
        return Err(GraphError::WrongNodeKind {
            expected: NodeKind::StoreRoot,
            actual: root.kind(),
        });
    };
    let relations_domain = canonical_domain(vec![
        CborValue::Text("relations".to_owned()),
        CborValue::Bytes(database_id.to_vec()),
    ]);
    let relation_key = crate::row_store::TypedKey::Bytes(relation_id.to_vec());
    let relation = reader
        .find_ordered_entry(&relation_map, &relations_domain, &relation_key)?
        .ok_or(GraphError::ContextMismatch)?;
    let (schema_oid, primary_root, _secondary_root, row_count) =
        decode_relation_value(&relation.value, algorithm)?;
    // The secondary OID is parsed but never dereferenced by primary admission.

    let schema_node = reader.read_native_node(&schema_oid)?;
    let NodeData::Schema {
        encoded_length,
        schema_digest,
        byte_root,
    } = schema_node
    else {
        return Err(GraphError::WrongNodeKind {
            expected: NodeKind::Schema,
            actual: schema_node.kind(),
        });
    };
    let schema_bytes = reader.read_schema_byte_index(&byte_root, encoded_length, schema_digest)?;
    let descriptor =
        SchemaDescriptor::decode(&schema_bytes).map_err(|_| GraphError::NonCanonicalData)?;
    if schema_relation_id(&descriptor)? != relation_id {
        return Err(GraphError::ContextMismatch);
    }


    let rows_domain = canonical_domain(vec![
        CborValue::Text("rows".to_owned()),
        CborValue::Bytes(relation_id.to_vec()),
        CborValue::Bytes(schema_digest.to_vec()),
    ]);
    reader.validate_map_root(&primary_root, &rows_domain, row_count)?;

    let generation = u64::from_be_bytes(snapshot_id[..8].try_into().unwrap_or([0; 8]));
    let schema = crate::row_store::SchemaGeneration::issue(
        database_id,
        relation_id,
        schema_oid,
        schema_digest,
        generation,
    );
    let version = crate::row_store::RowMapVersion::issue(
        database_id,
        relation_id,
        store_root.clone(),
        schema,
        primary_root,
        generation,
        Some(row_count),
    )
    .map_err(|_| GraphError::ContextMismatch)?;
    let mut authority = Sha256::new();
    authority.update(b"orna.repository.orp.snapshot-authority.v1\0");
    authority.update(database_id);
    authority.update(snapshot_id);
    authority.update(store_root.as_bytes());
    authority.update(relation_id);
    authority.update(version.primary_root().as_bytes());
    authority.update(schema_digest);
    authority.update(row_count.to_be_bytes());
    crate::row_store::RowMapSnapshot::issue_lazy(version, authority.finalize().into())
        .map_err(|_| GraphError::ContextMismatch)
}




fn schema_relation_id(schema: &SchemaDescriptor) -> Result<[u8; 16], GraphError> {
    let relation_key = Raw::Int(1.into());
    let Raw::Map(fields) = schema.raw() else {
        return Err(GraphError::NonCanonicalData);
    };
    let relation = fields
        .iter()
        .find(|(key, _)| key == &relation_key)
        .map(|(_, value)| value)
        .ok_or(GraphError::NonCanonicalData)?;
    let Raw::Tag(37, relation) = relation else {
        return Err(GraphError::NonCanonicalData);
    };
    let Raw::Bytes(bytes) = relation.as_ref() else {
        return Err(GraphError::NonCanonicalData);
    };
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| GraphError::NonCanonicalData)
}

fn canonical_domain(fields: Vec<CborValue>) -> Vec<u8> {
    canonical_value_bytes(&CborValue::Array(fields))
}

fn decode_relation_value(
    encoded: &[u8],
    algorithm: GitHashAlgorithm,
) -> Result<(NativeObjectId, NativeObjectId, Option<NativeObjectId>, u64), GraphError> {
    let CborValue::Array(fields) = decode_canonical_cbor(encoded)? else {
        return Err(GraphError::NonCanonicalData);
    };
    if fields.len() != 4 {
        return Err(GraphError::NonCanonicalData);
    }
    let decode_oid = |value: Option<&CborValue>| -> Result<NativeObjectId, GraphError> {
        let CborValue::Bytes(bytes) = value.ok_or(GraphError::NonCanonicalData)? else {
            return Err(GraphError::NonCanonicalData);
        };
        NativeObjectId::new(algorithm, bytes)
    };
    let schema_oid = decode_oid(fields.first())?;
    let primary_root = decode_oid(fields.get(1))?;
    let secondary_root = match fields.get(2).ok_or(GraphError::NonCanonicalData)? {
        CborValue::Null => None,
        CborValue::Bytes(bytes) => Some(NativeObjectId::new(algorithm, bytes)?),
        _ => return Err(GraphError::NonCanonicalData),
    };
    let Some(CborValue::Unsigned(row_count)) = fields.get(3) else {
        return Err(GraphError::NonCanonicalData);
    };
    if *row_count > MAX_SIGNED_LENGTH {
        return Err(GraphError::InvalidLength(*row_count));
    }
    Ok((schema_oid, primary_root, secondary_root, *row_count))
}

struct PinnedNativeGraphReader {
    repository: crate::Repository,
    algorithm: GitHashAlgorithm,
    seen: BTreeSet<NativeObjectId>,
    metadata_bytes: u64,
}

impl PinnedNativeGraphReader {
    fn new(repository: crate::Repository, algorithm: GitHashAlgorithm) -> Self {
        Self {
            repository,
            algorithm,
            seen: BTreeSet::new(),
            metadata_bytes: 0,
        }
    }


    fn validate_map_root(
        &mut self,
        root: &NativeObjectId,
        expected_domain: &[u8],
        expected_count: u64,
    ) -> Result<(), GraphError> {
        let root_node = self.read_native_node(root)?;
        match root_node {
            NodeData::OrderedLeaf { domain, entries } => {
                if domain != expected_domain {
                    return Err(GraphError::InvalidDomain);
                }
                if entries.len() as u64 != expected_count {
                    return Err(GraphError::InvalidCount(entries.len()));
                }
            }
            NodeData::OrderedBranch {
                domain, entries, ..
            } => {
                if domain != expected_domain {
                    return Err(GraphError::InvalidDomain);
                }
                check_branch_fence_order(&domain, &entries)?;
                if entries.len() == 1 {
                    return Err(GraphError::UncollapsedRoot);
                }
                let count = entries.iter().try_fold(0u64, |sum, entry| {
                    sum.checked_add(entry.row_count)
                        .filter(|count| *count <= MAX_SIGNED_LENGTH)
                        .ok_or(GraphError::InvalidCount(usize::MAX))
                })?;
                if count != expected_count {
                    return Err(GraphError::InvalidCount(
                        usize::try_from(count).unwrap_or(usize::MAX),
                    ));
                }
            }
            actual => {
                return Err(GraphError::WrongNodeKind {
                    expected: NodeKind::OrderedLeaf,
                    actual: actual.kind(),
                });
            }
        }
        Ok(())
    }

    fn find_ordered_entry(
        &mut self,
        root: &NativeObjectId,
        expected_domain: &[u8],
        key: &crate::row_store::TypedKey,
    ) -> Result<Option<OrderedLeafEntry>, GraphError> {
        let node = self.read_native_node(root)?;
        let mut read_node = |oid: &NativeOid| self.read_native_node(oid);
        find_ordered_entry_with(&mut read_node, node, expected_domain, key)
    }

    fn read_native_node(&mut self, oid: &NativeObjectId) -> Result<NodeData, GraphError> {
        if oid.algorithm() != self.algorithm {
            return Err(GraphError::InvalidOidWidth {
                expected: self.algorithm.width(),
                actual: oid.as_bytes().len(),
            });
        }
        let tree = self.read_object(NativeObjectKind::Tree, oid, MAX_INDEXED_TREE_BYTES)?;
        let entries = parse_git_tree(&tree, self.algorithm, 2)?;
        let mut data_oid = None;
        let mut refs_oid = None;
        for entry in entries {
            match entry.name.as_slice() {
                b"data" if entry.mode.as_slice() == b"100644" => data_oid = Some(entry.oid),
                b"refs" if entry.mode.as_slice() == b"40000" => refs_oid = Some(entry.oid),
                _ => return Err(GraphError::InvalidTreeEnvelope),
            }
        }
        let data = self.read_object(
            NativeObjectKind::Blob,
            &data_oid.ok_or(GraphError::MissingNodeData)?,
            NODE_DATA_LIMIT as u64,
        )?;
        let mut refs = Vec::new();
        if let Some(refs_oid) = refs_oid {
            let raw_refs =
                self.read_object(NativeObjectKind::Tree, &refs_oid, MAX_INDEXED_TREE_BYTES)?;
            for entry in parse_git_tree(&raw_refs, self.algorithm, MAX_REFS)? {
                let name =
                    std::str::from_utf8(&entry.name).map_err(|_| GraphError::MalformedOid)?;
                let named_oid = NativeOid::from_hex(self.algorithm, name)?;
                if named_oid != entry.oid {
                    return Err(GraphError::MalformedReferenceName);
                }
                let kind = match entry.mode.as_slice() {
                    b"100644" => NativeObjectKind::Blob,
                    b"40000" => NativeObjectKind::Tree,
                    _ => return Err(GraphError::WrongReferenceKind),
                };
                refs.push(NativeRefEntry::new(named_oid, kind));
            }
        }
        validate_native_node(
            &ValidatedRepositoryContext {
                algorithm: self.algorithm,
            },
            &NativeNodeEnvelope::new(data, refs),
        )
    }

    fn read_object(
        &mut self,
        kind: NativeObjectKind,
        oid: &NativeObjectId,
        maximum: u64,
    ) -> Result<Vec<u8>, GraphError> {
        if oid.algorithm() != self.algorithm {
            return Err(GraphError::InvalidOidWidth {
                expected: self.algorithm.width(),
                actual: oid.as_bytes().len(),
            });
        }
        let seen = &mut self.seen;
        let metadata_bytes = &mut self.metadata_bytes;
        read_checked_object(&self.repository, self.algorithm, kind, oid, |size| {
            if size > maximum {
                return Err(GraphError::NodeDataTooLarge(
                    usize::try_from(size).unwrap_or(usize::MAX),
                ));
            }
            if seen.insert(oid.clone()) {
                if seen.len() as u64 > MAX_FULL_VERIFY_OBJECTS {
                    return Err(GraphError::InventoryQuotaExceeded);
                }
                *metadata_bytes = metadata_bytes
                    .checked_add(size)
                    .ok_or(GraphError::MetadataQuotaExceeded)?;
                if *metadata_bytes > MAX_FULL_METADATA_BYTES {
                    return Err(GraphError::MetadataQuotaExceeded);
                }
            }
            Ok(())
        })
    }


    /// Reads a kind-6 node's descriptor bytes and checks them against the
    /// node's `orna.schema.v3` identity (store-format-3 section 5), hashed
    /// while the byte index streams.
    fn read_schema_byte_index(
        &mut self,
        root: &NativeObjectId,
        expected_length: u64,
        schema_identity: [u8; 32],
    ) -> Result<Vec<u8>, GraphError> {
        if expected_length > MAX_RANGE_METADATA_BYTES {
            return Err(GraphError::MetadataQuotaExceeded);
        }
        let capacity =
            usize::try_from(expected_length).map_err(|_| GraphError::MetadataQuotaExceeded)?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(capacity)
            .map_err(|_| GraphError::MetadataQuotaExceeded)?;
        let mut hasher = Sha256::new();
        hasher.update(SCHEMA_IDENTITY_DOMAIN.as_bytes());
        hasher.update([0]);
        let (length, _) = self.walk_byte_index(
            root,
            None,
            &mut hasher,
            Some(&mut output),
            Some(expected_length),
        )?;
        let digest: [u8; 32] = hasher.finalize().into();
        if length != expected_length || digest != schema_identity {
            return Err(GraphError::ContentIdentityMismatch);
        }
        Ok(output)
    }

    fn verify_full_byte_index(
        &mut self,
        root: &NativeObjectId,
        expected_length: u64,
        expected_digest: [u8; 32],
    ) -> Result<(), GraphError> {
        let mut hasher = Sha256::new();
        let (length, _) = self.walk_byte_index(root, None, &mut hasher, None, None)?;
        let digest: [u8; 32] = hasher.finalize().into();
        if length != expected_length || digest != expected_digest {
            return Err(GraphError::ContentIdentityMismatch);
        }
        Ok(())
    }

    fn walk_byte_index(
        &mut self,
        oid: &NativeObjectId,
        expected_height: Option<u8>,
        hasher: &mut Sha256,
        mut output: Option<&mut Vec<u8>>,
        output_limit: Option<u64>,
    ) -> Result<(u64, u8), GraphError> {
        let node = self.read_native_node(oid)?;
        let NodeData::ByteIndex {
            height,
            total_length,
            entries,
        } = node
        else {
            return Err(GraphError::WrongNodeKind {
                expected: NodeKind::ByteIndex,
                actual: node.kind(),
            });
        };
        if expected_height.is_some_and(|expected| expected != height) {
            return Err(GraphError::InvalidByteIndex);
        }
        let mut total = 0u64;
        for entry in entries {
            if height == 0 {
                if entry.span_length > crate::blob_store::GEAR_MAXIMUM as u64 {
                    return Err(GraphError::InvalidByteIndex);
                }
                let chunk = self.read_object(
                    NativeObjectKind::Blob,
                    &entry.target,
                    crate::blob_store::GEAR_MAXIMUM as u64,
                )?;
                let chunk_digest: [u8; 32] = Sha256::digest(&chunk).into();
                if chunk.len() as u64 != entry.span_length
                    || entry.chunk_sha256 != Some(chunk_digest)
                {
                    return Err(GraphError::ChunkDigestMismatch);
                }
                hasher.update(&chunk);
                if let Some(output) = output.as_deref_mut() {
                    let next_len = output
                        .len()
                        .checked_add(chunk.len())
                        .ok_or(GraphError::MetadataQuotaExceeded)?;
                    if output_limit.is_none_or(|limit| next_len as u64 > limit) {
                        return Err(GraphError::MetadataQuotaExceeded);
                    }
                    output.extend_from_slice(&chunk);
                }
            } else {
                let (child_length, child_height) = self.walk_byte_index(
                    &entry.target,
                    Some(height - 1),
                    hasher,
                    output.as_deref_mut(),
                    output_limit,
                )?;
                if child_length != entry.span_length || child_height + 1 != height {
                    return Err(GraphError::InvalidByteIndex);
                }
            }
            total = total
                .checked_add(entry.span_length)
                .ok_or(GraphError::InvalidByteIndex)?;
        }
        if total != total_length {
            return Err(GraphError::InvalidByteIndex);
        }
        Ok((total, height))
    }

}

fn find_ordered_entry_with(
    read_node: &mut impl FnMut(&NativeOid) -> Result<NodeData, GraphError>,
    node: NodeData,
    expected_domain: &[u8],
    key: &crate::row_store::TypedKey,
) -> Result<Option<OrderedLeafEntry>, GraphError> {
    // ORP-1 child ranges are nonoverlapping and exhaustive in fence order
    // (store-format-3 section 2), so the first child whose inclusive maximum
    // fence is at or above the key is the only child that can hold it. A
    // point lookup therefore reads exactly one node per level, and a miss
    // returns after that single path instead of probing later siblings.
    //
    // Every node on the path is checked against what its parent's entry
    // claims for it: domain, height, exact row count, an inclusive maximum
    // equal to the fence, and a minimum above the previous fence. Those checks
    // use only the node itself, so they add no reads. A malformed range on
    // the path is rejected; overlap between children off the path is left to
    // full admission and to range reads, which read those children anyway.
    let mut node = node;
    // The root's height is whatever it records, and it has no lower bound.
    let mut expected_height: Option<u8> = None;
    let mut lower_exclusive: Option<crate::row_store::TypedKey> = None;
    // The parent's claim for `node`: its exact row count and inclusive
    // maximum key. The root has no parent entry and so no claim.
    let mut claim: Option<(u64, crate::row_store::TypedKey)> = None;
    loop {
        match node {
            NodeData::OrderedLeaf { domain, entries } => {
                if domain != expected_domain || expected_height.is_some_and(|height| height != 0) {
                    return Err(GraphError::InvalidDomain);
                }
                let mut decoded: Vec<(crate::row_store::TypedKey, &OrderedLeafEntry)> =
                    Vec::with_capacity(entries.len());
                for entry in &entries {
                    let entry_key = crate::row_store::TypedKey::decode_canonical(&entry.key)
                        .map_err(|_| GraphError::NonCanonicalData)?;
                    if decoded.last().is_some_and(|(last, _)| last >= &entry_key) {
                        return Err(GraphError::NonCanonicalData);
                    }
                    decoded.push((entry_key, entry));
                }
                let breaks_claim = claim.as_ref().is_some_and(|(row_count, fence)| {
                    entries.len() as u64 != *row_count
                        || decoded.last().is_none_or(|(maximum, _)| maximum != fence)
                });
                let breaks_lower = lower_exclusive.as_ref().is_some_and(|lower| {
                    decoded.first().is_some_and(|(minimum, _)| minimum <= lower)
                });
                if breaks_claim || breaks_lower {
                    return Err(GraphError::InvalidCount(entries.len()));
                }
                return Ok(decoded
                    .binary_search_by(|(candidate, _)| candidate.cmp(key))
                    .ok()
                    .map(|index| decoded[index].1.clone()));
            }
            NodeData::OrderedBranch {
                domain,
                height,
                entries,
            } => {
                if domain != expected_domain
                    || height == 0
                    || expected_height.is_some_and(|expected| expected != height)
                    || entries.is_empty()
                {
                    return Err(GraphError::InvalidDomain);
                }
                let total_count = entries.iter().try_fold(0u64, |sum, entry| {
                    sum.checked_add(entry.row_count)
                        .filter(|count| *count <= MAX_SIGNED_LENGTH)
                        .ok_or(GraphError::InvalidCount(usize::MAX))
                })?;
                let mut fences: Vec<crate::row_store::TypedKey> = Vec::with_capacity(entries.len());
                for entry in &entries {
                    let fence =
                        crate::row_store::TypedKey::decode_canonical(&entry.inclusive_max_key)
                            .map_err(|_| GraphError::NonCanonicalData)?;
                    if fences.last().is_some_and(|previous| previous >= &fence) {
                        return Err(GraphError::NonCanonicalData);
                    }
                    fences.push(fence);
                }
                // This branch's maximum is its last child's fence and its
                // minimum is no greater than its first child's fence, so both
                // halves of the parent's claim are checked without reading
                // any child other than the one the key selects.
                let breaks_claim = claim.as_ref().is_some_and(|(row_count, maximum)| {
                    total_count != *row_count || fences.last() != Some(maximum)
                });
                let breaks_lower = lower_exclusive
                    .as_ref()
                    .is_some_and(|lower| fences.first().is_some_and(|first| first <= lower));
                if breaks_claim || breaks_lower {
                    return Err(GraphError::InvalidCount(
                        usize::try_from(total_count).unwrap_or(usize::MAX),
                    ));
                }
                let index = fences.partition_point(|fence| fence < key);
                let Some(entry) = entries.get(index) else {
                    // The key sorts above this node's maximum: no row holds it.
                    return Ok(None);
                };
                // The first child inherits this node's own lower bound; any
                // later child must sort above its predecessor's fence.
                if let Some(index) = index.checked_sub(1) {
                    lower_exclusive = Some(fences[index].clone());
                }
                let child = read_node(&entry.child)?;
                expected_height = Some(height - 1);
                claim = Some((entry.row_count, fences.swap_remove(index)));
                node = child;
            }
            other => {
                return Err(GraphError::WrongNodeKind {
                    expected: NodeKind::OrderedLeaf,
                    actual: other.kind(),
                });
            }
        }
    }
}

fn replace_ordered_entry_with(
    read_node: &mut impl FnMut(&NativeOid) -> Result<NodeData, GraphError>,
    write_node: &mut impl FnMut(&NodeData) -> Result<NativeOid, GraphError>,
    node: NodeData,
    expected_domain: &[u8],
    expected_height: Option<u8>,
    lower_exclusive: Option<&crate::row_store::TypedKey>,
    key: &crate::row_store::TypedKey,
    value: Vec<u8>,
) -> Result<NativeOid, GraphError> {
    match node {
        NodeData::OrderedLeaf { domain, mut entries } => {
            if domain != expected_domain || expected_height.is_some_and(|height| height != 0) {
                return Err(GraphError::InvalidDomain);
            }
            check_leaf_key_order(&domain, &entries)?;
            if lower_exclusive.is_some_and(|lower| {
                entries
                    .first()
                    .and_then(|entry| crate::row_store::TypedKey::decode_canonical(&entry.key).ok())
                    .is_some_and(|minimum| minimum <= *lower)
            }) {
                return Err(GraphError::NonCanonicalData);
            }
            let index = entries
                .binary_search_by(|entry| {
                    crate::row_store::TypedKey::decode_canonical(&entry.key)
                        .map(|candidate| candidate.cmp(key))
                        .unwrap_or(std::cmp::Ordering::Less)
                })
                .map_err(|_| GraphError::ContextMismatch)?;
            entries[index].value = value;
            write_node(&NodeData::OrderedLeaf { domain, entries })
        }
        NodeData::OrderedBranch {
            domain,
            height,
            mut entries,
        } => {
            if domain != expected_domain
                || height == 0
                || expected_height.is_some_and(|expected| expected != height)
                || entries.is_empty()
            {
                return Err(GraphError::InvalidDomain);
            }
            check_branch_fence_order(&domain, &entries)?;
            let mut previous_fence: Option<crate::row_store::TypedKey> = None;
            let mut selected = None;
            for (index, entry) in entries.iter().enumerate() {
                let fence = crate::row_store::TypedKey::decode_canonical(&entry.inclusive_max_key)
                    .map_err(|_| GraphError::NonCanonicalData)?;
                if key <= &fence {
                    let child = read_node(&entry.child)?;
                    let bounds = ordered_node_bounds_with(
                        read_node,
                        child.clone(),
                        expected_domain,
                        Some(height - 1),
                    )?;
                    if bounds.0 != entry.row_count
                        || bounds.2 != fence
                        || previous_fence
                            .as_ref()
                            .is_some_and(|previous| bounds.1 <= *previous)
                    {
                        return Err(GraphError::InvalidCount(
                            usize::try_from(bounds.0).unwrap_or(usize::MAX),
                        ));
                    }
                    selected = Some((index, child, previous_fence.as_ref().cloned()));
                    break;
                }
                previous_fence = Some(fence);
            }
            let Some((index, child, lower_bound)) = selected else {
                return Err(GraphError::ContextMismatch);
            };
            entries[index].child = replace_ordered_entry_with(
                read_node,
                write_node,
                child,
                expected_domain,
                Some(height - 1),
                lower_bound.as_ref(),
                key,
                value,
            )?;
            write_node(&NodeData::OrderedBranch {
                domain,
                height,
                entries,
            })
        }
        other => Err(GraphError::WrongNodeKind {
            expected: NodeKind::OrderedLeaf,
            actual: other.kind(),
        }),
    }
}

/// Collects every node object of one ordered index (root, branches and leaves)
/// plus every stored-value payload object its entries reference, validating
/// each node against its parent's claims on the way down.
fn collect_ordered_index_objects_with(
    read_node: &mut impl FnMut(&NativeOid) -> Result<NodeData, GraphError>,
    node: &NodeData,
    node_oid: &NativeOid,
    expected_domain: &[u8],
    expected_height: Option<u8>,
    nodes: &mut BTreeSet<NativeOid>,
    payload: &mut BTreeSet<NativeOid>,
    // One walk has a single fixed domain; include height so a shared OID
    // reached at another level is validated again under that parent context.
    validated_contexts: &mut BTreeSet<(NativeOid, Option<u8>)>,
) -> Result<(), GraphError> {
    let context = (node_oid.clone(), expected_height);
    match node {
        NodeData::OrderedLeaf { domain, entries } => {
            if domain != expected_domain || expected_height.is_some_and(|height| height != 0) {
                return Err(GraphError::InvalidDomain);
            }
            check_leaf_key_order(domain, entries)?;
            if !validated_contexts.insert(context) {
                return Ok(());
            }
            for entry in entries {
                for dependency in leaf_value_dependencies(domain, &entry.value)? {
                    payload.insert(dependency.oid);
                }
            }
            Ok(())
        }
        NodeData::OrderedBranch {
            domain,
            height,
            entries,
        } => {
            if domain != expected_domain
                || *height == 0
                || expected_height.is_some_and(|expected| expected != *height)
                || entries.is_empty()
            {
                return Err(GraphError::InvalidDomain);
            }
            check_branch_fence_order(domain, entries)?;
            if !validated_contexts.insert(context) {
                return Ok(());
            }
            let mut previous_fence: Option<crate::row_store::TypedKey> = None;
            for entry in entries {
                let fence = crate::row_store::TypedKey::decode_canonical(&entry.inclusive_max_key)
                    .map_err(|_| GraphError::NonCanonicalData)?;
                let child = read_node(&entry.child)?;
                let bounds = ordered_node_bounds_with(
                    read_node,
                    child.clone(),
                    expected_domain,
                    Some(*height - 1),
                )?;
                if bounds.0 != entry.row_count
                    || bounds.2 != fence
                    || previous_fence
                        .as_ref()
                        .is_some_and(|previous| bounds.1 <= *previous)
                {
                    return Err(GraphError::InvalidCount(
                        usize::try_from(bounds.0).unwrap_or(usize::MAX),
                    ));
                }
                let child_oid = entry.child.clone();
                nodes.insert(child_oid.clone());
                collect_ordered_index_objects_with(
                    read_node,
                    &child,
                    &child_oid,
                    expected_domain,
                    Some(*height - 1),
                    nodes,
                    payload,
                    validated_contexts,
                )?;
                previous_fence = Some(fence);
            }
            Ok(())
        }
        other => Err(GraphError::WrongNodeKind {
            expected: NodeKind::OrderedLeaf,
            actual: other.kind(),
        }),
    }
}

fn ordered_node_bounds_with(
    read_node: &mut impl FnMut(&NativeOid) -> Result<NodeData, GraphError>,
    node: NodeData,
    expected_domain: &[u8],
    expected_height: Option<u8>,
) -> Result<(u64, crate::row_store::TypedKey, crate::row_store::TypedKey), GraphError> {
    match node {
        NodeData::OrderedLeaf { domain, entries } => {
            if domain != expected_domain || expected_height.is_some_and(|height| height != 0) {
                return Err(GraphError::InvalidDomain);
            }
            let first = entries.first().ok_or(GraphError::InvalidCount(0))?;
            let last = entries.last().ok_or(GraphError::InvalidCount(0))?;
            let minimum = crate::row_store::TypedKey::decode_canonical(&first.key)
                .map_err(|_| GraphError::NonCanonicalData)?;
            let maximum = crate::row_store::TypedKey::decode_canonical(&last.key)
                .map_err(|_| GraphError::NonCanonicalData)?;
            Ok((entries.len() as u64, minimum, maximum))
        }
        NodeData::OrderedBranch {
            domain,
            height,
            entries,
        } => {
            if domain != expected_domain
                || height == 0
                || expected_height.is_some_and(|expected| expected != height)
                || entries.is_empty()
            {
                return Err(GraphError::InvalidDomain);
            }
            let total_count = entries.iter().try_fold(0u64, |sum, entry| {
                sum.checked_add(entry.row_count)
                    .filter(|count| *count <= MAX_SIGNED_LENGTH)
                    .ok_or(GraphError::InvalidCount(usize::MAX))
            })?;
            let first = entries.first().ok_or(GraphError::InvalidCount(0))?;
            let last = entries.last().ok_or(GraphError::InvalidCount(0))?;
            let first_fence =
                crate::row_store::TypedKey::decode_canonical(&first.inclusive_max_key)
                    .map_err(|_| GraphError::NonCanonicalData)?;
            let last_fence = crate::row_store::TypedKey::decode_canonical(&last.inclusive_max_key)
                .map_err(|_| GraphError::NonCanonicalData)?;
            let first_node = read_node(&first.child)?;
            let first_bounds =
                ordered_node_bounds_with(read_node, first_node, expected_domain, Some(height - 1))?;
            if first_bounds.0 != first.row_count || first_bounds.2 != first_fence {
                return Err(GraphError::InvalidCount(
                    usize::try_from(first_bounds.0).unwrap_or(usize::MAX),
                ));
            }
            let (minimum, maximum) = if std::ptr::eq(first, last) {
                (first_bounds.1, first_bounds.2)
            } else {
                let last_node = read_node(&last.child)?;
                let last_bounds = ordered_node_bounds_with(
                    read_node,
                    last_node,
                    expected_domain,
                    Some(height - 1),
                )?;
                if last_bounds.0 != last.row_count || last_bounds.2 != last_fence {
                    return Err(GraphError::InvalidCount(
                        usize::try_from(last_bounds.0).unwrap_or(usize::MAX),
                    ));
                }
                if last_bounds.1 <= first_bounds.2 {
                    return Err(GraphError::NonCanonicalData);
                }
                (first_bounds.1, last_bounds.2)
            };
            Ok((total_count, minimum, maximum))
        }
        other => Err(GraphError::WrongNodeKind {
            expected: NodeKind::OrderedLeaf,
            actual: other.kind(),
        }),
    }
}

fn rows_domain(relation_id: &[u8; 16], schema_digest: &[u8; 32]) -> Vec<u8> {
    canonical_domain(vec![
        CborValue::Text("rows".to_owned()),
        CborValue::Bytes(relation_id.to_vec()),
        CborValue::Bytes(schema_digest.to_vec()),
    ])
}

/// Domain of the format-3 schema identity (store-format-3 section 5).
pub(crate) const SCHEMA_IDENTITY_DOMAIN: &str = "orna.schema.v3";

/// Domain of the format-3 row identity (store-format-3 section 5).
const ROW_IDENTITY_DOMAIN: &str = "orna.row.v3";

/// SHA-256 of an ASCII domain, NUL and a canonical SOV-3 payload: a format-3
/// domain digest (store-format-3 section 5).
fn sov3_domain_digest(domain: &str, payload: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(domain.as_bytes());
    digest.update([0]);
    digest.update(payload);
    digest.finalize().into()
}

/// The format-3 schema identity a kind-6 node records and every rows domain
/// of the relation binds: `orna.schema.v3` over the full closed storage
/// descriptor. A descriptor holds no Blob, so its canonical SOV-3 encoding
/// is its canonical descriptor bytes.
pub(crate) fn schema_identity_v3(descriptor: &[u8]) -> [u8; 32] {
    sov3_domain_digest(SCHEMA_IDENTITY_DOMAIN, descriptor)
}

/// The format-3 row identity (store-format-3 section 5): `orna.row.v3` over
/// the canonical SOV-3 encoding of `[database_uuid, table_uuid, typed_key,
/// stored_fields]`. `stored_fields` is the row's canonical ROV-3 field tuple
/// with every overflow value already expanded; each Blob enters as its
/// tag-60112 form, so the identity binds content and annotations and no
/// descriptor, page or overflow object ID enters it.
fn row_identity_v3(
    database_id: &[u8; 16],
    relation_id: &[u8; 16],
    key: &crate::row_store::TypedKey,
    stored_fields: &[u8],
    oid_width: usize,
) -> Result<[u8; 32], GraphError> {
    let fields = sov3_value(
        decode_canonical_content(stored_fields)?,
        oid_width,
        // An overflow reference here was not expanded: not a logical row.
        &mut |_| Err(GraphError::NonCanonicalData),
    )?;
    if !matches!(fields, CborValue::Array(_)) {
        return Err(GraphError::NonCanonicalData);
    }
    let key = decode_canonical_content(
        &key.canonical_bytes()
            .map_err(|_| GraphError::NonCanonicalData)?,
    )?;
    let uuid = |id: &[u8; 16]| CborValue::Tag(37, Box::new(CborValue::Bytes(id.to_vec())));
    let payload = CborValue::Array(vec![uuid(database_id), uuid(relation_id), key, fields]);
    Ok(sov3_domain_digest(
        ROW_IDENTITY_DOMAIN,
        &canonical_value_bytes(&payload),
    ))
}

fn validate_ordered_root_node(
    node: &NodeData,
    expected_domain: &[u8],
    expected_count: u64,
) -> Result<(), GraphError> {
    match node {
        NodeData::OrderedLeaf { domain, entries } => {
            if domain != expected_domain || entries.len() as u64 != expected_count {
                return Err(GraphError::InvalidCount(entries.len()));
            }
        }
        NodeData::OrderedBranch {
            domain, entries, ..
        } => {
            if domain != expected_domain || entries.len() < 2 {
                return Err(GraphError::InvalidDomain);
            }
            check_branch_fence_order(domain, entries)?;
            let count = entries.iter().try_fold(0u64, |sum, entry| {
                sum.checked_add(entry.row_count)
                    .filter(|count| *count <= MAX_SIGNED_LENGTH)
                    .ok_or(GraphError::InvalidCount(usize::MAX))
            })?;
            if count != expected_count {
                return Err(GraphError::InvalidCount(
                    usize::try_from(count).unwrap_or(usize::MAX),
                ));
            }
        }
        actual => {
            return Err(GraphError::WrongNodeKind {
                expected: NodeKind::OrderedLeaf,
                actual: actual.kind(),
            });
        }
    }
    Ok(())
}

/// The outcome of one row-map maintenance pass: the maintained map, the rows
/// it holds and the logical rekeys the batch made.
#[derive(Debug)]
struct RowMapPass {
    maintained: crate::orp::maintenance::Maintained,
    row_count: u64,
    logical_rekeys: Vec<LogicalRowRekey>,
}

/// Applies `mutations` to the pinned row map whose validated root node
/// `root_node` holds `expected_count` rows. This is all of
/// [`NativeGraphContext::maintain_row_index`] except where nodes are read
/// from and written to: `read_node` reads old nodes and `store` takes every
/// node and overflow object the pass writes.
///
/// Deletes apply first, then rekeys, then inserts, each kind in batch order
/// and each checked against the rows held by then: a delete or rekey source
/// that is absent is `RowKeyAbsent`, and a rekey onto a present key or an
/// insert of one is `DuplicateRowKey`. The first failure in that order is
/// the one reported. Every key the batch names resolves to one slot, so the
/// batch may arrive in any order and a rekey may chain onto an earlier
/// rekey's target or reuse a key the batch removed.
fn maintain_row_map(
    domain: &[u8],
    root: NativeOid,
    root_node: NodeData,
    expected_count: u64,
    mutations: &[RowIndexMutation],
    read_node: &mut impl FnMut(&NativeOid) -> Result<NodeData, GraphError>,
    store: &mut impl crate::overflow::OverflowStore,
) -> Result<RowMapPass, GraphError> {
    use crate::orp::maintenance::{
        RowChange, RowChanges, lookup_ordered_entries, maintain_ordered_map,
    };
    use crate::row_store::TypedKey;

    // The pinned value of every key a mutation names, from one descent over
    // the touched pages.
    let touched: Vec<TypedKey> = mutations
        .iter()
        .flat_map(|mutation| match mutation {
            RowIndexMutation::Delete { key } | RowIndexMutation::Insert { key, .. } => {
                [Some(key), None]
            }
            RowIndexMutation::Rekey { from, to } => [Some(from), Some(to)],
        })
        .flatten()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let pinned = lookup_ordered_entries(domain, &root_node, &touched, &mut *read_node)?;
    let mut rows: BTreeMap<TypedKey, Option<Cow<'_, [u8]>>> = touched
        .into_iter()
        .map(|key| {
            let value = pinned
                .get(&key)
                .map(|value| Cow::Borrowed(value.as_slice()));
            (key, value)
        })
        .collect();

    for mutation in mutations {
        let RowIndexMutation::Delete { key } = mutation else {
            continue;
        };
        let slot = rows.get_mut(key).ok_or(GraphError::ContextMismatch)?;
        if slot.take().is_none() {
            return Err(GraphError::RowKeyAbsent);
        }
    }
    let mut logical_rekeys = Vec::new();
    for mutation in mutations {
        let RowIndexMutation::Rekey { from, to } = mutation else {
            continue;
        };
        if rows.get(from).is_none_or(Option::is_none) {
            return Err(GraphError::RowKeyAbsent);
        }
        if rows.get(to).is_some_and(Option::is_some) {
            return Err(GraphError::DuplicateRowKey);
        }
        let value = rows.get_mut(from).and_then(Option::take);
        rows.insert(to.clone(), value);
        logical_rekeys.push(LogicalRowRekey {
            old_address: from.clone(),
            new_address: to.clone(),
        });
    }
    // Inserted rows are lowered to the shared overflow graph first, so a row
    // or value over 8 KiB or 32 direct Blob references is stored through tag
    // 60113 (store-format-3 section 5).
    let mut overflow = crate::overflow::OverflowWriter::new(&mut *store);
    for mutation in mutations {
        let RowIndexMutation::Insert { key, fields } = mutation else {
            continue;
        };
        let slot = rows.get_mut(key).ok_or(GraphError::ContextMismatch)?;
        if slot.is_some() {
            return Err(GraphError::DuplicateRowKey);
        }
        let value = overflow
            .lower_row(fields)?
            .canonical_page_bytes()
            .map_err(|_| GraphError::NonCanonicalData)?;
        *slot = Some(Cow::Owned(value));
    }

    let mut changes = RowChanges::new();
    let mut row_count = expected_count;
    for (key, value) in rows {
        let before = pinned.get(&key).map(Vec::as_slice);
        let change = match (value, before) {
            (None, None) => continue,
            (Some(value), Some(before)) if *value == *before => continue,
            (None, Some(_)) => {
                row_count = row_count
                    .checked_sub(1)
                    .ok_or(GraphError::InvalidCount(0))?;
                RowChange::Remove
            }
            (Some(value), before) => {
                if before.is_none() {
                    row_count += 1;
                }
                RowChange::Put(value.into_owned())
            }
        };
        let key_bytes = key
            .canonical_bytes()
            .map_err(|_| GraphError::NonCanonicalData)?;
        changes.insert(key, (key_bytes, change));
    }
    if row_count > MAX_SIGNED_LENGTH {
        return Err(GraphError::InvalidCount(
            usize::try_from(row_count).unwrap_or(usize::MAX),
        ));
    }

    let maintained = maintain_ordered_map(
        domain,
        root,
        root_node,
        &changes,
        &mut *read_node,
        &mut |node| store.write_node(node),
    )?;
    if maintained.row_count != row_count {
        return Err(GraphError::InvalidCount(
            usize::try_from(maintained.row_count).unwrap_or(usize::MAX),
        ));
    }
    Ok(RowMapPass {
        maintained,
        row_count,
        logical_rekeys,
    })
}

fn collect_ordered_range_with(
    read_node: &mut impl FnMut(&NativeOid) -> Result<NodeData, GraphError>,
    node: NodeData,
    expected_domain: &[u8],
    expected_height: Option<u8>,
    lower_exclusive: Option<&crate::row_store::TypedKey>,
    lower_inclusive: Option<&crate::row_store::TypedKey>,
    resume_after: Option<&crate::row_store::TypedKey>,
    upper_exclusive: Option<&crate::row_store::TypedKey>,
    limit: usize,
    output: &mut Vec<OrderedLeafEntry>,
) -> Result<(), GraphError> {
    if output.len() >= limit {
        return Ok(());
    }
    match node {
        NodeData::OrderedLeaf { domain, entries } => {
            if domain != expected_domain || expected_height.is_some_and(|height| height != 0) {
                return Err(GraphError::InvalidDomain);
            }
            let mut previous: Option<crate::row_store::TypedKey> = None;
            for entry in entries {
                let key = crate::row_store::TypedKey::decode_canonical(&entry.key)
                    .map_err(|_| GraphError::NonCanonicalData)?;
                if previous.as_ref().is_some_and(|last| last >= &key)
                    || lower_exclusive.is_some_and(|lower| key <= *lower)
                {
                    return Err(GraphError::NonCanonicalData);
                }
                previous = Some(key.clone());
                if resume_after.is_some_and(|after| key <= *after) {
                    continue;
                }
                if lower_inclusive.is_some_and(|lower| key < *lower) {
                    continue;
                }
                if upper_exclusive.is_some_and(|upper| key >= *upper) {
                    break;
                }
                output.push(entry);
                if output.len() >= limit {
                    break;
                }
            }
            Ok(())
        }
        NodeData::OrderedBranch {
            domain,
            height,
            entries,
        } => {
            if domain != expected_domain
                || height == 0
                || expected_height.is_some_and(|expected| expected != height)
                || entries.is_empty()
            {
                return Err(GraphError::InvalidDomain);
            }
            let mut total_count = 0u64;
            let mut previous_fence: Option<crate::row_store::TypedKey> = None;
            for entry in entries {
                total_count = total_count
                    .checked_add(entry.row_count)
                    .filter(|count| *count <= MAX_SIGNED_LENGTH)
                    .ok_or(GraphError::InvalidCount(usize::MAX))?;
                let fence = crate::row_store::TypedKey::decode_canonical(&entry.inclusive_max_key)
                    .map_err(|_| GraphError::NonCanonicalData)?;
                if previous_fence
                    .as_ref()
                    .is_some_and(|previous| previous >= &fence)
                {
                    return Err(GraphError::NonCanonicalData);
                }
                // A fence is the child's own inclusive maximum key, so a child
                // whose fence sorts below the lower bound spans no key in the
                // interval and is left unread. Children are ordered by fence,
                // so the first child whose own minimum reaches the exclusive
                // upper bound ends the range: it and every later child lie at
                // or above it. That child is still read and validated, so the
                // upper bound keeps its exact meaning, and every child the
                // range does descend into is fully validated below. A bounded
                // range therefore reads the pages that overlap its interval
                // rather than the whole index.
                // A resume cursor skips a child whose fence is at or below it,
                // for the same reason a lower bound does: no key it holds can
                // order after the cursor.
                let within_lower = lower_inclusive.is_none_or(|lower| fence >= *lower)
                    && resume_after.is_none_or(|after| fence > *after);
                let mut past_upper = false;
                if within_lower {
                    let child = read_node(&entry.child)?;
                    let child_bounds = ordered_node_bounds_with(
                        read_node,
                        child.clone(),
                        expected_domain,
                        Some(height - 1),
                    )?;
                    if child_bounds.0 != entry.row_count
                        || child_bounds.2 != fence
                        || previous_fence
                            .as_ref()
                            .is_some_and(|previous| child_bounds.1 <= *previous)
                    {
                        return Err(GraphError::InvalidCount(
                            usize::try_from(child_bounds.0).unwrap_or(usize::MAX),
                        ));
                    }
                    if upper_exclusive.is_some_and(|upper| child_bounds.1 >= *upper) {
                        past_upper = true;
                    } else {
                        collect_ordered_range_with(
                            read_node,
                            child,
                            expected_domain,
                            Some(height - 1),
                            previous_fence.as_ref(),
                            lower_inclusive,
                            resume_after,
                            upper_exclusive,
                            limit,
                            output,
                        )?;
                    }
                }
                previous_fence = Some(fence);
                if past_upper || output.len() >= limit {
                    break;
                }
            }
            if total_count > MAX_SIGNED_LENGTH {
                return Err(GraphError::InvalidCount(usize::MAX));
            }
            Ok(())
        }
        other => Err(GraphError::WrongNodeKind {
            expected: NodeKind::OrderedLeaf,
            actual: other.kind(),
        }),
    }
}

#[cfg(test)]
mod persisted_orp_tests {
    use super::*;
    use crate::row_store::{RowValue, TypedKey};
    use std::future::Future;

    fn block_on<F: Future>(future: F) -> F::Output {
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        let mut future = std::pin::pin!(future);
        loop {
            match future.as_mut().poll(&mut context) {
                std::task::Poll::Ready(output) => return output,
                std::task::Poll::Pending => std::thread::yield_now(),
            }
        }
    }

    /// The revision record parser: a commit that carries no migration marker is
    /// not the same as one that carries an empty value, and the author field is
    /// never confused with the trailer field.
    #[test]
    fn revision_records_separate_the_author_from_the_migration_marker() {
        assert_eq!(
            split_revision_record("Ada <ada@example.test>\tformat-1-to-3"),
            ("Ada <ada@example.test>", Some("format-1-to-3".to_owned()))
        );
        // An ordinary commit renders an empty trailer value, which is no marker.
        assert_eq!(
            split_revision_record("Ada <ada@example.test>\t"),
            ("Ada <ada@example.test>", None)
        );
        // A record with no trailer field at all is still an author.
        assert_eq!(
            split_revision_record("Ada <ada@example.test>"),
            ("Ada <ada@example.test>", None)
        );
        // An author name containing spaces survives intact.
        assert_eq!(
            split_revision_record("Ada Lovelace <ada@example.test>\tformat-2-to-3"),
            (
                "Ada Lovelace <ada@example.test>",
                Some("format-2-to-3".to_owned())
            )
        );
    }

    fn fixture_git_object(directory: &Path, kind: &str, content: &[u8]) -> NativeOid {
        let mut child = Command::new("git")
            .current_dir(directory)
            .args(["hash-object", "-w", "-t", kind, "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn fixture hash-object");
        child
            .stdin
            .take()
            .expect("hash-object stdin")
            .write_all(content)
            .expect("write fixture object");
        let output = child.wait_with_output().expect("wait for hash-object");
        assert!(
            output.status.success(),
            "git hash-object failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        NativeOid::from_hex(
            GitHashAlgorithm::Sha1,
            std::str::from_utf8(&output.stdout)
                .expect("hash-object prints UTF-8")
                .trim(),
        )
        .expect("fixture object ID")
    }

    fn fixture_git_node(directory: &Path, node: &NodeData) -> NativeOid {
        let envelope = NativeNodeEnvelope::from_node(node).expect("canonical fixture node");
        let data = fixture_git_object(directory, "blob", &envelope.data);
        let mut entries = vec![ParsedGitTreeEntry {
            mode: b"100644".to_vec(),
            name: b"data".to_vec(),
            oid: data,
        }];
        if !envelope.refs.is_empty() {
            let refs = envelope
                .refs
                .into_iter()
                .map(|reference| ParsedGitTreeEntry {
                    mode: match reference.kind {
                        NativeObjectKind::Blob => b"100644".to_vec(),
                        NativeObjectKind::Tree => b"40000".to_vec(),
                    },
                    name: reference.oid.to_hex().into_bytes(),
                    oid: reference.oid,
                })
                .collect();
            let refs = encode_git_tree(refs).expect("canonical refs tree");
            entries.push(ParsedGitTreeEntry {
                mode: b"40000".to_vec(),
                name: b"refs".to_vec(),
                oid: fixture_git_object(directory, "tree", &refs),
            });
        }
        let tree = encode_git_tree(entries).expect("canonical node tree");
        fixture_git_object(directory, "tree", &tree)
    }

    fn fixture_schema_node(
        directory: &Path,
        relation_id: [u8; 16],
        include_blob_field: bool,
    ) -> (NativeOid, [u8; 32]) {
        // Each schema descriptor has its own relation identity and field layout.
        let uuid = |id: [u8; 16]| Raw::Tag(37, Box::new(Raw::Bytes(id.to_vec())));
        let field = |id: [u8; 16], name: &str, ty: Raw, role: u64| {
            Raw::Array(vec![
                uuid(id),
                Raw::Text(name.to_owned()),
                ty,
                Raw::Int(role.into()),
                Raw::Array(vec![Raw::Int(0.into())]),
            ])
        };
        let mut fields = vec![field(
            [0x11; 16],
            "id",
            Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Str".to_owned())]),
            0,
        )];
        if include_blob_field {
            fields.push(field(
                [0x22; 16],
                "payload",
                Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Blob".to_owned())]),
                1,
            ));
        }
        let descriptor = SchemaDescriptor::new(Raw::Map(vec![
            (Raw::Int(0.into()), Raw::Int(1.into())),
            (Raw::Int(1.into()), uuid(relation_id)),
            (Raw::Int(2.into()), Raw::Array(vec![uuid([0x11; 16])])),
            (Raw::Int(3.into()), Raw::Array(fields)),
            (Raw::Int(4.into()), Raw::Array(Vec::new())),
        ]))
        .expect("valid fixture schema descriptor");
        fixture_descriptor_node(directory, &descriptor)
    }

    /// Writes `descriptor` as a kind-6 schema node over a one-chunk byte index.
    fn fixture_descriptor_node(
        directory: &Path,
        descriptor: &SchemaDescriptor,
    ) -> (NativeOid, [u8; 32]) {
        let schema_bytes = descriptor.encode().expect("canonical fixture schema bytes");
        let schema_digest = schema_identity_v3(&schema_bytes);
        let chunk = fixture_git_object(directory, "blob", &schema_bytes);
        let byte_root = fixture_git_node(
            directory,
            &NodeData::ByteIndex {
                height: 0,
                total_length: schema_bytes.len() as u64,
                entries: vec![ByteIndexEntry {
                    span_length: schema_bytes.len() as u64,
                    target: chunk,
                    chunk_sha256: Some(Sha256::digest(&schema_bytes).into()),
                }],
            },
        );
        let schema_oid = fixture_git_node(
            directory,
            &NodeData::Schema {
                encoded_length: schema_bytes.len() as u64,
                schema_digest,
                byte_root,
            },
        );
        (schema_oid, schema_digest)
    }

    /// A repeated walk down the schema node path is answered from the node
    /// cache: the second walk returns the same descriptor and records a hit
    /// for every node it reads, with no further repository reads needed.
    #[test]
    fn a_repeated_path_walk_hits_the_node_cache() {
        let (_directory, graph) = capture_test_context();
        let scope = graph.open_read_scope().expect("schema scope");
        let first = graph
            .read_schema_descriptor(&scope)
            .expect("the issued graph verified its schema node");
        let hits_before = graph.node_cache_hits();
        let second = graph
            .read_schema_descriptor(&scope)
            .expect("a repeated walk reads the cached schema node");
        assert_eq!(
            first.encode().expect("canonical descriptor bytes"),
            second.encode().expect("canonical descriptor bytes")
        );
        assert!(
            graph.node_cache_hits() > hits_before,
            "the repeated walk must be answered from the node cache"
        );
    }

    /// A cache hit is admitted exactly like a miss. Both charge the blob's
    /// metadata to the same quota, so a repeated read cannot pass a budget that
    /// its first read exhausted.
    #[test]
    fn a_cache_hit_is_charged_metadata_like_a_miss() {
        let (directory, graph) = capture_test_context();
        let content = b"quota charged on hit and miss\n";
        let hex = write_blob_for_digest_test(directory.path(), content);
        let oid = NativeOid::from_hex(graph.algorithm, &hex).expect("object id");
        let scope = graph.open_read_scope().expect("read scope");
        let size = content.len() as u64;
        let objects_used = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let metadata_used = Arc::new(std::sync::atomic::AtomicU64::new(0));
        // The quota admits exactly one read's metadata, shared by both reads.
        let mut first = ObjectBudget::new(
            16,
            16,
            Arc::clone(&objects_used),
            size,
            Arc::clone(&metadata_used),
        );
        assert_eq!(
            graph
                .read_git_object(NativeObjectKind::Blob, &oid, 1024, &scope, &mut first)
                .expect("the miss is admitted"),
            content
        );
        let hits_before = graph.node_cache_hits();
        let mut second = ObjectBudget::new(16, 16, objects_used, size, metadata_used);
        assert!(matches!(
            graph.read_git_object(NativeObjectKind::Blob, &oid, 1024, &scope, &mut second),
            Err(GraphError::MetadataQuotaExceeded)
        ));
        assert!(
            graph.node_cache_hits() > hits_before,
            "the second read must be the cache hit that is charged"
        );
    }

    /// A loose object whose bytes were replaced by another object's still
    /// decodes, so the digest check is the only guard before the caller sees
    /// the bytes. The batch path must refuse them.
    #[test]
    fn a_batch_read_whose_bytes_miss_the_requested_digest_fails_closed() {
        let (directory, _graph) = capture_test_context();
        let root = directory.path();
        let requested = write_blob_for_digest_test(root, b"aaaa\n");
        let other = write_blob_for_digest_test(root, b"bbbb\n");
        let loose = |hex: &str| root.join(".git/objects").join(&hex[..2]).join(&hex[2..]);
        std::fs::remove_file(loose(&requested)).expect("remove the loose object");
        std::fs::copy(loose(&other), loose(&requested)).expect("swap in the other object");
        let repository = crate::Repository::discover(root).expect("open repository");
        let requested = NativeOid::from_hex(GitHashAlgorithm::Sha1, &requested).expect("object id");
        assert!(matches!(
            read_checked_object(
                &repository,
                GitHashAlgorithm::Sha1,
                NativeObjectKind::Blob,
                &requested,
                |_| Ok(()),
            ),
            Err(GraphError::GitObjectHashMismatch)
        ));
    }

    /// Writes one blob with git and returns its object id as hex.
    fn write_blob_for_digest_test(root: &std::path::Path, content: &[u8]) -> String {
        use std::io::Write as _;
        let mut child = std::process::Command::new("git")
            .args(["hash-object", "-w", "--stdin"])
            .current_dir(root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn git hash-object");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(content)
            .expect("write blob");
        String::from_utf8(child.wait_with_output().expect("hash-object").stdout)
            .expect("utf-8 object id")
            .trim()
            .to_owned()
    }

    fn capture_test_context() -> (tempfile::TempDir, NativeGraphContext) {
        let directory = tempfile::tempdir().expect("create capture repository");
        let graph = capture_context_in(directory.path());
        (directory, graph)
    }
    fn capture_test_context_with_blob_field() -> (tempfile::TempDir, NativeGraphContext) {
        let directory = tempfile::tempdir().expect("create capture repository");
        let graph = capture_context_in_with_blob_field(directory.path(), true);
        (directory, graph)
    }

    /// G2: the frozen-candidate read-tree of `expected_head` must be preceded
    /// by the bounded expansion pre-pass, so a too-deep head is refused.
    #[test]
    fn frozen_candidate_refuses_a_head_tree_nested_beyond_the_walk_limit() {
        let (directory, graph) = capture_test_context();
        crate::test_support::configure_fixture_git_identity(directory.path());
        let deep = crate::test_support::deep_nested_commit(directory.path(), 300);
        crate::test_support::git_with_input(
            directory.path(),
            &["update-ref", "HEAD", &deep],
            "",
        );
        let repository = crate::Repository::discover(directory.path()).expect("open repository");
        let head = repository.head().expect("read head").expect("born head");
        assert!(matches!(
            repository.build_frozen_publication_candidate(
                &head,
                &graph,
                graph.store_root(),
                "orna: test deep head",
            ),
            Err(crate::RepositoryError::SourceTreeLimit)
        ));
    }

    /// Builds the capture fixture context for a repository path, creating the
    /// repository if it does not exist yet. Every fixture object is
    /// content-addressed and the owner identity is fixed, so the same path
    /// yields the same context in a killed child and in the restarting parent
    /// that reclaims its pin namespace.
    fn capture_context_in(root: &Path) -> NativeGraphContext {
        capture_context_in_with_blob_field(root, false)
    }

    fn capture_context_in_with_blob_field(
        root: &Path,
        include_blob_field: bool,
    ) -> NativeGraphContext {
        init_fixture_repository(root);
        let relation_id = [0x32; 16];
        // The schema descriptor binds relation identity and its complete field layout
        // (Orna 1.1.0 Section 29).
        let (schema_oid, schema_digest) =
            fixture_schema_node(root, relation_id, include_blob_field);
        fixture_relation_context(root, relation_id, schema_oid, schema_digest)
    }

    fn init_fixture_repository(root: &Path) {
        let init = Command::new("git")
            .current_dir(root)
            .args(["init", "--quiet", "--template="])
            .status()
            .expect("initialize capture repository");
        assert!(init.success());
    }

    /// Issues the graph context of a fixture store whose one relation has an
    /// empty row map under the given schema node.
    fn fixture_relation_context(
        root: &Path,
        relation_id: [u8; 16],
        schema_oid: NativeOid,
        schema_digest: [u8; 32],
    ) -> NativeGraphContext {
        let algorithm = GitHashAlgorithm::Sha1;
        let database_id = [0x31; 16];
        let primary_root = fixture_git_node(
            root,
            &NodeData::OrderedLeaf {
                domain: rows_domain(relation_id, schema_digest),
                entries: Vec::new(),
            },
        );
        let relation_domain = canonical_value_bytes(&CborValue::Array(vec![
            CborValue::Text("relations".to_owned()),
            CborValue::Bytes(database_id.to_vec()),
        ]));
        let relation_key = TypedKey::Bytes(relation_id.to_vec())
            .canonical_bytes()
            .expect("canonical relation key");
        let relation_value = canonical_value_bytes(&CborValue::Array(vec![
            CborValue::Bytes(schema_oid.as_bytes().to_vec()),
            CborValue::Bytes(primary_root.as_bytes().to_vec()),
            CborValue::Null,
            CborValue::Unsigned(0),
        ]));
        let relation_map = fixture_git_node(
            root,
            &NodeData::OrderedLeaf {
                domain: relation_domain,
                entries: vec![OrderedLeafEntry {
                    key: relation_key,
                    value: relation_value,
                }],
            },
        );
        let store_root = fixture_git_node(root, &NodeData::StoreRoot { relation_map });
        let snapshot_id = [0x38; 32];
        let generation = u64::from_be_bytes(
            snapshot_id[..8]
                .try_into()
                .expect("snapshot generation has eight bytes"),
        );
        let schema_generation = crate::row_store::SchemaGeneration::issue(
            database_id,
            relation_id,
            schema_oid,
            schema_digest,
            generation,
        );
        let version = crate::row_store::RowMapVersion::issue(
            database_id,
            relation_id,
            store_root.clone(),
            schema_generation,
            primary_root,
            generation,
            Some(0),
        )
        .expect("consistent fixture row-map version");
        let snapshot = crate::row_store::RowMapSnapshot::issue(version, [0x34; 32], Vec::new())
            .expect("sealed empty fixture row map");
        let repository = crate::Repository::discover(root).expect("discover capture repository");
        let graph = NativeGraphContext::issue(
            repository,
            [0x35; 32],
            [0x36; 32],
            database_id,
            [0x37; 16],
            snapshot_id,
            algorithm,
            store_root,
            schema_digest,
            snapshot,
        )
        .expect("issue graph context after verifying fixture schema");
        graph
    }

    #[test]
    fn load_format3_row_map_keeps_secondary_root_opaque() {
        let (directory, graph) = capture_test_context();
        let version = graph.row_snapshot.version();
        let relation_id = *version.relation_id();
        let database_id = *version.database_id();
        let algorithm = graph.algorithm;
        let descriptor = SchemaDescriptor::new(Raw::Map(vec![
            (Raw::Int(0.into()), Raw::Int(1.into())),
            (
                Raw::Int(1.into()),
                Raw::Tag(37, Box::new(Raw::Bytes(relation_id.to_vec()))),
            ),
            (Raw::Int(2.into()), Raw::Array(Vec::new())),
            (Raw::Int(3.into()), Raw::Array(Vec::new())),
            (Raw::Int(4.into()), Raw::Array(Vec::new())),
        ]))
        .expect("valid relation schema descriptor");
        let schema_bytes = descriptor.encode().expect("canonical schema bytes");
        let schema_digest = schema_identity_v3(&schema_bytes);
        let schema_chunk = fixture_git_object(directory.path(), "blob", &schema_bytes);
        let schema_byte_root = fixture_git_node(
            directory.path(),
            &NodeData::ByteIndex {
                height: 0,
                total_length: schema_bytes.len() as u64,
                entries: vec![ByteIndexEntry {
                    span_length: schema_bytes.len() as u64,
                    target: schema_chunk,
                    chunk_sha256: Some(Sha256::digest(&schema_bytes).into()),
                }],
            },
        );
        let schema_root = fixture_git_node(
            directory.path(),
            &NodeData::Schema {
                encoded_length: schema_bytes.len() as u64,
                schema_digest,
                byte_root: schema_byte_root.clone(),
            },
        );
        let primary_root = fixture_git_node(
            directory.path(),
            &NodeData::OrderedLeaf {
                domain: rows_domain(relation_id, schema_digest),
                entries: Vec::new(),
            },
        );
        // This schema node is distinct from the selected schema and is not a
        // secondary map. The primary-only loader must leave its OID opaque.
        let secondary_root = fixture_git_node(
            directory.path(),
            &NodeData::Schema {
                encoded_length: 0,
                schema_digest,
                byte_root: schema_byte_root,
            },
        );

        let relations_domain = canonical_domain(vec![
            CborValue::Text("relations".to_owned()),
            CborValue::Bytes(database_id.to_vec()),
        ]);
        let relation_map = fixture_git_node(
            directory.path(),
            &NodeData::OrderedLeaf {
                domain: relations_domain,
                entries: vec![OrderedLeafEntry {
                    key: TypedKey::Bytes(relation_id.to_vec())
                        .canonical_bytes()
                        .expect("canonical relation key"),
                    value: canonical_value_bytes(&CborValue::Array(vec![
                        CborValue::Bytes(schema_root.as_bytes().to_vec()),
                        CborValue::Bytes(primary_root.as_bytes().to_vec()),
                        CborValue::Bytes(secondary_root.as_bytes().to_vec()),
                        CborValue::Unsigned(0),
                    ])),
                }],
            },
        );
        let store_root = fixture_git_node(
            directory.path(),
            &NodeData::StoreRoot { relation_map },
        );

        let loaded = load_format3_row_map(
            &graph.repository,
            algorithm,
            store_root,
            database_id,
            graph.snapshot_id,
            relation_id,
        )
        .expect("load the primary map without traversing the secondary root");
        assert_eq!(loaded.version().primary_root(), &primary_root);
    }

    #[test]
    fn schema_relation_identity_is_read_from_descriptor() {
        let relation_id = [0x42; 16];
        let descriptor = SchemaDescriptor::new(Raw::Map(vec![
            (Raw::Int(0.into()), Raw::Int(1.into())),
            (
                Raw::Int(1.into()),
                Raw::Tag(37, Box::new(Raw::Bytes(relation_id.to_vec()))),
            ),
            (Raw::Int(2.into()), Raw::Array(Vec::new())),
            (Raw::Int(3.into()), Raw::Array(Vec::new())),
            (Raw::Int(4.into()), Raw::Array(Vec::new())),
        ]))
        .expect("valid relation schema descriptor");

        assert_eq!(
            schema_relation_id(&descriptor).expect("extract relation identity"),
            relation_id
        );
    }

    fn fixture_ref_exists(directory: &Path, reference: &str) -> bool {
        Command::new("git")
            .current_dir(directory)
            .args(["show-ref", "--verify", "--quiet", reference])
            .status()
            .expect("inspect fixture ref")
            .success()
    }

    fn pending_refs(directory: &Path) -> Vec<u8> {
        let output = Command::new("git")
            .current_dir(directory)
            .args([
                "for-each-ref",
                "--format=%(refname)",
                "refs/orna/pins",
            ])
            .output()
            .expect("list pending refs");
        assert!(output.status.success());
        output
            .stdout
            .split(|byte| *byte == b'\n')
            .filter(|reference| {
                reference
                    .windows(b"/scratch/".len())
                    .any(|part| part == b"/scratch/")
            })
            .flat_map(|reference| reference.iter().copied().chain(std::iter::once(b'\n')))
            .collect()
    }

    fn rows_domain(relation: [u8; 16], schema: [u8; 32]) -> Vec<u8> {
        let mut domain = vec![0x83, 0x64];
        domain.extend_from_slice(b"rows");
        bytes(&mut domain, &relation);
        bytes(&mut domain, &schema);
        domain
    }

    fn persisted_row_leaf(value: Vec<u8>) -> NodeData {
        NodeData::OrderedLeaf {
            domain: rows_domain([0x22; 16], [0x33; 32]),
            entries: vec![OrderedLeafEntry {
                key: vec![0x01],
                value,
            }],
        }
    }

    #[test]
    fn persisted_orp_row_leaf_decodes_and_rejects_malformed_or_unsupported_values() {
        let algorithm = GitHashAlgorithm::Sha1;
        let fields = vec![0x81, 0x01];
        let node = persisted_row_leaf(fields.clone());
        let envelope = NativeNodeEnvelope::from_node(&node).expect("canonical ORP leaf");
        NodeData::decode_canonical(&envelope.data, algorithm)
            .expect("canonical ORP node data round-trips");
        let decoded = validate_native_node(&ValidatedRepositoryContext { algorithm }, &envelope)
            .expect("persisted row leaf validates");
        assert_eq!(decoded, node);

        let NodeData::OrderedLeaf { entries, .. } = decoded else {
            panic!("ordered row leaf expected");
        };
        assert_eq!(
            TypedKey::decode_canonical(&entries[0].key).unwrap(),
            TypedKey::UInt(1)
        );
        assert_eq!(
            RowValue::decode_canonical_fields(entries[0].value.clone()).unwrap(),
            RowValue::inline(fields, Vec::new()).unwrap()
        );

        let NodeData::OrderedLeaf { domain, .. } = &node else {
            unreachable!();
        };
        let mut malformed = Vec::new();
        array(&mut malformed, 4);
        uint(&mut malformed, 1);
        uint(&mut malformed, NodeKind::OrderedLeaf.number().into());
        malformed.extend_from_slice(domain);
        array(&mut malformed, 1);
        array(&mut malformed, 2);
        malformed.push(0x01);
        malformed.extend_from_slice(&[0x81, 0x18, 0x01]);
        assert!(matches!(
            NodeData::decode_canonical(&malformed, algorithm),
            Err(GraphError::NonCanonicalData)
        ));

        let descriptor_oid = NativeObjectId::new(algorithm, &[0x44; 20]).unwrap();
        let mut blob_reference = Vec::new();
        cbor_head(&mut blob_reference, 6, 60111);
        array(&mut blob_reference, 5);
        uint(&mut blob_reference, 1);
        bytes(&mut blob_reference, &[0x55; 32]);
        blob_reference.push(0x6A);
        blob_reference.extend_from_slice(b"text/plain");
        null(&mut blob_reference);
        bytes(&mut blob_reference, descriptor_oid.as_bytes());
        let mut blob_fields = vec![0x81];
        blob_fields.extend_from_slice(&blob_reference);
        let blob_node = persisted_row_leaf(blob_fields);
        let mut blob_envelope = NativeNodeEnvelope::from_node(&blob_node).unwrap();
        blob_envelope.refs[0] = NativeRefEntry::new(descriptor_oid, NativeObjectKind::Blob);
        assert!(matches!(
            validate_native_node(&ValidatedRepositoryContext { algorithm }, &blob_envelope),
            Err(GraphError::WrongReferenceKind)
        ));

        let mut malformed_oid_value = Vec::new();
        cbor_head(&mut malformed_oid_value, 6, 60111);
        array(&mut malformed_oid_value, 5);
        uint(&mut malformed_oid_value, 1);
        bytes(&mut malformed_oid_value, &[0x55; 32]);
        malformed_oid_value.push(0x6A);
        malformed_oid_value.extend_from_slice(b"text/plain");
        null(&mut malformed_oid_value);
        bytes(&mut malformed_oid_value, &[0x66; 19]);
        let mut malformed_oid_fields = vec![0x81];
        malformed_oid_fields.extend_from_slice(&malformed_oid_value);
        assert!(matches!(
            persisted_row_leaf(malformed_oid_fields).dependencies(),
            Err(GraphError::InvalidOidWidth { .. })
        ));

        let overflow_oid = NativeObjectId::new(algorithm, &[0x77; 20]).unwrap();
        let mut overflow_value = Vec::new();
        cbor_head(&mut overflow_value, 6, 60113);
        array(&mut overflow_value, 1);
        bytes(&mut overflow_value, overflow_oid.as_bytes());
        let overflow_node = persisted_row_leaf(overflow_value.clone());
        let overflow_envelope = NativeNodeEnvelope::from_node(&overflow_node).unwrap();
        validate_native_node(
            &ValidatedRepositoryContext { algorithm },
            &overflow_envelope,
        )
        .expect("OGS accepts a well-formed overflow edge");
        let mut missing_edge = overflow_envelope.clone();
        missing_edge.refs.clear();
        assert!(matches!(
            validate_native_node(&ValidatedRepositoryContext { algorithm }, &missing_edge),
            Err(GraphError::MissingReference)
        ));

        let mut over_fanout = Vec::with_capacity(MAX_REFS + 1);
        for index in 0..=MAX_REFS {
            let mut bytes = [0u8; 20];
            bytes[16..].copy_from_slice(&(index as u32).to_be_bytes());
            over_fanout.push(NativeObjectId::new(algorithm, &bytes).unwrap());
        }
        assert!(matches!(
            NodeData::DependencyIndex {
                height: 0,
                children: over_fanout,
            }
            .encode_canonical(),
            Err(GraphError::FanoutExceeded(count)) if count == MAX_REFS + 1
        ));
        assert!(RowValue::decode_canonical_fields(overflow_value).is_err());
    }

    #[test]
    fn orp_orders_leaf_keys_and_branch_fences_by_typed_value_not_cbor_bytes() {
        let domain = rows_domain([0x22; 16], [0x33; 32]);
        let boolean = TypedKey::Bool(false).canonical_bytes().unwrap();
        let bytes = TypedKey::Bytes(vec![0]).canonical_bytes().unwrap();
        assert!(
            boolean > bytes,
            "fixture encodings must oppose logical order"
        );
        let leaf = NodeData::OrderedLeaf {
            domain: domain.clone(),
            entries: vec![
                OrderedLeafEntry {
                    key: boolean.clone(),
                    value: vec![0x80],
                },
                OrderedLeafEntry {
                    key: bytes.clone(),
                    value: vec![0x80],
                },
            ],
        };
        assert!(leaf.encode_canonical().is_ok());
        let reversed = NodeData::OrderedLeaf {
            domain: domain.clone(),
            entries: vec![
                OrderedLeafEntry {
                    key: bytes.clone(),
                    value: vec![0x80],
                },
                OrderedLeafEntry {
                    key: boolean.clone(),
                    value: vec![0x80],
                },
            ],
        };
        assert!(matches!(
            reversed.encode_canonical(),
            Err(GraphError::NonCanonicalData)
        ));

        let first = NativeObjectId::new(GitHashAlgorithm::Sha1, &[0x41; 20]).unwrap();
        let second = NativeObjectId::new(GitHashAlgorithm::Sha1, &[0x42; 20]).unwrap();
        let branch = NodeData::OrderedBranch {
            domain: domain.clone(),
            height: 1,
            entries: vec![
                OrderedBranchEntry {
                    inclusive_max_key: boolean.clone(),
                    child: first.clone(),
                    row_count: 5_000_000,
                },
                OrderedBranchEntry {
                    inclusive_max_key: bytes.clone(),
                    child: second.clone(),
                    row_count: 5_000_000,
                },
            ],
        };
        assert!(branch.encode_canonical().is_ok());
        let reversed_fences = NodeData::OrderedBranch {
            domain: domain.clone(),
            height: 1,
            entries: vec![
                OrderedBranchEntry {
                    inclusive_max_key: bytes,
                    child: first.clone(),
                    row_count: 1,
                },
                OrderedBranchEntry {
                    inclusive_max_key: boolean,
                    child: second.clone(),
                    row_count: 1,
                },
            ],
        };
        assert!(matches!(
            reversed_fences.encode_canonical(),
            Err(GraphError::NonCanonicalData)
        ));
        assert!(validate_ordered_root_node(&branch, &domain, 10_000_000).is_ok());
    }

    #[test]
    fn orp_rejects_unrecognized_or_malformed_domain_tuples() {
        let mut unknown = vec![0x82, 0x61, b'x', 0x50];
        unknown.extend_from_slice(&[0; 16]);
        let mut malformed_rows = vec![0x82, 0x64, b'r', b'o', b'w', b's', 0x50];
        malformed_rows.extend_from_slice(&[0; 16]);
        for domain in [unknown, malformed_rows] {
            assert!(matches!(
                NodeData::OrderedLeaf {
                    domain,
                    entries: Vec::new(),
                }
                .encode_canonical(),
                Err(GraphError::InvalidDomain)
            ));
        }
    }

/// ORNA-CAP-001: newly captured row content is verified and protected by a durable native ref before acknowledgement.
    #[test]
    fn captured_blob_candidate_is_promoted_to_a_durable_pin() {
        let (directory, graph) = capture_test_context();
        let scope = graph.open_read_scope().expect("owner read scope");
        let payload = b"captured Blob bytes";
        let candidate = graph
            .capture_blob_candidate(&payload[..], payload.len() as u64, &scope)
            .expect("capture authorized stream");
        let pin_id = candidate.pin_id;
        let pending_ref = scratch_content_ref(&graph.owner_id, &pin_id);

        assert!(fixture_ref_exists(directory.path(), &pending_ref));
        let pin = graph
            .protect_captured_blob(candidate, &scope)
            .expect("verify and accept captured candidate");

        assert_eq!(
            pin.content_identity(),
            crate::blob_store::digest_bytes(payload)
        );
        assert_eq!(
            pin.protected_ref(),
            accepted_content_ref(&graph.owner_id, &pin_id)
        );
        assert_eq!(*pin.transfer_record().owner_id(), graph.owner_id);
        assert!(fixture_ref_exists(directory.path(), pin.protected_ref()));
        assert!(!fixture_ref_exists(directory.path(), &pending_ref));
    }

    /// A byte-identical copy of the Orna 1.1.0 release `tests/final-vectors.json`,
    /// pinned to its release `SHA256SUMS` value so drift from the publication
    /// fails here.
    const FINAL_VECTORS_JSON: &str =
        include_str!("../tests/fixtures/reference/tests/final-vectors.json");
    const FINAL_VECTORS_SHA256: &str =
        "985140787cde755ae53034de28d46cb4f5696d8bc95e385876e95ec0c953899d";

    fn vector_hex(value: &serde_json::Value) -> Vec<u8> {
        let text = value.as_str().expect("a hexadecimal vector field");
        (0..text.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&text[at..at + 2], 16).expect("hexadecimal digits"))
            .collect()
    }

    /// Reads one object straight from the fixture repository's Git store.
    fn stored_git_object(directory: &Path, oid: &str) -> (String, Vec<u8>) {
        let kind = Command::new("git")
            .current_dir(directory)
            .args(["cat-file", "-t", oid])
            .output()
            .expect("read the stored object kind");
        assert!(kind.status.success(), "object {oid} is not stored");
        let kind = String::from_utf8(kind.stdout)
            .expect("object kinds are UTF-8")
            .trim()
            .to_owned();
        let bytes = Command::new("git")
            .current_dir(directory)
            .args(["cat-file", &kind, oid])
            .output()
            .expect("read the stored object bytes");
        assert!(bytes.status.success(), "object {oid} is unreadable");
        (kind, bytes.stdout)
    }

    /// Orna 1.1.0 `tests/final-vectors.json` `native_graph`: capturing the
    /// published three-byte payload into a sha1 graph and accepting it with
    /// the `rov3-text` annotation writes exactly the seven published OGS-1
    /// objects, roots them at the published descriptor OID and binds the exact
    /// `rov3-text` value. The release authors the vector with
    /// `tools/write_vectors.py` over `tools/final_model.py` `store_blob`.
    #[test]
    fn capture_reproduces_the_final_vector_native_graph() {
        assert_eq!(
            hex_encode(&Sha256::digest(FINAL_VECTORS_JSON.as_bytes())),
            FINAL_VECTORS_SHA256,
            "the vendored final vectors no longer match the release"
        );
        let vectors: serde_json::Value =
            serde_json::from_str(FINAL_VECTORS_JSON).expect("the vendored final vectors are JSON");
        let expected = &vectors["native_graph"];
        assert_eq!(expected["algorithm"], "sha1");
        let rov3 = vectors["values"]
            .as_array()
            .expect("value vectors")
            .iter()
            .find(|case| case["id"] == "rov3-text")
            .expect("the rov3-text vector");
        let annotation = rov3["value"]["value"].as_array().expect("rov3-text fields");

        // The generator captures `abc`; the vector records its length and digest.
        let payload = b"abc";
        assert_eq!(expected["blob_length"], payload.len());
        assert_eq!(expected["blob_sha256"], hex_encode(&Sha256::digest(payload)));

        let (directory, graph) = capture_test_context();
        assert_eq!(graph.algorithm, GitHashAlgorithm::Sha1);
        let graph = Arc::new(graph);
        let scope = graph.open_read_scope().expect("owner read scope");
        let capability = RepositoryCaptureCapability::new(Arc::clone(&graph), scope)
            .expect("matching graph and owner scope");
        let candidate = capability
            .capture_blob_candidate(&payload[..], payload.len() as u64)
            .expect("capture the published payload");
        let binding = capability
            .accept_captured_blob(
                candidate,
                annotation[2].as_str().expect("a media type"),
                annotation[3].as_str(),
            )
            .expect("accept with the published annotation");
        assert_eq!(
            binding.descriptor_oid().to_hex(),
            expected["descriptor_oid"].as_str().expect("a descriptor OID")
        );
        assert_eq!(
            binding.encoded_value(),
            vector_hex(&rov3["canonical_hex"]),
            "rov3-text"
        );

        // The verified closure under the descriptor is exactly the published
        // object set, and Git stores each object with its published kind and
        // bytes.
        let pin = binding.into_pin();
        let reference = AdmittedBlobReference {
            context: pin.context,
            database_id: pin.database_id,
            owner_id: pin.owner_id,
            snapshot_id: pin.snapshot_id,
            descriptor_oid: pin.descriptor_oid.clone(),
            identity: pin.identity,
        };
        let closure_scope = graph.open_read_scope().expect("closure verification scope");
        let closure = graph
            .verify_full_blob(&reference, &closure_scope)
            .expect("verify the captured closure")
            .into_iter()
            .map(|oid| oid.to_hex())
            .collect::<BTreeSet<_>>();
        let objects = expected["objects"].as_array().expect("native objects");
        assert_eq!(objects.len(), 7);
        assert_eq!(
            closure,
            objects
                .iter()
                .map(|object| object["oid"].as_str().expect("an object ID").to_owned())
                .collect::<BTreeSet<_>>()
        );
        for object in objects {
            let oid = object["oid"].as_str().expect("an object ID");
            let (kind, bytes) = stored_git_object(directory.path(), oid);
            assert_eq!(kind, object["kind"].as_str().expect("an object kind"), "{oid}");
            assert_eq!(bytes, vector_hex(&object["bytes_hex"]), "{oid}");
        }
    }

/// ORNA-CAP-003: accepted pins transfer to an owner-scoped local ref namespace, never an ordinary remote ref.
    #[test]
    fn capture_acceptance_transfers_owner_scoped_scratch_to_annotated_pending_binding() {
        let (directory, graph) = capture_test_context();
        let graph = Arc::new(graph);
        let scope = graph.open_read_scope().expect("owner read scope");
        let capability = RepositoryCaptureCapability::new(Arc::clone(&graph), scope)
            .expect("matching graph and owner scope");
        let payload = b"annotated captured JSON";
        let candidate = capability
            .capture_blob_candidate(&payload[..], payload.len() as u64)
            .expect("capture through the shared writer");
        let pin_id = candidate.pin_id;
        let scratch_ref = scratch_content_ref(&graph.owner_id, &pin_id);
        assert!(fixture_ref_exists(directory.path(), &scratch_ref));

        let binding = capability
            .accept_captured_blob(candidate, "application/json", None)
            .expect("promote and annotate captured Blob");
        let transfer = binding.transfer_record();
        let pending_ref = accepted_content_ref(transfer.owner_id(), transfer.pin_id());
        assert_eq!(transfer.content_identity(), crate::blob_store::digest_bytes(payload));
        assert!(binding
            .encoded_value()
            .windows(b"application/json".len())
            .any(|window| window == b"application/json"));
        assert!(fixture_ref_exists(directory.path(), &pending_ref));
        assert!(!fixture_ref_exists(directory.path(), &scratch_ref));
        drop(binding);
        assert!(fixture_ref_exists(directory.path(), &pending_ref));
    }

    #[test]
    fn shared_capture_capability_uses_the_issued_graph_scope_and_writer() {
        let (directory, graph) = capture_test_context();
        let graph = Arc::new(graph);
        let scope = graph.open_read_scope().expect("owner read scope");
        let capability = RepositoryCaptureCapability::new(Arc::clone(&graph), scope.clone())
            .expect("matching graph and owner scope");
        let payload = b"shared capability streams into the owned writer";
        let candidate = capability
            .capture_blob_candidate(&payload[..], payload.len() as u64)
            .expect("capture through the shared repository capability");
        let pending_ref = scratch_content_ref(&graph.owner_id, &candidate.pin_id);
        assert!(fixture_ref_exists(directory.path(), &pending_ref));
        drop(candidate);
        assert!(!fixture_ref_exists(directory.path(), &pending_ref));
    }

    #[test]
    fn publication_transaction_keeps_capture_closure_pinned_until_rejected_callback_cleanup() {
        let (directory, graph) = capture_test_context();
        let scope = graph.open_read_scope().expect("owner read scope");
        let payload = b"candidate rejected by shared publication callback";
        let captured = graph
            .capture_blob_candidate(&payload[..], payload.len() as u64, &scope)
            .expect("capture candidate payload");
        let pin = graph
            .protect_captured_blob(captured, &scope)
            .expect("protect complete OGB-2 closure");
        let original_pin_ref = pin.protected_ref.clone();
        let descriptor_oid = pin.descriptor_oid.to_hex();
        let reference = AdmittedBlobReference {
            context: pin.context,
            database_id: pin.database_id,
            owner_id: pin.owner_id,
            snapshot_id: pin.snapshot_id,
            descriptor_oid: pin.descriptor_oid.clone(),
            identity: pin.identity,
        };
        let closure_scope = graph.open_read_scope().expect("closure verification scope");
        let closure_oids = graph
            .verify_full_blob(&reference, &closure_scope)
            .expect("enumerate the complete captured Blob closure")
            .into_iter()
            .map(|oid| oid.to_hex())
            .collect::<Vec<_>>();
        assert!(closure_oids.contains(&descriptor_oid));
        assert!(fixture_ref_exists(directory.path(), &original_pin_ref));
        let key = TypedKey::Text("rejected".to_owned());
        let callback_key = key.clone();
        let callback_pin_ref = original_pin_ref.clone();
        let worktree = directory.path().to_path_buf();
        // The original graph is a committed snapshot in production. Keep its
        // fixture StoreRoot reachable while the callback runs `git gc`.
        let baseline_ref = format!("refs/orna/test/baseline-{}", hex_encode(&graph.owner_id));
        let baseline_oid = graph.store_root.to_hex();
        let zero = "0".repeat(graph.algorithm.width() * 2);
        let baseline = Command::new("git")
            .current_dir(&worktree)
            .args(["update-ref", &baseline_ref, &baseline_oid, &zero])
            .output()
            .expect("retain baseline StoreRoot through GC");
        assert!(
            baseline.status.success(),
            "retain baseline StoreRoot: {}",
            String::from_utf8_lossy(&baseline.stderr)
        );
        let result = block_on(crate::commit_protected_blob_row(
            &graph,
            pin,
            "text/plain",
            None,
            crate::ProtectedBlobRowInsert::new(key.clone()),
            move |candidate| async move {
                assert_eq!(candidate.key(), &callback_key);
                assert_eq!(
                    candidate
                        .protected_content_transfer()
                        .descriptor_oid()
                        .to_hex(),
                    descriptor_oid
                );
                assert!(fixture_ref_exists(&worktree, &callback_pin_ref));
                let refs = Command::new("git")
                    .current_dir(&worktree)
                    .args(["for-each-ref", "--format=%(refname)", "refs/orna/pins"])
                    .output()
                    .expect("list protected candidate refs");
                assert!(refs.status.success());
                let candidate_ref = std::str::from_utf8(&refs.stdout)
                    .expect("ref names are UTF-8")
                    .lines()
                    .find(|reference| reference.contains("/scratch/row-"))
                    .expect("candidate store-root ref")
                    .to_owned();
                assert!(fixture_ref_exists(&worktree, &candidate_ref));

                let gc = Command::new("git")
                    .current_dir(&worktree)
                    .args(["gc", "--prune=now", "--quiet"])
                    .output()
                    .expect("run GC while publication is pending");
                assert!(
                    gc.status.success(),
                    "git gc failed: {}",
                    String::from_utf8_lossy(&gc.stderr)
                );
                for oid in &closure_oids {
                    let object = Command::new("git")
                        .current_dir(&worktree)
                        .args(["cat-file", "-e", oid])
                        .output()
                        .expect("verify captured graph object after GC");
                    assert!(
                        object.status.success(),
                        "protected closure object {oid} was collected"
                    );
                }
                let candidate_root = candidate.store_root().to_hex();
                let root = Command::new("git")
                    .current_dir(&worktree)
                    .args(["cat-file", "-e", &candidate_root])
                    .output()
                    .expect("verify candidate store root after GC");
                assert!(
                    root.status.success(),
                    "protected candidate root was collected"
                );
                Err::<(), _>("publication rejected")
            },
        ));
        assert!(matches!(
            result,
            Err(crate::PublicationTransactionError::Commit(
                "publication rejected"
            ))
        ));
        assert!(pending_refs(directory.path()).is_empty());
        assert!(!fixture_ref_exists(directory.path(), &original_pin_ref));

        let scope = graph.open_read_scope().expect("fresh owner read scope");
        assert!(
            graph
                .lookup_row(&TypedKey::Text("rejected".to_owned()), &scope)
                .expect("lookup baseline row map")
                .is_none()
        );

        let captured = graph
            .capture_blob_candidate(&payload[..], payload.len() as u64, &scope)
            .expect("capture second candidate payload");
        let mut pin = graph
            .protect_captured_blob(captured, &scope)
            .expect("protect second OGB-2 closure");
        let invalid_pin_ref = pin.protected_ref.clone();
        let mut invalid_descriptor = pin.descriptor_oid.as_bytes().to_vec();
        invalid_descriptor[0] ^= 1;
        pin.descriptor_oid = NativeOid::new(graph.algorithm, invalid_descriptor)
            .expect("valid-width invalid descriptor");
        let result = block_on(crate::commit_protected_blob_row(
            &graph,
            pin,
            "text/plain",
            None,
            crate::ProtectedBlobRowInsert::new(TypedKey::Text("rejected-pin".to_owned())),
            |_| {
                panic!("invalid pin must not invoke the callback");
                #[allow(unreachable_code)]
                async {
                    Ok::<(), &'static str>(())
                }
            },
        ));
        assert!(matches!(
            result,
            Err(crate::PublicationTransactionError::GraphCleanup {
                graph: GraphError::InvalidProtectedRef,
                cleanup,
            }) if cleanup.iter().any(|(reference, error)| {
                reference == &invalid_pin_ref && error == &GraphError::GitCommandFailed
            })
        ));
        assert!(fixture_ref_exists(directory.path(), &invalid_pin_ref));
        assert!(pending_refs(directory.path()).is_empty());

        let scope = graph.open_read_scope().expect("fresh owner read scope");
        assert!(
            graph
                .lookup_row(&TypedKey::Text("rejected-pin".to_owned()), &scope)
                .expect("lookup baseline row map")
                .is_none()
        );
    }

    #[test]
    fn publication_transaction_reports_both_failed_rejected_callback_cleanups() {
        let (directory, graph) = capture_test_context();
        let scope = graph.open_read_scope().expect("owner read scope");
        let payload = b"failed cleanup refs remain recoverable by identity";
        let captured = graph
            .capture_blob_candidate(&payload[..], payload.len() as u64, &scope)
            .expect("capture candidate payload");
        let pin = graph
            .protect_captured_blob(captured, &scope)
            .expect("protect complete OGB-2 closure");
        let pin_ref = pin.protected_ref.clone();
        let callback_pin_ref = pin_ref.clone();
        let worktree = directory.path().to_path_buf();
        let wrong_target = graph.store_root.to_hex();
        let result = block_on(crate::commit_protected_blob_row(
            &graph,
            pin,
            "text/plain",
            None,
            crate::ProtectedBlobRowInsert::new(TypedKey::Text("rejected-cleanup".to_owned())),
            move |_| async move {
                let output = Command::new("git")
                    .current_dir(&worktree)
                    .args([
                        "for-each-ref",
                        "--format=%(refname)",
                        "refs/orna/pins",
                    ])
                    .output()
                    .expect("list provisional row refs");
                assert!(output.status.success());
                let candidate_ref = std::str::from_utf8(&output.stdout)
                    .expect("ref names are UTF-8")
                    .lines()
                    .find(|reference| reference.contains("/row-"))
                    .expect("prepared candidate ref")
                    .to_owned();

                for reference in [&candidate_ref, &callback_pin_ref] {
                    let output = Command::new("git")
                        .current_dir(&worktree)
                        .args(["update-ref", reference, &wrong_target])
                        .output()
                        .expect("replace cleanup ref target");
                    assert!(
                        output.status.success(),
                        "replace cleanup ref target: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                Err::<(), _>("publication rejected after ref replacement")
            },
        ));

        let cleanup = match result {
            Err(crate::PublicationTransactionError::CommitCleanup { commit, cleanup }) => {
                assert_eq!(commit, "publication rejected after ref replacement");
                cleanup
            }
            other => panic!("expected aggregate cleanup error, got {other:?}"),
        };
        assert_eq!(cleanup.len(), 2);
        assert!(cleanup.iter().all(|(_, error)| *error == GraphError::GitCommandFailed));
        assert!(cleanup.iter().any(|(reference, _)| reference == &pin_ref));
        let candidate_ref = cleanup
            .iter()
            .map(|(reference, _)| reference)
            .find(|reference| reference.contains("/scratch/row-"))
            .expect("candidate cleanup identity");
        assert!(fixture_ref_exists(directory.path(), &pin_ref));
        assert!(fixture_ref_exists(directory.path(), candidate_ref));
    }

    #[test]
    fn row_index_collection_checks_each_shared_child_parent_fence() {
        let domain = rows_domain([0x11; 16], [0x22; 32]);
        let key_a = TypedKey::Text("a".to_owned());
        let key_b = TypedKey::Text("b".to_owned());
        let key_c = TypedKey::Text("c".to_owned());
        let leaf = NodeData::OrderedLeaf {
            domain: domain.clone(),
            entries: vec![
                OrderedLeafEntry {
                    key: key_a.canonical_bytes().expect("canonical key"),
                    value: vec![0x80],
                },
                OrderedLeafEntry {
                    key: key_b.canonical_bytes().expect("canonical key"),
                    value: vec![0x80],
                },
            ],
        };
        let child_oid = NativeOid::new(GitHashAlgorithm::Sha1, vec![0x33; 20])
            .expect("valid child object id");
        let root_oid = NativeOid::new(GitHashAlgorithm::Sha1, vec![0x44; 20])
            .expect("valid root object id");
        let branch = NodeData::OrderedBranch {
            domain: domain.clone(),
            height: 1,
            entries: vec![
                OrderedBranchEntry {
                    inclusive_max_key: key_b.canonical_bytes().expect("canonical fence"),
                    child: child_oid.clone(),
                    row_count: 2,
                },
                OrderedBranchEntry {
                    inclusive_max_key: key_c.canonical_bytes().expect("canonical fence"),
                    child: child_oid.clone(),
                    row_count: 2,
                },
            ],
        };
        let mut read_node = |oid: &NativeOid| {
            if oid == &child_oid {
                Ok(leaf.clone())
            } else {
                Err(GraphError::ContextMismatch)
            }
        };
        let mut nodes = BTreeSet::from([root_oid.clone()]);
        let mut payload = BTreeSet::new();
        let mut validated_contexts = BTreeSet::new();

        assert!(matches!(
            collect_ordered_index_objects_with(
                &mut read_node,
                &branch,
                &root_oid,
                &domain,
                None,
                &mut nodes,
                &mut payload,
                &mut validated_contexts,
            ),
            Err(GraphError::InvalidCount(2))
        ));
    }

    #[test]
    fn full_blob_verification_rejects_noncanonical_gear_chunk_boundaries() {
        let (directory, graph) = capture_test_context();
        let bytes = b"ab";
        let identity = crate::blob_store::digest_bytes(bytes);

        let build_reference = |chunks: &[&[u8]]| {
            let mut entries = Vec::new();
            for chunk in chunks {
                let oid = fixture_git_object(directory.path(), "blob", chunk);
                let span = crate::blob_store::ChunkSpan::new(
                    0,
                    chunk.len() as u64,
                    crate::blob_store::digest_bytes(chunk).sha256(),
                )
                .expect("valid chunk span");
                entries.push(ByteIndexEntry {
                    span_length: span.length(),
                    target: oid,
                    chunk_sha256: Some(span.sha256()),
                });
            }
            let byte_root = fixture_git_node(
                directory.path(),
                &NodeData::ByteIndex {
                    height: 0,
                    total_length: bytes.len() as u64,
                    entries,
                },
            );
            let descriptor_oid = fixture_git_node(
                directory.path(),
                &NodeData::BlobDescriptor {
                    length: bytes.len() as u64,
                    raw_sha256: identity.sha256(),
                    byte_root: Some(byte_root),
                },
            );
            AdmittedBlobReference {
                context: graph.identity,
                database_id: graph.database_id,
                owner_id: graph.owner_id,
                snapshot_id: graph.snapshot_id,
                descriptor_oid,
                identity,
            }
        };

        let scope = graph.open_read_scope().expect("verification scope");
        graph
            .verify_full_blob(&build_reference(&[bytes]), &scope)
            .expect("single final tail is canonical");

        let noncanonical = build_reference(&[b"a", b"b"]);
        let scope = graph.open_read_scope().expect("fresh verification scope");
        assert!(matches!(
            graph.verify_full_blob(&noncanonical, &scope),
            Err(GraphError::InvalidByteIndex)
        ));
    }

    #[test]
    fn relation_candidate_rejects_substituted_relation_and_keeps_source_root_selected() {
        let (directory, graph) = capture_test_context();
        let scope = graph.open_read_scope().expect("owner read scope");
        let original = graph.row_snapshot.version().clone();
        let substituted_relation_id = [0x42; 16];
        let substituted_schema = crate::row_store::SchemaGeneration::issue(
            *original.database_id(),
            substituted_relation_id,
            original.schema().schema_oid().clone(),
            *original.schema().schema_digest(),
            original.schema().generation(),
        );
        let substituted = crate::row_store::RowMapVersion::issue(
            *original.database_id(),
            substituted_relation_id,
            original.store_root().clone(),
            substituted_schema,
            original.primary_root().clone(),
            original.generation(),
            original.row_count(),
        )
        .expect("internally consistent substituted relation binding");
        let key = TypedKey::Text("must-not-be-written".to_owned());
        let fields = vec![0x80];

        assert!(matches!(
            graph.maintain_relation_rows(
                &substituted,
                &[RowIndexMutation::Insert {
                    key: key.clone(),
                    fields: fields.clone(),
                }],
                &scope,
            ),
            Err(GraphError::ContextMismatch)
        ));

        assert_eq!(graph.store_root, *original.store_root());
        let selected = load_format3_row_map(
            &graph.repository,
            graph.algorithm,
            graph.store_root.clone(),
            *original.database_id(),
            graph.snapshot_id,
            *original.relation_id(),
        )
        .expect("prior selected relation root remains admitted");
        assert_eq!(selected.version().store_root(), original.store_root());
        assert_eq!(selected.version().schema(), original.schema());
        assert_eq!(selected.version().primary_root(), original.primary_root());
        assert_eq!(selected.version().row_count(), original.row_count());
        assert!(graph
            .lookup_row(&key, &scope)
            .expect("lookup selected source map")
            .is_none());
        assert!(pending_refs(directory.path()).is_empty());

        let candidate_store_root = graph
            .maintain_relation_rows(
                &original,
                &[RowIndexMutation::Insert { key, fields }],
                &scope,
            )
            .expect("admit rows against the exact pinned relation version");
        assert_ne!(&candidate_store_root, original.store_root());
        let successor = load_format3_row_map(
            &graph.repository,
            graph.algorithm,
            candidate_store_root,
            *original.database_id(),
            graph.snapshot_id,
            *original.relation_id(),
        )
        .expect("candidate StoreRoot resolves its successor relation map");
        assert_eq!(successor.version().schema(), original.schema());
        assert_eq!(successor.version().row_count(), Some(1));
        assert_eq!(graph.store_root, *original.store_root());
        assert!(graph
            .lookup_row(&TypedKey::Text("must-not-be-written".to_owned()), &scope)
            .expect("source root remains selected")
            .is_none());
    }

    #[test]
    fn one_native_store_root_publishes_row_changes_for_multiple_relations() {
        let (directory, mut graph) = capture_test_context();
        let scope = graph.open_read_scope().expect("owner read scope");
        let original = graph.row_snapshot.version().clone();
        let second_relation = [0x42; 16];
        let algorithm = graph.algorithm;
        let schema_oid = original.schema().schema_oid().clone();
        let (second_schema_oid, second_schema_digest) =
            fixture_schema_node(directory.path(), second_relation, false);
        let second_primary = fixture_git_node(
            directory.path(),
            &NodeData::OrderedLeaf {
                domain: rows_domain(second_relation, second_schema_digest),
                entries: Vec::new(),
            },
        );
        let relation_domain = canonical_value_bytes(&CborValue::Array(vec![
            CborValue::Text("relations".to_owned()),
            CborValue::Bytes(original.database_id().to_vec()),
        ]));
        let relation_entry = |
            relation_id: [u8; 16],
            schema_oid: &NativeOid,
            primary_root: &NativeOid,
        | {
            OrderedLeafEntry {
                key: TypedKey::Bytes(relation_id.to_vec())
                    .canonical_bytes()
                    .expect("canonical relation key"),
                value: canonical_value_bytes(&CborValue::Array(vec![
                    CborValue::Bytes(schema_oid.as_bytes().to_vec()),
                    CborValue::Bytes(primary_root.as_bytes().to_vec()),
                    CborValue::Null,
                    CborValue::Unsigned(0),
                ])),
            }
        };
        let relation_map = fixture_git_node(
            directory.path(),
            &NodeData::OrderedLeaf {
                domain: relation_domain,
                entries: vec![
                    relation_entry(
                        *original.relation_id(),
                        &schema_oid,
                        original.primary_root(),
                    ),
                    relation_entry(second_relation, &second_schema_oid, &second_primary),
                ],
            },
        );
        let source_root = fixture_git_node(
            directory.path(),
            &NodeData::StoreRoot { relation_map },
        );
        let first_snapshot = load_format3_row_map(
            &graph.repository,
            algorithm,
            source_root.clone(),
            *original.database_id(),
            graph.snapshot_id,
            *original.relation_id(),
        )
        .expect("load first relation in shared source StoreRoot");
        let second_snapshot = load_format3_row_map(
            &graph.repository,
            algorithm,
            source_root.clone(),
            *original.database_id(),
            graph.snapshot_id,
            second_relation,
        )
        .expect("load second relation in shared source StoreRoot");
        let first_version = first_snapshot.version().clone();
        let second_version = second_snapshot.version().clone();
        graph.store_root = source_root;
        graph.row_snapshot = first_snapshot;

        let first_mutation = [RowIndexMutation::Insert {
            key: TypedKey::Text("first".to_owned()),
            fields: vec![0x80],
        }];
        let first = graph
            .maintain_row_index(&first_mutation, &scope)
            .expect("mutate first relation natively");

        let first_only_root = graph
            .maintain_relations_rows(&[(&first_version, &first)], &scope)
            .expect("publish first relation without rewriting the second");
        let untouched_second = load_format3_row_map(
            &graph.repository,
            algorithm,
            first_only_root,
            *original.database_id(),
            graph.snapshot_id,
            second_relation,
        )
        .expect("load untouched second relation from successor StoreRoot");
        assert_eq!(
            untouched_second.version().schema().schema_oid(),
            second_version.schema().schema_oid()
        );
        assert_eq!(
            untouched_second.version().schema().schema_digest(),
            second_version.schema().schema_digest()
        );
        assert_eq!(
            untouched_second.version().primary_root(),
            second_version.primary_root()
        );
        assert_eq!(
            untouched_second.version().generation(),
            second_version.generation()
        );
        assert_eq!(
            untouched_second.version().row_count(),
            second_version.row_count()
        );
        graph.row_snapshot = second_snapshot;
        let second_mutation = [RowIndexMutation::Insert {
            key: TypedKey::Text("second".to_owned()),
            fields: vec![0x80],
        }];
        let second = graph
            .maintain_row_index(&second_mutation, &scope)
            .expect("mutate second relation natively");
        graph.row_snapshot = load_format3_row_map(
            &graph.repository,
            algorithm,
            graph.store_root.clone(),
            *original.database_id(),
            graph.snapshot_id,
            *original.relation_id(),
        )
        .expect("restore first relation context");

        let published_root = graph
            .maintain_relations_rows(
                &[(&first_version, &first), (&second_version, &second)],
                &scope,
            )
            .expect("publish both relation changes into one StoreRoot");
        assert_ne!(published_root, graph.store_root);
        graph.store_root = published_root.clone();
        for (relation_id, key) in [
            (*first_version.relation_id(), TypedKey::Text("first".to_owned())),
            (second_relation, TypedKey::Text("second".to_owned())),
        ] {
            graph.row_snapshot = load_format3_row_map(
                &graph.repository,
                algorithm,
                published_root.clone(),
                *original.database_id(),
                graph.snapshot_id,
                relation_id,
            )
            .expect("load updated relation from shared published root");
            assert_eq!(graph.row_snapshot.version().row_count(), Some(1));
            assert!(graph
                .lookup_row(&key, &scope)
                .expect("read published native row")
                .is_some());
        }
    }

    /// Applies one maintenance pass to the fixture relation and publishes it
    /// as the graph's pinned row map, so the next pass rewrites from it.
    /// Returns the new primary root, its row count, every object the pass
    /// wrote (sorted) and its created/reused index node counts.
    fn maintain_and_repin(
        graph: &mut NativeGraphContext,
        mutations: &[RowIndexMutation],
    ) -> (String, u64, Vec<String>, usize, usize) {
        let scope = graph.open_read_scope().expect("owner read scope");
        let maintenance = graph
            .maintain_row_index(mutations, &scope)
            .expect("maintain fixture rows");
        let version = graph.row_snapshot.version().clone();
        let store_root = graph
            .replace_relation_primary_root(
                &graph.store_root,
                &version,
                maintenance.primary_root(),
                maintenance.row_count(),
                &scope,
            )
            .expect("publish maintained rows into the relation map");
        graph.row_snapshot = load_format3_row_map(
            &graph.repository,
            graph.algorithm,
            store_root.clone(),
            graph.database_id,
            graph.snapshot_id,
            *version.relation_id(),
        )
        .expect("reload the maintained relation");
        graph.store_root = store_root;
        let mut objects = maintenance
            .objects_written()
            .iter()
            .map(NativeOid::to_hex)
            .collect::<Vec<_>>();
        objects.sort();
        (
            maintenance.primary_root().to_hex(),
            maintenance.row_count(),
            objects,
            maintenance.index_objects_created().len(),
            maintenance.index_objects_reused().len(),
        )
    }

    /// Pins the exact ORP-1 objects `maintain_row_index` writes for a
    /// multi-page build, an edit of that build (delete, rekey, insert) and its
    /// deletion down to the one empty leaf. Object ids are hashes of the page
    /// and branch bytes, so a builder change that alters any page, fence,
    /// count or partition boundary changes this transcript.
    #[test]
    fn maintain_row_index_writes_pinned_page_bytes() {
        let (_directory, mut graph) = capture_test_context();
        let empty = vec![0x80];
        let flagged = vec![0x81, 0xf5];
        let key = |index: usize| TypedKey::Text(format!("row-{index:04}"));
        let seed = (0..300)
            .map(|index| RowIndexMutation::Insert {
                key: key(index),
                fields: empty.clone(),
            })
            .collect::<Vec<_>>();
        let edit = vec![
            RowIndexMutation::Delete { key: key(0) },
            RowIndexMutation::Delete { key: key(150) },
            RowIndexMutation::Rekey {
                from: key(299),
                to: TypedKey::Text("row-0300-z".to_owned()),
            },
            RowIndexMutation::Insert {
                key: TypedKey::Text("row-0150-b".to_owned()),
                fields: flagged,
            },
        ];
        let mut remaining = (1..299)
            .filter(|index| *index != 150)
            .map(|index| RowIndexMutation::Delete { key: key(index) })
            .collect::<Vec<_>>();
        remaining.push(RowIndexMutation::Delete {
            key: TypedKey::Text("row-0150-b".to_owned()),
        });
        remaining.push(RowIndexMutation::Delete {
            key: TypedKey::Text("row-0300-z".to_owned()),
        });

        let mut transcript = String::new();
        for mutations in [&seed, &edit, &remaining] {
            let (root, rows, objects, created, reused) = maintain_and_repin(&mut graph, mutations);
            let digest = Sha256::digest(objects.join("\n").as_bytes());
            transcript.push_str(&format!(
                "{root} {rows} {} {created} {reused} {}\n",
                objects.len(),
                digest
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            ));
        }
        assert_eq!(
            transcript, PINNED_PAGE_TRANSCRIPT,
            "ORP-1 page bytes changed"
        );
    }

    /// Captured from `maintain_row_index` before its page builder became the
    /// shared canonical ordered-map builder: root, row count, objects
    /// written, index nodes created and reused, and SHA-256 of the sorted
    /// written object ids, per pass.
    const PINNED_PAGE_TRANSCRIPT: &str = "\
108139c78d535e7947c3e9c3bdace667dd197ad3 300 17 8 0 43edf1679f44a02f334bcb9b86bb6642aa5fcacd7dd8d0b4599ae841ed7e4026
3cf13f6b38ff09e1c6bd558b44c4e69c7973ec60 299 9 4 4 51bc5de1c96239409ad186b4d6e7049548f1a3ecd9831642b5b400a16597fa82
1c28ee1360793fb7f892a4910b04f88d701d615a 0 2 1 0 cd9e5188da52678cb40448fc11adbb91ce6ec424d3145517f14c390fbbbf71ef
";

    /// Publication hands `maintain_row_index` its batch in canonical key byte
    /// order, which for Text keys is length-first rather than logical, so
    /// 20,000 inserts into a 20,000-row table land all over the pinned rows.
    /// The pass must build the root that a complete canonical build of the
    /// final rows builds.
    #[test]
    fn text_key_bulk_insert_in_canonical_byte_order_builds_the_complete_root() {
        const ROWS: usize = 20_000;
        let (_directory, mut graph) = capture_test_context();
        let pinned_fields = vec![0x80];
        let inserted_fields = vec![0x81, 0xf5];
        // Unpadded decimal keys: "10" sorts before "9" logically, but after it
        // as canonical bytes, whose length prefix comes first.
        let key = |index: usize| TypedKey::Text(index.to_string());
        let seed = (0..ROWS)
            .map(|index| RowIndexMutation::Insert {
                key: key(2 * index),
                fields: pinned_fields.clone(),
            })
            .collect::<Vec<_>>();
        maintain_and_repin(&mut graph, &seed);

        let mut inserted_keys = (0..ROWS)
            .map(|index| key(2 * index + 1))
            .collect::<Vec<_>>();
        inserted_keys.sort_by_cached_key(|key| key.canonical_bytes().expect("canonical Text key"));
        assert!(
            inserted_keys.windows(2).any(|pair| pair[0] > pair[1]),
            "canonical byte order of the inserts must differ from their logical order"
        );
        let inserts = inserted_keys
            .into_iter()
            .map(|key| RowIndexMutation::Insert {
                key,
                fields: inserted_fields.clone(),
            })
            .collect::<Vec<_>>();

        let scope = graph.open_read_scope().expect("owner read scope");
        let maintenance = graph
            .maintain_row_index(&inserts, &scope)
            .expect("insert 20,000 rows among 20,000");
        assert_eq!(maintenance.row_count(), 2 * ROWS as u64);

        let final_rows = (0..2 * ROWS)
            .map(|index| {
                let fields = if index % 2 == 0 {
                    &pinned_fields
                } else {
                    &inserted_fields
                };
                (key(index), fields.clone())
            })
            .collect::<Vec<_>>();
        let mut written = BTreeSet::new();
        let full_build = complete_row_build(
            &rows_domain_for_version(graph.row_snapshot.version()),
            &final_rows,
            &mut |node| graph.write_native_node(node, &mut written),
        );
        assert_eq!(maintenance.primary_root(), &full_build);
    }

    /// The complete canonical build of `rows` (key and stored field tuple,
    /// in any order) through `write`, as relation admission builds a map.
    fn complete_row_build(
        domain: &[u8],
        rows: &[(TypedKey, Vec<u8>)],
        write: &mut impl FnMut(&NodeData) -> Result<NativeOid, GraphError>,
    ) -> NativeOid {
        let mut entries = rows
            .iter()
            .map(|(key, fields)| {
                crate::row_store::RowEntry::new(
                    key.clone(),
                    RowValue::decode_canonical_fields(fields.clone()).expect("inline row"),
                )
            })
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| left.key().cmp(right.key()));
        NativeGraphContext::build_canonical_ordered_map(domain, &entries, write)
            .expect("complete canonical build")
    }

    /// The pass as it was first written: every delete, then every rekey, then
    /// every insert, each applied to the ascending rows one at a time. It is
    /// the oracle a maintenance pass must agree with, rows and error alike.
    fn one_at_a_time_row_index_mutations(
        rows: &mut Vec<(TypedKey, Vec<u8>)>,
        mutations: &[RowIndexMutation],
    ) -> Result<Vec<LogicalRowRekey>, GraphError> {
        let search = |rows: &[(TypedKey, Vec<u8>)], key: &TypedKey| {
            rows.binary_search_by(|(existing, _)| existing.cmp(key))
        };
        let mut logical_rekeys = Vec::new();
        for mutation in mutations {
            if let RowIndexMutation::Delete { key } = mutation {
                let index = search(rows, key).map_err(|_| GraphError::RowKeyAbsent)?;
                rows.remove(index);
            }
        }
        for mutation in mutations {
            if let RowIndexMutation::Rekey { from, to } = mutation {
                let index = search(rows, from).map_err(|_| GraphError::RowKeyAbsent)?;
                if search(rows, to).is_ok() {
                    return Err(GraphError::DuplicateRowKey);
                }
                let (_, fields) = rows.remove(index);
                let insertion = search(rows, to).unwrap_or_else(|index| index);
                rows.insert(insertion, (to.clone(), fields));
                logical_rekeys.push(LogicalRowRekey {
                    old_address: from.clone(),
                    new_address: to.clone(),
                });
            }
        }
        for mutation in mutations {
            if let RowIndexMutation::Insert { key, fields } = mutation {
                let insertion = search(rows, key).unwrap_or_else(|index| index);
                if rows
                    .get(insertion)
                    .is_some_and(|(existing, _)| existing == key)
                {
                    return Err(GraphError::DuplicateRowKey);
                }
                rows.insert(insertion, (key.clone(), fields.clone()));
            }
        }
        Ok(logical_rekeys)
    }

    /// The batches a maintenance pass is most likely to get wrong, run through
    /// `maintain_row_index` against one pinned map, each with the outcome
    /// applying it one mutation at a time gives: the same error, or the same
    /// rows as the root of their complete build.
    #[test]
    fn maintain_row_index_keeps_one_at_a_time_outcomes() {
        let (_directory, mut graph) = capture_test_context();
        let text = |key: &str| TypedKey::Text(key.to_owned());
        let fields = |value: u8| {
            canonical_value_bytes(&CborValue::Array(vec![CborValue::Unsigned(value.into())]))
        };
        let rows = |entries: &[(&str, u8)]| {
            entries
                .iter()
                .map(|&(key, value)| (text(key), fields(value)))
                .collect::<Vec<_>>()
        };
        let delete = |key: &str| RowIndexMutation::Delete { key: text(key) };
        let rekey = |from: &str, to: &str| RowIndexMutation::Rekey {
            from: text(from),
            to: text(to),
        };
        let insert = |key: &str, value: u8| RowIndexMutation::Insert {
            key: text(key),
            fields: fields(value),
        };
        // "aa" sorts between "a" and "b" logically but after "b" as bytes.
        let pinned = rows(&[("a", 1), ("aa", 2), ("b", 3), ("ba", 4)]);
        maintain_and_repin(
            &mut graph,
            &pinned
                .iter()
                .map(|(key, fields)| RowIndexMutation::Insert {
                    key: key.clone(),
                    fields: fields.clone(),
                })
                .collect::<Vec<_>>(),
        );
        let domain = rows_domain_for_version(graph.row_snapshot.version());
        let mut sink = BTreeSet::new();
        let cases = [
            (
                vec![delete("a"), delete("a")],
                Err(GraphError::RowKeyAbsent),
            ),
            // Deletes apply before inserts, so the absent delete is reported.
            (
                vec![insert("a", 9), delete("zz")],
                Err(GraphError::RowKeyAbsent),
            ),
            // Rekeys fail in batch order.
            (
                vec![rekey("b", "aa"), rekey("zz", "c")],
                Err(GraphError::DuplicateRowKey),
            ),
            (
                vec![rekey("zz", "c"), rekey("b", "aa")],
                Err(GraphError::RowKeyAbsent),
            ),
            (vec![rekey("a", "a")], Err(GraphError::DuplicateRowKey)),
            // A deleted row is no rekey source.
            (
                vec![delete("a"), rekey("a", "c")],
                Err(GraphError::RowKeyAbsent),
            ),
            (
                vec![rekey("a", "c"), rekey("c", "bb")],
                Ok(rows(&[("aa", 2), ("b", 3), ("ba", 4), ("bb", 1)])),
            ),
            // Two rows swap keys through a free one.
            (
                vec![rekey("a", "x"), rekey("b", "a"), rekey("x", "b")],
                Ok(rows(&[("a", 3), ("aa", 2), ("b", 1), ("ba", 4)])),
            ),
            (
                vec![delete("b"), rekey("a", "b")],
                Ok(rows(&[("aa", 2), ("b", 1), ("ba", 4)])),
            ),
            // Updates as publication sends them: by canonical key bytes, each
            // key's delete before its insert.
            (
                vec![delete("b"), insert("b", 9), delete("aa"), insert("aa", 8)],
                Ok(rows(&[("a", 1), ("aa", 8), ("b", 9), ("ba", 4)])),
            ),
            // A removed key is inserted again.
            (
                vec![insert("a", 7), rekey("a", "c")],
                Ok(rows(&[("a", 7), ("aa", 2), ("b", 3), ("ba", 4), ("c", 1)])),
            ),
            (
                vec![rekey("a", "c"), insert("c", 7)],
                Err(GraphError::DuplicateRowKey),
            ),
            (
                vec![insert("c", 7), insert("ab", 6), insert("c", 5)],
                Err(GraphError::DuplicateRowKey),
            ),
            (vec![insert("ba", 7)], Err(GraphError::DuplicateRowKey)),
            (
                vec![insert("c", 7), insert("", 6), insert("ab", 5)],
                Ok(rows(&[
                    ("", 6),
                    ("a", 1),
                    ("aa", 2),
                    ("ab", 5),
                    ("b", 3),
                    ("ba", 4),
                    ("c", 7),
                ])),
            ),
        ];
        for (mutations, expected) in cases {
            let mut reference = pinned.clone();
            let outcome =
                one_at_a_time_row_index_mutations(&mut reference, &mutations).map(|_| reference);
            assert_eq!(outcome, expected, "one-at-a-time pass over {mutations:?}");
            let scope = graph.open_read_scope().expect("owner read scope");
            let maintained = graph.maintain_row_index(&mutations, &scope);
            match expected {
                Ok(expected_rows) => {
                    let maintenance =
                        maintained.unwrap_or_else(|error| panic!("{mutations:?}: {error:?}"));
                    assert_eq!(
                        maintenance.row_count(),
                        expected_rows.len() as u64,
                        "{mutations:?}"
                    );
                    let complete = complete_row_build(&domain, &expected_rows, &mut |node| {
                        graph.write_capture_node(node, &mut sink, u64::MAX)
                    });
                    assert_eq!(maintenance.primary_root(), &complete, "{mutations:?}");
                }
                Err(error) => assert_eq!(maintained.err(), Some(error), "{mutations:?}"),
            }
        }

        let scope = graph.open_read_scope().expect("owner read scope");
        let chained = graph
            .maintain_row_index(&[rekey("a", "c"), delete("b"), rekey("c", "b")], &scope)
            .expect("rekey chain onto a deleted key");
        assert_eq!(
            chained.logical_rekeys(),
            [
                LogicalRowRekey {
                    old_address: text("a"),
                    new_address: text("c"),
                },
                LogicalRowRekey {
                    old_address: text("c"),
                    new_address: text("b"),
                },
            ]
        );
        let complete = complete_row_build(
            &domain,
            &rows(&[("aa", 2), ("b", 1), ("ba", 4)]),
            &mut |node| graph.write_capture_node(node, &mut sink, u64::MAX),
        );
        assert_eq!(chained.primary_root(), &complete);
    }

    /// splitmix64, so a fixed seed replays the same cases on every run.
    struct SplitMix64(u64);

    impl SplitMix64 {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut mixed = self.0;
            mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            mixed ^ (mixed >> 31)
        }

        fn below(&mut self, bound: usize) -> usize {
            (self.next() % bound as u64) as usize
        }

        /// One of `keys`, a non-negative Int spelled `Int` or `UInt` at random.
        fn key_from(&mut self, keys: &[TypedKey]) -> TypedKey {
            let key = keys[self.below(keys.len())].clone();
            self.spelled(key)
        }

        /// A batch key: any of `universe` `strays` times in eight, otherwise
        /// one taken out of `pool`, or none once `pool` is empty.
        fn draw(
            &mut self,
            pool: &mut Vec<TypedKey>,
            strays: usize,
            universe: &[TypedKey],
        ) -> Option<TypedKey> {
            if self.below(8) < strays {
                return Some(self.key_from(universe));
            }
            if pool.is_empty() {
                return None;
            }
            let key = pool.swap_remove(self.below(pool.len()));
            Some(self.spelled(key))
        }

        /// `key`, a non-negative Int spelled `Int` or `UInt` at random.
        fn spelled(&mut self, key: TypedKey) -> TypedKey {
            match key {
                TypedKey::Int(value) if value >= 0 && self.below(2) == 0 => {
                    TypedKey::UInt(value.unsigned_abs())
                }
                key => key,
            }
        }
    }

    /// Distinct one-field row tuples, one per call.
    fn fuzz_values() -> impl FnMut() -> Vec<u8> {
        let mut values = 0u64;
        move || {
            values += 1;
            canonical_value_bytes(&CborValue::Array(vec![CborValue::Unsigned(values)]))
        }
    }

    /// A seeded key universe: the Text words of up to four letters over two
    /// to four letters, whose canonical byte order is not their logical
    /// order, or a run of Int keys around zero.
    fn fuzz_universe(rng: &mut SplitMix64, text: bool) -> Vec<TypedKey> {
        if !text {
            let span = 2 + rng.below(40) as i64;
            return (-span..span).map(TypedKey::Int).collect();
        }
        let letters = 2 + rng.below(3) as u8;
        let mut level = vec![String::new()];
        let mut keys = level.clone();
        for _ in 0..1 + rng.below(4) {
            level = level
                .iter()
                .flat_map(|prefix| {
                    (b'a'..b'a' + letters)
                        .map(move |letter| format!("{prefix}{}", char::from(letter)))
                })
                .collect();
            keys.extend(level.iter().cloned());
        }
        keys.into_iter().map(TypedKey::Text).collect()
    }

    /// Pinned rows over a random share of `universe`, ascending by key, and
    /// the keys left free.
    fn fuzz_pinned(
        rng: &mut SplitMix64,
        universe: &[TypedKey],
        value: &mut impl FnMut() -> Vec<u8>,
    ) -> (Vec<(TypedKey, Vec<u8>)>, Vec<TypedKey>) {
        let density = 1 + rng.below(9);
        let mut pinned = Vec::new();
        let mut free = Vec::new();
        for key in universe {
            if rng.below(10) < density {
                let key = rng.key_from(std::slice::from_ref(key));
                pinned.push((key, value()));
            } else {
                free.push(key.clone());
            }
        }
        pinned.sort_by(|left, right| left.0.cmp(&right.0));
        (pinned, free)
    }

    /// A seeded batch of deletes, rekeys, inserts and delete-plus-insert
    /// updates against `pinned`. Deletes and rekey sources take keys still
    /// removable: pinned keys, then each rekey's target. Inserts and rekey
    /// targets take keys still addable: free keys, then each removed key. So
    /// batches chain rekeys and reuse removed keys. Removals stray to any key
    /// of `universe` never or one time in eight and additions up to five
    /// times in eight, so batches also repeat keys (a second delete of one
    /// row), name absent ones and collide, and no outcome crowds out the
    /// others. Half the batches come in publication's order: canonical key
    /// bytes, and a key's delete before its insert.
    fn fuzz_batch(
        rng: &mut SplitMix64,
        universe: &[TypedKey],
        pinned: &[(TypedKey, Vec<u8>)],
        free: Vec<TypedKey>,
        value: &mut impl FnMut() -> Vec<u8>,
    ) -> Vec<RowIndexMutation> {
        let mut removable = pinned
            .iter()
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        let mut addable = free;
        let (removal_strays, addition_strays) = (rng.below(2), rng.below(6));
        let mut batch = Vec::new();
        for _ in 0..rng.below(25) {
            match rng.below(10) {
                0..=2 => {
                    let Some(key) = rng.draw(&mut removable, removal_strays, universe) else {
                        continue;
                    };
                    addable.push(key.clone());
                    batch.push(RowIndexMutation::Delete { key });
                }
                3..=4 => {
                    let from = rng.draw(&mut removable, removal_strays, universe);
                    let to = rng.draw(&mut addable, addition_strays, universe);
                    let (Some(from), Some(to)) = (from, to) else {
                        continue;
                    };
                    addable.push(from.clone());
                    removable.push(to.clone());
                    batch.push(RowIndexMutation::Rekey { from, to });
                }
                5..=7 => {
                    let Some(key) = rng.draw(&mut addable, addition_strays, universe) else {
                        continue;
                    };
                    batch.push(RowIndexMutation::Insert {
                        key,
                        fields: value(),
                    });
                }
                _ => {
                    let Some(key) = rng.draw(&mut removable, removal_strays, universe) else {
                        continue;
                    };
                    batch.push(RowIndexMutation::Delete { key: key.clone() });
                    batch.push(RowIndexMutation::Insert {
                        key,
                        fields: value(),
                    });
                }
            }
        }
        if rng.below(2) == 0 {
            batch.sort_by_cached_key(|mutation| {
                let (RowIndexMutation::Delete { key }
                | RowIndexMutation::Insert { key, .. }
                | RowIndexMutation::Rekey { from: key, .. }) = mutation;
                key.canonical_bytes().expect("canonical key")
            });
        }
        batch
    }

    /// Nodes held in memory, so the maintenance pass runs without Git. An
    /// object ID is the SHA-1 of the node's canonical data, so equal nodes
    /// get equal IDs as Git envelopes do. The fuzz rows are a few bytes, so
    /// no value is ever lowered to a byte index.
    #[derive(Clone, Default)]
    struct MemoryNodes(HashMap<NativeOid, NodeData>);

    impl crate::overflow::OverflowStore for MemoryNodes {
        fn oid_width(&self) -> usize {
            GitHashAlgorithm::Sha1.width()
        }

        fn write_node(&mut self, node: &NodeData) -> Result<NativeOid, GraphError> {
            let digest = <sha1::Sha1 as sha1::Digest>::digest(node.encode_canonical()?);
            let oid = NativeOid::new(GitHashAlgorithm::Sha1, digest.as_slice())?;
            self.0.insert(oid.clone(), node.clone());
            Ok(oid)
        }

        fn write_byte_index(&mut self, bytes: &[u8]) -> Result<NativeOid, GraphError> {
            panic!(
                "a {}-byte fuzz value was lowered to a byte index",
                bytes.len()
            )
        }
    }

    /// Seeded random batches (see `fuzz_batch`) over small key universes,
    /// 4,000 of them, run through the pass `maintain_row_index` applies,
    /// `maintain_row_map`, against an in-memory copy of each pinned map. Text
    /// keys order differently as canonical bytes than logically and Int keys
    /// come spelled `Int` and `UInt`. The pass must report exactly the rekeys
    /// (key spelling included) of the one-at-a-time reference and build the
    /// complete build of its rows, or fail with exactly its error.
    #[test]
    fn row_map_pass_matches_the_one_at_a_time_reference() {
        use crate::overflow::OverflowStore;

        let domain = canonical_value_bytes(&CborValue::Array(vec![
            CborValue::Text("rows".to_owned()),
            CborValue::Bytes(vec![0x11; 16]),
            CborValue::Bytes(vec![0x22; 32]),
        ]));
        let mut rng = SplitMix64(0x0374_5eed_0374_5eed);
        let mut value = fuzz_values();
        let (mut ok, mut ok_with_rekeys, mut absent, mut duplicate) = (0, 0, 0, 0);
        for case in 0..4_000 {
            let universe = fuzz_universe(&mut rng, case % 2 == 0);
            let (pinned, free) = fuzz_pinned(&mut rng, &universe, &mut value);
            let batch = fuzz_batch(&mut rng, &universe, &pinned, free, &mut value);

            let mut nodes = MemoryNodes::default();
            let root = complete_row_build(&domain, &pinned, &mut |node| nodes.write_node(node));
            let old = nodes.clone();
            let pass = maintain_row_map(
                &domain,
                root.clone(),
                old.0[&root].clone(),
                pinned.len() as u64,
                &batch,
                &mut |oid| old.0.get(oid).cloned().ok_or(GraphError::UnavailableObject),
                &mut nodes,
            );
            let mut reference_rows = pinned.clone();
            let reference = one_at_a_time_row_index_mutations(&mut reference_rows, &batch);
            match (&pass, &reference) {
                (Ok(pass), Ok(reference_rekeys)) => {
                    // Debug output keeps the Int/UInt spelling equality ignores.
                    assert_eq!(
                        format!("{:?}", pass.logical_rekeys),
                        format!("{reference_rekeys:?}"),
                        "case {case}: {pinned:?} with {batch:?}"
                    );
                    assert_eq!(
                        pass.row_count,
                        reference_rows.len() as u64,
                        "case {case}: {pinned:?} with {batch:?}"
                    );
                    let complete = complete_row_build(&domain, &reference_rows, &mut |node| {
                        nodes.write_node(node)
                    });
                    assert_eq!(
                        pass.maintained.root, complete,
                        "case {case}: {pinned:?} with {batch:?}"
                    );
                    ok += 1;
                    ok_with_rekeys += usize::from(!pass.logical_rekeys.is_empty());
                }
                (Err(error), Err(reference_error)) => {
                    assert_eq!(
                        error, reference_error,
                        "case {case}: {pinned:?} with {batch:?}"
                    );
                    match reference_error {
                        GraphError::RowKeyAbsent => absent += 1,
                        GraphError::DuplicateRowKey => duplicate += 1,
                        other => panic!("case {case}: unexpected {other:?}"),
                    }
                }
                _ => panic!(
                    "case {case}: the pass gave {pass:?} but one at a time {reference:?} \
                     for {pinned:?} with {batch:?}"
                ),
            }
        }
        // Each outcome is exercised, not only the easy one.
        assert!(
            ok >= 400 && ok_with_rekeys >= 100 && absent >= 400 && duplicate >= 400,
            "ok {ok} (with rekeys {ok_with_rekeys}), absent {absent}, duplicate {duplicate}"
        );
    }

    /// Seeded batches as in `row_map_pass_matches_the_one_at_a_time_reference`
    /// through `maintain_row_index` itself, against two pinned Text-keyed
    /// maps in Git: the same error as the one-at-a-time reference, or its
    /// rekeys and the root of the complete build of its rows.
    #[test]
    fn maintain_row_index_matches_the_one_at_a_time_reference() {
        let mut rng = SplitMix64(0x0129_5eed_0129_5eed);
        let mut value = fuzz_values();
        let mut outcomes = BTreeSet::new();
        for _ in 0..2 {
            let (_directory, mut graph) = capture_test_context();
            let universe = fuzz_universe(&mut rng, true);
            let (pinned, free) = fuzz_pinned(&mut rng, &universe, &mut value);
            maintain_and_repin(
                &mut graph,
                &pinned
                    .iter()
                    .map(|(key, fields)| RowIndexMutation::Insert {
                        key: key.clone(),
                        fields: fields.clone(),
                    })
                    .collect::<Vec<_>>(),
            );
            let domain = rows_domain_for_version(graph.row_snapshot.version());
            let mut sink = BTreeSet::new();
            for case in 0..25 {
                let batch = fuzz_batch(&mut rng, &universe, &pinned, free.clone(), &mut value);
                let mut reference_rows = pinned.clone();
                let reference = one_at_a_time_row_index_mutations(&mut reference_rows, &batch);
                let scope = graph.open_read_scope().expect("owner read scope");
                match (graph.maintain_row_index(&batch, &scope), reference) {
                    (Ok(maintenance), Ok(rekeys)) => {
                        assert_eq!(
                            format!("{:?}", maintenance.logical_rekeys()),
                            format!("{rekeys:?}"),
                            "case {case}: {batch:?}"
                        );
                        assert_eq!(
                            maintenance.row_count(),
                            reference_rows.len() as u64,
                            "case {case}: {batch:?}"
                        );
                        let complete = complete_row_build(&domain, &reference_rows, &mut |node| {
                            graph.write_capture_node(node, &mut sink, u64::MAX)
                        });
                        assert_eq!(
                            maintenance.primary_root(),
                            &complete,
                            "case {case}: {batch:?}"
                        );
                        outcomes.insert("ok");
                    }
                    (Err(error), Err(reference_error)) => {
                        assert_eq!(error, reference_error, "case {case}: {batch:?}");
                        outcomes.insert(match error {
                            GraphError::RowKeyAbsent => "absent",
                            GraphError::DuplicateRowKey => "duplicate",
                            other => panic!("case {case}: unexpected {other:?}"),
                        });
                    }
                    (outcome, reference) => panic!(
                        "case {case}: maintain_row_index gave {outcome:?} but one at a time \
                         {reference:?} for {batch:?}"
                    ),
                }
            }
        }
        assert_eq!(
            outcomes.len(),
            3,
            "every outcome is exercised: {outcomes:?}"
        );
    }

    /// ORNA-ROW-006 and store-format-3 §3 on a map two branch levels deep
    /// (A07-storage-rows-04): one update reads the pages on the path to its
    /// row and at most two more, never the table, and its root is the
    /// complete build of the same rows. A page read costs its envelope and
    /// data objects, plus its refs tree when it names dependencies, and every
    /// pass reads the `height` branches on the path, each with refs, so the
    /// objects read are at least twice the pages read plus `height`.
    #[test]
    fn one_update_reads_its_path_and_at_most_two_more_pages() {
        const ROWS: usize = 20_000;
        let (_directory, mut graph) = capture_test_context();
        let key = |index: usize| TypedKey::Text(format!("row-{index:05}"));
        let fields = |index: usize, label: &str| {
            canonical_value_bytes(&CborValue::Array(vec![CborValue::Text(format!(
                "{label} {index}"
            ))]))
        };
        let scope = graph.open_read_scope().expect("seed scope");
        let seeded = graph
            .maintain_row_index(
                &(0..ROWS)
                    .map(|index| RowIndexMutation::Insert {
                        key: key(index),
                        fields: fields(index, "seed"),
                    })
                    .collect::<Vec<_>>(),
                &scope,
            )
            .expect("seed rows");
        adopt_maintenance(&mut graph, &seeded);

        let scope = graph.open_read_scope().expect("root scope");
        let mut objects = ObjectBudget::new(
            scope.max_objects,
            scope.max_objects,
            Arc::clone(&scope.objects_used),
            scope.metadata_quota,
            Arc::clone(&scope.metadata_used),
        );
        let root = graph
            .read_native_node(
                graph.row_snapshot.version().primary_root(),
                &scope,
                &mut objects,
            )
            .expect("read the pinned root");
        let NodeData::OrderedBranch { height, .. } = root else {
            panic!("{ROWS} rows fill more than one page");
        };
        let height = u64::from(height);
        assert!(
            height >= 2,
            "{ROWS} rows build two branch levels, not {height}"
        );

        let changed = 12_345;
        let scope = graph.open_read_scope().expect("update scope");
        let update = graph
            .maintain_row_index(
                &[
                    RowIndexMutation::Delete { key: key(changed) },
                    RowIndexMutation::Insert {
                        key: key(changed),
                        fields: fields(changed, "updated"),
                    },
                ],
                &scope,
            )
            .expect("one update");
        let objects_read = scope.objects_read();
        let pages_read = (objects_read - height) / 2;
        assert!(
            pages_read <= height + 3,
            "one update read {objects_read} objects, so up to {pages_read} pages, \
             in a map {height} branch levels deep"
        );

        let rows = (0..ROWS)
            .map(|index| {
                let label = if index == changed { "updated" } else { "seed" };
                (key(index), fields(index, label))
            })
            .collect::<Vec<_>>();
        let mut sink = BTreeSet::new();
        let complete = complete_row_build(
            &rows_domain_for_version(graph.row_snapshot.version()),
            &rows,
            &mut |node| graph.write_capture_node(node, &mut sink, u64::MAX),
        );
        assert_eq!(update.primary_root(), &complete);
    }

    #[test]
    fn publication_transaction_commit_supports_metadata_lookup_without_payload_read() {
        let (directory, mut graph) = capture_test_context();
        let seed_scope = graph.open_read_scope().expect("owner seed scope");
        let seed_value = vec![0x80];
        let mutations: Vec<_> = (0..300)
            .map(|index| RowIndexMutation::Insert {
                key: TypedKey::Text(format!("row-{index:04}")),
                fields: seed_value.clone(),
            })
            .collect();
        let seeded = graph
            .maintain_row_index(&mutations, &seed_scope)
            .expect("build a branched row map");
        let version = graph.row_snapshot.version();
        let seeded_store_root = graph
            .replace_relation_primary_root(
                &graph.store_root,
                version,
                seeded.primary_root(),
                seeded.row_count(),
                &seed_scope,
            )
            .expect("publish seeded row root into relation map");
        let seeded_snapshot = load_format3_row_map(
            &graph.repository,
            graph.algorithm,
            seeded_store_root.clone(),
            graph.database_id,
            graph.snapshot_id,
            *version.relation_id(),
        )
        .expect("load branched row map from format-3 StoreRoot");
        graph.store_root = seeded_store_root;
        graph.row_snapshot = seeded_snapshot;
        drop(seed_scope);
        let scope = graph.open_read_scope().expect("owner read scope");
        let payload = b"metadata lookup must not hydrate this payload";
        let captured = graph
            .capture_blob_candidate(&payload[..], payload.len() as u64, &scope)
            .expect("capture candidate payload");
        let pin = graph
            .protect_captured_blob(captured, &scope)
            .expect("protect complete OGB-2 closure");
        let identity = pin.content_identity();
        let pin_id = *pin.pin_id();
        let key = TypedKey::Text("row-0150-z".to_owned());
        let callback_key = key.clone();
        let reference = "refs/orna/test-publications/blob-1".to_owned();
        let durable_reference = reference.clone();
        let worktree = directory.path().to_path_buf();
        let (candidate, receipt) = block_on(crate::commit_protected_blob_row(
            &graph,
            pin,
            "text/plain",
            None,
            crate::ProtectedBlobRowInsert::new(key.clone()),
            move |candidate| async move {
                assert_eq!(candidate.key(), &callback_key);
                assert_eq!(candidate.content_identity(), identity);
                assert_eq!(candidate.protected_content_transfer().pin_id(), &pin_id);
                let output = Command::new("git")
                    .current_dir(&worktree)
                    .args([
                        "update-ref",
                        &durable_reference,
                        &candidate.store_root().to_hex(),
                    ])
                    .output()
                    .expect("persist candidate through caller commit callback");
                assert!(output.status.success());
                Ok::<_, &'static str>("committed")
            },
        ))
        .expect("shared publication callback committed");
        assert_eq!(receipt, "committed");
        assert!(fixture_ref_exists(directory.path(), &reference));
        assert!(pending_refs(directory.path()).is_empty());
        let committed = load_format3_row_map(
            &graph.repository,
            graph.algorithm,
            candidate.store_root().clone(),
            candidate.database_id().to_owned(),
            graph.snapshot_id,
            *candidate.relation_id(),
        )
        .expect("durable StoreRoot resolves the inserted relation row map");
        assert_eq!(committed.version().primary_root(), candidate.primary_root());
        let descriptor = candidate
            .protected_content_transfer()
            .descriptor_oid()
            .to_hex();
        let chunk = fixture_git_object(directory.path(), "blob", payload);
        let listing = Command::new("git")
            .current_dir(directory.path())
            .args([
                "ls-tree",
                "-r",
                "-t",
                "--full-tree",
                &candidate.store_root().to_hex(),
            ])
            .output()
            .expect("enumerate published StoreRoot closure");
        assert!(listing.status.success());
        let reachable: BTreeSet<&str> = std::str::from_utf8(&listing.stdout)
            .expect("Git tree listing is UTF-8")
            .lines()
            .filter_map(|line| line.split_ascii_whitespace().nth(2))
            .collect();
        assert!(reachable.contains(descriptor.as_str()));
        assert!(reachable.contains(chunk.to_hex().as_str()));
        assert_eq!(committed.version().row_count(), Some(301));
        let mut root_objects = ObjectBudget::new(
            scope.max_objects,
            scope.max_objects,
            Arc::clone(&scope.objects_used),
            scope.metadata_quota,
            Arc::clone(&scope.metadata_used),
        );
        let candidate_root = graph
            .read_native_node(candidate.primary_root(), &scope, &mut root_objects)
            .expect("read branched candidate root");
        assert!(matches!(candidate_root, NodeData::OrderedBranch { .. }));

        let scope = graph.open_read_scope().expect("fresh owner read scope");
        let metadata = graph
            .lookup_candidate_blob_metadata(&candidate, &key, &scope)
            .expect("metadata-only ORP lookup")
            .expect("inserted row metadata");
        assert_eq!(metadata.content_identity(), identity);
        assert_eq!(metadata.media_type(), "text/plain");
        assert_eq!(metadata.suffix(), None);
        assert_eq!(scope.payload_bytes_read_for_test(), 0);
    }

    #[test]
    fn candidate_row_annotation_must_be_the_admitted_canonical_pair() {
        let (_directory, graph) = capture_test_context();
        let scope = graph.open_read_scope().expect("owner read scope");
        let payload = b"candidate annotation is bound to its pin";
        let candidate = graph
            .capture_blob_candidate(&payload[..], payload.len() as u64, &scope)
            .expect("capture authorized stream");
        let pin = graph
            .protect_captured_blob(candidate, &scope)
            .expect("protect complete OGB-2 closure");
        let transfer = pin.transfer_record();
        let identity = transfer.content_identity();

        let stored_row = |media_type: &str, suffix: Option<&str>| {
            let mut row = Vec::new();
            array(&mut row, 1);
            cbor_head(&mut row, 6, 60111);
            array(&mut row, 5);
            uint(&mut row, identity.length());
            bytes(&mut row, &identity.sha256());
            cbor_head(&mut row, 3, media_type.len() as u64);
            row.extend_from_slice(media_type.as_bytes());
            match suffix {
                Some(suffix) => {
                    cbor_head(&mut row, 3, suffix.len() as u64);
                    row.extend_from_slice(suffix.as_bytes());
                }
                None => null(&mut row),
            }
            bytes(&mut row, transfer.descriptor_oid().as_bytes());
            row
        };

        let admitted = decode_candidate_blob_metadata(
            &stored_row("text/plain", None),
            &transfer,
            "text/plain",
            None,
        )
        .expect("the admitted annotation binds the candidate row");
        assert_eq!(admitted.media_type(), "text/plain");
        assert_eq!(admitted.suffix(), None);
        assert_eq!(admitted.content_identity(), identity);

        let annotated = decode_candidate_blob_metadata(
            &stored_row("audio/mpeg", Some("mp2")),
            &transfer,
            "audio/mpeg",
            Some("mp2"),
        )
        .expect("a matching annotated pair binds the candidate row");
        assert_eq!(annotated.media_type(), "audio/mpeg");
        assert_eq!(annotated.suffix(), Some("mp2"));

        assert!(matches!(
            decode_candidate_blob_metadata(
                &stored_row("TEXT/PLAIN", None),
                &transfer,
                "TEXT/PLAIN",
                None,
            ),
            Err(GraphError::InvalidBlobAnnotation)
        ));
        // An explicit suffix equal to the type's default is parseable but not
        // canonical MIME-1, so it cannot bind even when the pin admits it.
        assert!(matches!(
            decode_candidate_blob_metadata(
                &stored_row("text/plain", Some("txt")),
                &transfer,
                "text/plain",
                Some("txt"),
            ),
            Err(GraphError::InvalidBlobAnnotation)
        ));
        assert!(matches!(
            decode_candidate_blob_metadata(
                &stored_row("image/png", None),
                &transfer,
                "text/plain",
                None,
            ),
            Err(GraphError::InvalidBlobAnnotation)
        ));
        assert!(matches!(
            decode_candidate_blob_metadata(
                &stored_row("audio/mpeg", Some("mp2")),
                &transfer,
                "text/plain",
                None,
            ),
            Err(GraphError::InvalidBlobAnnotation)
        ));
        assert!(matches!(
            decode_candidate_blob_metadata(
                &stored_row("text/plain", None),
                &transfer,
                "text/plain",
                Some("txt"),
            ),
            Err(GraphError::InvalidBlobAnnotation)
        ));
    }

    #[test]
    fn protected_pin_acceptance_rejects_a_tampered_descriptor() {
        let (_directory, graph) = capture_test_context();
        let scope = graph.open_read_scope().expect("owner read scope");
        let payload = b"tampered pin descriptor must not bind a row";
        let candidate = graph
            .capture_blob_candidate(&payload[..], payload.len() as u64, &scope)
            .expect("capture authorized stream");
        let mut pin = graph
            .protect_captured_blob(candidate, &scope)
            .expect("protect complete OGB-2 closure");
        let mut tampered_oid = pin.descriptor_oid.as_bytes().to_vec();
        tampered_oid[0] ^= 1;
        pin.descriptor_oid = NativeOid::new(graph.algorithm, tampered_oid)
            .expect("valid-width tampered OID");

        assert!(matches!(
            graph.accept_protected_blob_pin(pin),
            Err(GraphError::InvalidProtectedRef)
        ));
    }

    #[test]
    fn accepted_blob_pin_admits_its_own_descriptor() {
        let (_directory, graph) = capture_test_context();
        let scope = graph.open_read_scope().expect("owner read scope");
        let payload = b"accepted pin admits its own Blob";
        let candidate = graph
            .capture_blob_candidate(&payload[..], payload.len() as u64, &scope)
            .expect("capture authorized stream");
        let pin = graph
            .protect_captured_blob(candidate, &scope)
            .expect("protect complete OGB-2 closure");

        let admitted = graph
            .admit_accepted_blob_pin(
                pin.pin_id(),
                &pin.descriptor_oid,
                pin.content_identity(),
                &scope,
            )
            .expect("admit the accepted pin");

        assert_eq!(admitted.descriptor_oid(), &pin.descriptor_oid);
        assert_eq!(admitted.content_identity(), pin.content_identity());
    }

    #[test]
    fn accepted_blob_pin_admission_rejects_a_different_descriptor() {
        let (_directory, graph) = capture_test_context();
        let scope = graph.open_read_scope().expect("owner read scope");
        let payload = b"accepted pin names exactly one descriptor";
        let candidate = graph
            .capture_blob_candidate(&payload[..], payload.len() as u64, &scope)
            .expect("capture authorized stream");
        let pin = graph
            .protect_captured_blob(candidate, &scope)
            .expect("protect complete OGB-2 closure");
        let mut tampered_oid = pin.descriptor_oid.as_bytes().to_vec();
        tampered_oid[0] ^= 1;
        let tampered_oid =
            NativeOid::new(graph.algorithm, tampered_oid).expect("valid-width tampered OID");

        assert!(matches!(
            graph.admit_accepted_blob_pin(
                pin.pin_id(),
                &tampered_oid,
                pin.content_identity(),
                &scope,
            ),
            Err(GraphError::InvalidProtectedRef)
        ));
    }

    #[test]
    fn accepted_blob_pin_refuses_an_unaccepted_candidate() {
        let (directory, graph) = capture_test_context();
        let scope = graph.open_read_scope().expect("owner read scope");
        let payload = b"candidate has not been accepted yet";
        let candidate = graph
            .capture_blob_candidate(&payload[..], payload.len() as u64, &scope)
            .expect("capture authorized stream");
        let pin_id = candidate.pin_id;
        let descriptor_oid = candidate.descriptor_oid.clone();
        let identity = candidate.content_identity();
        let pending_ref = scratch_content_ref(&graph.owner_id, &pin_id);
        assert!(fixture_ref_exists(directory.path(), &pending_ref));

        assert!(matches!(
            graph.admit_accepted_blob_pin(&pin_id, &descriptor_oid, identity, &scope),
            Err(GraphError::InvalidProtectedRef)
        ));
        assert!(fixture_ref_exists(directory.path(), &pending_ref));
    }

    #[test]
    fn accepted_blob_pin_admission_rejects_a_foreign_scope() {
        let (_directory, graph) = capture_test_context();
        let scope = graph.open_read_scope().expect("owner read scope");
        let payload = b"accepted pin is bound to its owner scope";
        let candidate = graph
            .capture_blob_candidate(&payload[..], payload.len() as u64, &scope)
            .expect("capture authorized stream");
        let pin = graph
            .protect_captured_blob(candidate, &scope)
            .expect("protect complete OGB-2 closure");
        // Fixture graphs share one identity and owner, so a foreign scope is
        // modelled by overriding each bound field in turn.
        let mut foreign_context = graph.open_read_scope().expect("owner read scope");
        foreign_context.context = ContextIdentity([0x7a; 32]);
        let mut foreign_owner = graph.open_read_scope().expect("owner read scope");
        foreign_owner.owner_id = [0x7b; 16];

        for foreign in [&foreign_context, &foreign_owner] {
            assert!(matches!(
                graph.admit_accepted_blob_pin(
                    pin.pin_id(),
                    &pin.descriptor_oid,
                    pin.content_identity(),
                    foreign,
                ),
                Err(GraphError::ContextMismatch)
            ));
        }
    }

    #[test]
    fn accepted_blob_pin_admission_rejects_a_mismatched_identity() {
        let (_directory, graph) = capture_test_context();
        let scope = graph.open_read_scope().expect("owner read scope");
        let payload = b"accepted pin identity must match its descriptor";
        let candidate = graph
            .capture_blob_candidate(&payload[..], payload.len() as u64, &scope)
            .expect("capture authorized stream");
        let pin = graph
            .protect_captured_blob(candidate, &scope)
            .expect("protect complete OGB-2 closure");

        assert!(matches!(
            graph.admit_accepted_blob_pin(
                pin.pin_id(),
                &pin.descriptor_oid,
                crate::blob_store::digest_bytes(b"a different payload"),
                &scope,
            ),
            Err(GraphError::ContentIdentityMismatch)
        ));
    }

    /// Issues the graph a mounted editor opens over `derived`'s snapshot: the
    /// same repository, identity, snapshot and row map, owned by the runtime
    /// lease owner instead of the repository-derived default.
    fn lease_owned_graph(derived: &NativeGraphContext, owner_id: [u8; 16]) -> NativeGraphContext {
        NativeGraphContext::issue(
            derived.repository.clone(),
            derived.repository_id,
            derived.identity.0,
            derived.database_id,
            owner_id,
            derived.snapshot_id,
            derived.algorithm,
            derived.store_root.clone(),
            derived.schema_digest,
            derived.row_snapshot.clone(),
        )
        .expect("issue the lease-owned graph over the derived snapshot")
    }

    #[test]
    fn accepted_blob_pin_lease_graph_admits_a_pin_captured_under_the_derived_owner() {
        let (_directory, derived) = capture_test_context();
        let derived_scope = derived.open_read_scope().expect("derived read scope");
        let payload = b"pin captured under the derived owner";
        let candidate = derived
            .capture_blob_candidate(&payload[..], payload.len() as u64, &derived_scope)
            .expect("capture authorized stream");
        let pin = derived
            .protect_captured_blob(candidate, &derived_scope)
            .expect("protect complete OGB-2 closure");
        let lease = lease_owned_graph(&derived, [0x5c; 16]);
        let scope = lease.open_read_scope().expect("lease read scope");

        assert!(matches!(
            lease.admit_accepted_blob_pin(
                pin.pin_id(),
                &pin.descriptor_oid,
                pin.content_identity(),
                &scope,
            ),
            Err(GraphError::InvalidProtectedRef)
        ));

        let fallback = lease.with_accepted_pin_fallback_owner(derived.owner_id);
        let admitted = fallback
            .admit_accepted_blob_pin(
                pin.pin_id(),
                &pin.descriptor_oid,
                pin.content_identity(),
                &scope,
            )
            .expect("admit the derived-owner pin through the fallback");
        assert_eq!(admitted.descriptor_oid(), &pin.descriptor_oid);
        assert_eq!(admitted.content_identity(), pin.content_identity());
    }

    #[test]
    fn accepted_blob_pin_lease_graph_admits_its_own_pin_with_the_derived_fallback_set() {
        let (_directory, derived) = capture_test_context();
        let lease = lease_owned_graph(&derived, [0x5c; 16]);
        let scope = lease.open_read_scope().expect("lease read scope");
        let payload = b"pin captured under the lease owner";
        let candidate = lease
            .capture_blob_candidate(&payload[..], payload.len() as u64, &scope)
            .expect("capture authorized stream");
        let pin = lease
            .protect_captured_blob(candidate, &scope)
            .expect("protect complete OGB-2 closure");
        let fallback = lease.with_accepted_pin_fallback_owner(derived.owner_id);

        let admitted = fallback
            .admit_accepted_blob_pin(
                pin.pin_id(),
                &pin.descriptor_oid,
                pin.content_identity(),
                &scope,
            )
            .expect("admit the lease-owned pin before the fallback");
        assert_eq!(admitted.descriptor_oid(), &pin.descriptor_oid);
    }

    #[test]
    fn accepted_blob_pin_lease_graph_refuses_a_pin_held_by_another_lease_owner() {
        let (_directory, derived) = capture_test_context();
        let owner_b = lease_owned_graph(&derived, [0xb0; 16]);
        let scope_b = owner_b.open_read_scope().expect("lease B read scope");
        let payload = b"pin captured under lease owner B";
        let candidate = owner_b
            .capture_blob_candidate(&payload[..], payload.len() as u64, &scope_b)
            .expect("capture authorized stream");
        let pin = owner_b
            .protect_captured_blob(candidate, &scope_b)
            .expect("protect complete OGB-2 closure");
        owner_b
            .admit_accepted_blob_pin(
                pin.pin_id(),
                &pin.descriptor_oid,
                pin.content_identity(),
                &scope_b,
            )
            .expect("lease B admits its own pin");

        let owner_a = lease_owned_graph(&derived, [0xa0; 16]);
        let scope_a = owner_a.open_read_scope().expect("lease A read scope");
        assert!(matches!(
            owner_a.admit_accepted_blob_pin(
                pin.pin_id(),
                &pin.descriptor_oid,
                pin.content_identity(),
                &scope_a,
            ),
            Err(GraphError::InvalidProtectedRef)
        ));
        // The derived fallback holds no ref for lease B's pin, so it cannot admit it either.
        let with_fallback = owner_a.with_accepted_pin_fallback_owner(derived.owner_id);
        assert!(matches!(
            with_fallback.admit_accepted_blob_pin(
                pin.pin_id(),
                &pin.descriptor_oid,
                pin.content_identity(),
                &scope_a,
            ),
            Err(GraphError::InvalidProtectedRef)
        ));
    }

    #[test]
    fn accepted_blob_pin_own_ref_naming_another_descriptor_is_refused_without_fallback() {
        let (directory, derived) = capture_test_context();
        let derived_scope = derived.open_read_scope().expect("derived read scope");
        let payload = b"pin whose lease-owned ref names another descriptor";
        let candidate = derived
            .capture_blob_candidate(&payload[..], payload.len() as u64, &derived_scope)
            .expect("capture authorized stream");
        let pin = derived
            .protect_captured_blob(candidate, &derived_scope)
            .expect("protect complete OGB-2 closure");
        let other_payload = b"a different Blob supplies the mismatched descriptor";
        let other_candidate = derived
            .capture_blob_candidate(&other_payload[..], other_payload.len() as u64, &derived_scope)
            .expect("capture authorized stream");
        let other = derived
            .protect_captured_blob(other_candidate, &derived_scope)
            .expect("protect complete OGB-2 closure");
        assert_ne!(other.descriptor_oid, pin.descriptor_oid);

        // The lease owner's own accepted ref names a different descriptor. The derived
        // fallback still holds the matching ref, and must not be consulted.
        let lease_owner = [0x5c; 16];
        let own_ref = accepted_content_ref(&lease_owner, pin.pin_id());
        let written = Command::new("git")
            .current_dir(directory.path())
            .args(["update-ref", own_ref.as_str(), other.descriptor_oid.to_hex().as_str()])
            .status()
            .expect("write the lease owner's mismatched accepted ref");
        assert!(written.success(), "write {own_ref}");

        let lease = lease_owned_graph(&derived, lease_owner);
        let lease_scope = lease.open_read_scope().expect("lease read scope");
        let with_fallback = lease_owned_graph(&derived, lease_owner)
            .with_accepted_pin_fallback_owner(derived.owner_id);
        for graph in [&lease, &with_fallback] {
            assert!(matches!(
                graph.admit_accepted_blob_pin(
                    pin.pin_id(),
                    &pin.descriptor_oid,
                    pin.content_identity(),
                    &lease_scope,
                ),
                Err(GraphError::InvalidProtectedRef)
            ));
        }
        derived
            .admit_accepted_blob_pin(
                pin.pin_id(),
                &pin.descriptor_oid,
                pin.content_identity(),
                &derived_scope,
            )
            .expect("the derived owner still admits its own matching pin");
    }

    #[test]
    fn accepted_blob_pin_reclaimed_after_its_owner_departed_is_refused() {
        let (directory, derived) = capture_test_context();
        let departed = lease_owned_graph(&derived, [0xd0; 16]);
        let departed_scope = departed.open_read_scope().expect("departed owner read scope");
        let payload = b"pin captured under an owner that departs";
        let candidate = departed
            .capture_blob_candidate(&payload[..], payload.len() as u64, &departed_scope)
            .expect("capture authorized stream");
        let pin = departed
            .protect_captured_blob(candidate, &departed_scope)
            .expect("protect complete OGB-2 closure");
        let successor = lease_owned_graph(&derived, [0x5c; 16])
            .with_accepted_pin_fallback_owner(departed.owner_id);
        let successor_scope = successor.open_read_scope().expect("successor read scope");
        successor
            .admit_accepted_blob_pin(
                pin.pin_id(),
                &pin.descriptor_oid,
                pin.content_identity(),
                &successor_scope,
            )
            .expect("the departed owner's accepted pin admits before reclamation");

        // An empty manifest names no accepted pin, so recovery deletes the departed ref.
        reclaim_dead_owner_pin_refs(&departed.repository, departed.owner_id, &BTreeMap::new())
            .expect("reclaim the departed owner's pins");
        assert!(!fixture_ref_exists(
            directory.path(),
            &accepted_content_ref(&departed.owner_id, pin.pin_id()),
        ));
        assert!(matches!(
            successor.admit_accepted_blob_pin(
                pin.pin_id(),
                &pin.descriptor_oid,
                pin.content_identity(),
                &successor_scope,
            ),
            Err(GraphError::InvalidProtectedRef)
        ));
        assert!(matches!(
            departed.admit_accepted_blob_pin(
                pin.pin_id(),
                &pin.descriptor_oid,
                pin.content_identity(),
                &departed_scope,
            ),
            Err(GraphError::InvalidProtectedRef)
        ));
    }

    #[test]
    fn pin_description_round_trips_the_captured_fixture_bytes() {
        let (_directory, graph) = capture_test_context();
        let scope = graph.open_read_scope().expect("owner read scope");
        let payload: &[u8] = include_bytes!("../tests/fixtures/pin-roundtrip.orna");
        let candidate = graph
            .capture_blob_candidate(payload, payload.len() as u64, &scope)
            .expect("capture authorized fixture stream");
        let pin = graph
            .protect_captured_blob(candidate, &scope)
            .expect("protect complete OGB-2 closure");

        let json = crate::publication_describe::describe_pin_json(&pin);
        let sha256 = Sha256::digest(payload);
        let sha256_hex: String = sha256.iter().map(|byte| format!("{byte:02x}")).collect();
        assert!(json.contains(&format!("\"length\":{}", payload.len())));
        assert!(json.contains(&format!("\"sha256\":\"{sha256_hex}\"")));
        assert_eq!(
            json,
            crate::publication_describe::describe_transfer_json(&pin.transfer_record()),
            "the pin description must come from its transfer record"
        );
        graph
            .accept_protected_blob_pin(pin)
            .expect("accept the described pin");
    }

/// ORNA-CAP-005: cancellation removes only unaccepted scratch and does not admit a dependent row.
    #[test]
    fn cancelled_captured_candidate_cleans_its_provisional_ref() {
        let (directory, graph) = capture_test_context();
        let scope = graph.open_read_scope().expect("owner read scope");
        let payload = b"candidate rejected before acceptance";
        let candidate = graph
            .capture_blob_candidate(&payload[..], payload.len() as u64, &scope)
            .expect("capture authorized stream");
        let pending_ref = scratch_content_ref(&graph.owner_id, &candidate.pin_id);
        assert!(fixture_ref_exists(directory.path(), &pending_ref));

        graph.cancel_reads();
        assert!(matches!(
            graph.protect_captured_blob(candidate, &scope),
            Err(GraphError::ReadCancelled)
        ));
        assert!(!fixture_ref_exists(directory.path(), &pending_ref));
    }

    #[test]
    fn wrong_context_rejection_cleans_issuing_candidate_ref() {
        let (issuing_directory, issuing_graph) = capture_test_context();
        let (_other_directory, other_graph) = capture_test_context();
        let issuing_scope = issuing_graph.open_read_scope().expect("issuing read scope");
        let other_scope = other_graph.open_read_scope().expect("other read scope");
        let payload = b"candidate belongs to issuing graph";
        let candidate = issuing_graph
            .capture_blob_candidate(&payload[..], payload.len() as u64, &issuing_scope)
            .expect("capture candidate under issuing graph");
        let pending_ref = scratch_content_ref(&issuing_graph.owner_id, &candidate.pin_id);
        assert!(fixture_ref_exists(issuing_directory.path(), &pending_ref));

        assert!(matches!(
            other_graph.protect_captured_blob(candidate, &other_scope),
            Err(GraphError::ContextMismatch)
        ));
        assert!(!fixture_ref_exists(issuing_directory.path(), &pending_ref));
    }

    #[test]
    fn abandoned_candidate_drops_its_provisional_ref() {
        let (directory, graph) = capture_test_context();
        let scope = graph.open_read_scope().expect("owner read scope");
        let payload = b"candidate abandoned before acceptance";
        let candidate = graph
            .capture_blob_candidate(&payload[..], payload.len() as u64, &scope)
            .expect("capture candidate");
        let pending_ref = scratch_content_ref(&graph.owner_id, &candidate.pin_id);
        assert!(fixture_ref_exists(directory.path(), &pending_ref));

        drop(candidate);

        assert!(!fixture_ref_exists(directory.path(), &pending_ref));
    }

/// ORNA-CAP-006: reclamation coordinates with publication ownership before deleting protected Git objects.
    #[test]
    fn owner_pin_reclaim_defers_across_publication_journal_interleaving() {
        let (directory, graph) = capture_test_context();
        let root = directory.path();
        let repository = crate::Repository::discover(root).expect("discover capture repository");
        let scope = graph.open_read_scope().expect("owner read scope");

        let accepted_payload = b"accepted graph pin retained through publication";
        let accepted_candidate = graph
            .capture_blob_candidate(&accepted_payload[..], accepted_payload.len() as u64, &scope)
            .expect("capture accepted payload");
        let accepted_pin = graph
            .protect_captured_blob(accepted_candidate, &scope)
            .expect("protect accepted payload");
        let accepted_ref = accepted_pin.protected_ref.clone();
        let accepted_pending = BTreeMap::from([(
            *accepted_pin.pin_id(),
            accepted_pin.descriptor_oid.clone(),
        )]);

        let unaccepted_payload = b"unaccepted graph pin awaiting safe cleanup";
        let unaccepted_candidate = graph
            .capture_blob_candidate(&unaccepted_payload[..], unaccepted_payload.len() as u64, &scope)
            .expect("capture unaccepted payload");
        let unaccepted_pin = graph
            .protect_captured_blob(unaccepted_candidate, &scope)
            .expect("protect unaccepted payload");
        let unaccepted_ref = unaccepted_pin.protected_ref.clone();

        let commit = |message: &str| {
            let output = Command::new("git")
                .current_dir(root)
                .args([
                    "-c",
                    "user.name=kierandrewett",
                    "-c",
                    "user.email=kieran@drewett.dev",
                    "commit",
                    "--allow-empty",
                    "-m",
                    message,
                ])
                .output()
                .expect("create journal fixture commit");
            assert!(
                output.status.success(),
                "create journal fixture commit: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        commit("journal baseline");
        let old_head = repository
            .head()
            .expect("read journal baseline")
            .expect("journal baseline commit");
        commit("journal candidate");
        let new_head = repository
            .head()
            .expect("read journal candidate")
            .expect("journal candidate commit");
        let journal = crate::PublicationJournal::new(
            old_head,
            new_head,
            vec![crate::PublicationJournalEntry::new(
                crate::ManagedPath::new("pending.orna").expect("managed fixture path"),
                None,
                Some(b"candidate".to_vec()),
            )],
        )
        .expect("valid publication journal");

        let publication_lock = NativeGraphCoordinationLock::acquire(&repository)
            .expect("hold publication coordination lock");
        let worker_repository = repository.clone();
        let worker_accepted = accepted_pending.clone();
        let dead_owner = graph.owner_id;
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            started_tx.send(()).expect("signal cleanup attempt");
            reclaim_dead_owner_pin_refs(&worker_repository, dead_owner, &worker_accepted)
        });
        started_rx.recv().expect("cleanup attempt started");
        assert_eq!(
            worker.join().expect("join concurrent cleanup"),
            Err(GraphError::PublicationNotTerminal),
            "cleanup must defer while a publisher holds the shared lock"
        );

        repository
            .write_publication_journal_locked(&journal)
            .expect("persist journal under the held publication lock");
        drop(publication_lock);

        assert_eq!(
            reclaim_dead_owner_pin_refs(&repository, graph.owner_id, &accepted_pending),
            Err(GraphError::PublicationNotTerminal),
            "cleanup must recheck the durable journal while holding the lock"
        );
        assert!(fixture_ref_exists(root, &accepted_ref));
        assert!(fixture_ref_exists(root, &unaccepted_ref));

        repository
            .clear_publication_journal()
            .expect("clear completed fixture journal");
        assert_eq!(
            reclaim_dead_owner_pin_refs(&repository, graph.owner_id, &accepted_pending),
            Ok(1),
            "retry removes only the unaccepted pin"
        );
        assert!(fixture_ref_exists(root, &accepted_ref));
        assert!(!fixture_ref_exists(root, &unaccepted_ref));
    }

    /// Reclaims several departed owners in one pass, retaining accepted pins.
    #[test]
    fn departed_owner_reclaim_settles_all_owners_in_one_pass() {
        let directory = tempfile::tempdir().expect("create reclaim repository");
        let root = directory.path();
        let init = Command::new("git")
            .current_dir(root)
            .args(["init", "--quiet", "--template="])
            .status()
            .expect("initialize reclaim repository");
        assert!(init.success());
        let repository = crate::Repository::discover(root).expect("discover reclaim repository");

        fs::write(root.join("pin-payload"), b"departed owner pin payload")
            .expect("write pin payload");
        let output = Command::new("git")
            .current_dir(root)
            .args(["hash-object", "-w", "pin-payload"])
            .output()
            .expect("hash pin payload");
        assert!(
            output.status.success(),
            "hash pin payload: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let payload_hex = String::from_utf8(output.stdout)
            .expect("hash output is UTF-8")
            .trim()
            .to_owned();
        let payload_oid =
            NativeOid::from_hex(GitHashAlgorithm::Sha1, &payload_hex).expect("payload object id");

        let owner_a = [0xA0; 16];
        let owner_b = [0xB0; 16];
        let owner_c = [0xC0; 16];
        let pins = [
            (owner_a, [0xA1; 16]),
            (owner_a, [0xA2; 16]),
            (owner_b, [0xB1; 16]),
            (owner_c, [0xC1; 16]),
            (owner_c, [0xC2; 16]),
        ];
        for (owner, pin_id) in pins {
            let reference = accepted_content_ref(&owner, &pin_id);
            let status = Command::new("git")
                .current_dir(root)
                .args(["update-ref", reference.as_str(), payload_hex.as_str()])
                .status()
                .expect("create departed owner pin ref");
            assert!(status.success(), "create {reference}");
        }
        let exists = |owner: [u8; 16], pin_id: [u8; 16]| {
            fixture_ref_exists(root, &accepted_content_ref(&owner, &pin_id))
        };

        let accepted_a = BTreeMap::from([([0xA1; 16], payload_oid.clone())]);
        let accepted_c = BTreeMap::from([([0xC1; 16], payload_oid.clone())]);
        let accepted_none: BTreeMap<[u8; 16], NativeOid> = BTreeMap::new();
        let departed = [
            (owner_a, &accepted_a),
            (owner_b, &accepted_none),
            (owner_c, &accepted_c),
        ];
        let publication_lock = NativeGraphCoordinationLock::acquire(&repository)
            .expect("hold coordination lock through another handle");
        let _inherited_file = publication_lock
            ._file
            .try_clone()
            .expect("duplicate lock descriptor as a forked child would");
        assert_eq!(
            reclaim_departed_owner_pin_refs(&repository, &departed),
            Err(GraphError::PublicationNotTerminal),
            "busy coordination lock defers the whole pass"
        );
        assert!(pins.iter().all(|(owner, pin_id)| exists(*owner, *pin_id)));
        drop(publication_lock);


        assert_eq!(
            reclaim_departed_owner_pin_refs(&repository, &departed),
            Ok(3),
            "one pass removes each owner's unaccepted pin"
        );
        assert!(exists(owner_a, [0xA1; 16]));
        assert!(!exists(owner_a, [0xA2; 16]));
        assert!(!exists(owner_b, [0xB1; 16]));
        assert!(exists(owner_c, [0xC1; 16]));
        assert!(!exists(owner_c, [0xC2; 16]));
    }

    const KILLED_OWNER_ROOT: &str = "ORNA_DEAD_OWNER_CAPTURE_ROOT";
    const KILLED_OWNER_TEST: &str =
        "native_graph::persisted_orp_tests::restart_preserves_accepted_pin_and_reclaims_unaccepted_pin";

    /// Captures two protected fixtures, then kills this process without
    /// unwinding, exactly as an operator `SIGKILL` does.
    fn killed_owner_child_body(root: &Path) {
        let graph = capture_context_in(root);
        let repository =
            crate::Repository::discover(root).expect("discover killed-owner repository");
        let _owner_lock = repository
            .acquire_runtime_owner_lock(graph.owner_id)
            .expect("killed owner acquires its lock");
        let scope = graph.open_read_scope().expect("killed owner read scope");
        let accepted_payload: &[u8] = include_bytes!("../tests/fixtures/pin-roundtrip.orna");
        let accepted_candidate = graph
            .capture_blob_candidate(accepted_payload, accepted_payload.len() as u64, &scope)
            .expect("capture accepted fixture stream");
        let accepted_pin = graph
            .protect_captured_blob(accepted_candidate, &scope)
            .expect("protect accepted OGB-2 closure");
        let unaccepted_payload = b"captured but not accepted before process death";
        let unaccepted_candidate = graph
            .capture_blob_candidate(&unaccepted_payload[..], unaccepted_payload.len() as u64, &scope)
            .expect("capture unaccepted fixture stream");
        let unaccepted_pin = graph
            .protect_captured_blob(unaccepted_candidate, &scope)
            .expect("protect unaccepted OGB-2 closure");
        println!(
            "{} {} {} {}",
            hex_encode(&graph.owner_id),
            hex_encode(accepted_pin.pin_id()),
            accepted_pin.descriptor_oid.to_hex(),
            hex_encode(unaccepted_pin.pin_id()),
        );
        std::io::stdout().flush().expect("flush pin evidence");
        let status = Command::new("kill")
            .args(["-9", &std::process::id().to_string()])
            .status()
            .expect("raise SIGKILL on the owner");
        unreachable!("the killed owner never returns ({status})");
    }

    /// Names every ref under one owner's private pin namespace.
    fn pin_ref_names(directory: &Path, owner_hex: &str) -> Vec<String> {
        let prefix = format!("refs/orna/pins/{owner_hex}/");
        let output = Command::new("git")
            .current_dir(directory)
            .args(["for-each-ref", "--format=%(refname)", &prefix])
            .output()
            .expect("list pin refs");
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .expect("ref listing is UTF-8")
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect()
    }

    fn decode_pin_id(hex: &str) -> [u8; 16] {
        let mut pin_id = [0; 16];
        for (index, pair) in hex.as_bytes().chunks_exact(2).enumerate() {
            pin_id[index] = u8::from_str_radix(
                std::str::from_utf8(pair).expect("pin ID is ASCII"),
                16,
            )
            .expect("pin ID is hexadecimal");
        }
        pin_id
    }

    /// Restart must keep refs named by durable accepted-pin evidence, reclaim
    /// unaccepted refs, and still read the accepted OGB-2 bytes after reopen.
/// ORNA-CAP-004: restart recovery distinguishes accepted pending pins from unaccepted scratch.
    #[test]
    fn restart_preserves_accepted_pin_and_reclaims_unaccepted_pin() {
        if let Some(root) = std::env::var_os(KILLED_OWNER_ROOT) {
            killed_owner_child_body(Path::new(&root));
            return;
        }

        let directory = tempfile::tempdir().expect("create killed-owner repository");
        let root = directory.path();
        let child = Command::new(std::env::current_exe().expect("resolve test binary"))
            .args([
                "--exact",
                KILLED_OWNER_TEST,
                "--nocapture",
                "--test-threads=1",
            ])
            .env(KILLED_OWNER_ROOT, root.as_os_str())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn the killed owner");
        let output = child.wait_with_output().expect("await the killed owner");
        assert_eq!(
            output.status.code(),
            None,
            "the owner must die by signal, not exit normally"
        );

        let evidence = String::from_utf8(output.stdout).expect("owner evidence is UTF-8");
        // The child shares the libtest harness, which writes the test name and
        // status before the body runs. Scan tokens for its four-value record.
        let (owner_hex, accepted_pin_hex, descriptor_hex, unaccepted_pin_hex) = evidence
            .split_whitespace()
            .collect::<Vec<_>>()
            .windows(4)
            .find_map(|tokens| {
                let [owner, pin, descriptor, unaccepted] = tokens else {
                    return None;
                };
                let hex_token = |token: &str, width: usize| {
                    token.len() == width && token.bytes().all(|byte| byte.is_ascii_hexdigit())
                };
                (hex_token(owner, 32)
                    && hex_token(pin, 32)
                    && hex_token(descriptor, 40)
                    && hex_token(unaccepted, 32))
                .then(|| {
                    (
                        (*owner).to_owned(),
                        (*pin).to_owned(),
                        (*descriptor).to_owned(),
                        (*unaccepted).to_owned(),
                    )
                })
            })
            .expect("the killed owner prints pin evidence before dying");

        assert_eq!(pin_ref_names(root, &owner_hex).len(), 2);
        let accepted_pin_id = decode_pin_id(&accepted_pin_hex);
        let unaccepted_pin_id = decode_pin_id(&unaccepted_pin_hex);
        let accepted_oid = NativeOid::from_hex(GitHashAlgorithm::Sha1, &descriptor_hex)
            .expect("accepted descriptor OID");
        let accepted_pending = BTreeMap::from([(accepted_pin_id, accepted_oid.clone())]);

        // A sibling namespace must never be reclaimed by this takeover.
        let foreign_owner = [0x99; 16];
        let foreign_ref = accepted_content_ref(&foreign_owner, &[0x77; 16]);
        let zero = "0".repeat(40);
        let foreign_created = Command::new("git")
            .current_dir(root)
            .args(["update-ref", &foreign_ref, &descriptor_hex, &zero])
            .status()
            .expect("create foreign owner pin ref");
        assert!(foreign_created.success());

        let repository =
            crate::Repository::discover(root).expect("discover killed-owner repository");
        let second = repository
            .acquire_runtime_owner_lock([0x42; 16])
            .expect("takeover after the owner was killed");
        assert_eq!(second.previous_owner(), Some(decode_pin_id(&owner_hex)));
        assert_eq!(second.reclaim_previous_owner_pins(&accepted_pending).unwrap(), 1);
        let accepted_ref = accepted_content_ref(&decode_pin_id(&owner_hex), &accepted_pin_id);
        assert_eq!(pin_ref_names(root, &owner_hex), vec![accepted_ref]);
        assert!(
            !fixture_ref_exists(
                root,
                &accepted_content_ref(&decode_pin_id(&owner_hex), &unaccepted_pin_id),
            ),
            "unaccepted pending content is reclaimed"
        );

        let gc = Command::new("git")
            .current_dir(root)
            .args(["gc", "--prune=now", "--quiet"])
            .output()
            .expect("collect objects after owner recovery");
        assert!(
            gc.status.success(),
            "git gc failed after recovery: {}",
            String::from_utf8_lossy(&gc.stderr)
        );

        // The remaining private root must still serve its full OGB-2 content
        // after constructing a fresh graph context over the reopened repo.
        let reopened_graph = capture_context_in(root);
        let scope = reopened_graph.open_read_scope().unwrap();
        let mut objects = ObjectBudget::new(
            scope.max_objects.min(MAX_RANGE_GRAPH_OBJECTS),
            scope.max_objects,
            Arc::clone(&scope.objects_used),
            scope.metadata_quota.min(MAX_RANGE_METADATA_BYTES),
            Arc::clone(&scope.metadata_used),
        );
        let descriptor = reopened_graph
            .read_blob_descriptor(&accepted_oid, &scope, &mut objects)
            .unwrap();
        let reference = reopened_graph.admit_blob(descriptor).unwrap();
        let payload: &[u8] = include_bytes!("../tests/fixtures/pin-roundtrip.orna");
        let range = reopened_graph
            .read_blob_range(&reference, 0..payload.len() as u64, &scope)
            .unwrap();
        assert_eq!(range.bytes(), payload);

        assert_eq!(
            pin_ref_names(root, &hex_encode(&foreign_owner)),
            vec![foreign_ref],
            "reclamation must stay inside the killed owner's pin namespace"
        );
    }

    /// Publishes `keys` as a multi-page row map under the relation's primary
    /// root, so range reads run through this graph's production read path.
    fn publish_row_map(graph: &mut NativeGraphContext, keys: impl IntoIterator<Item = TypedKey>) {
        let seed_scope = graph.open_read_scope().expect("owner seed scope");
        let seed_value = vec![0x80];
        let mutations: Vec<_> = keys
            .into_iter()
            .map(|key| RowIndexMutation::Insert {
                key,
                fields: seed_value.clone(),
            })
            .collect();
        let seeded = graph
            .maintain_row_index(&mutations, &seed_scope)
            .expect("build a multi-page row map");
        let version = graph.row_snapshot.version();
        let seeded_store_root = graph
            .replace_relation_primary_root(
                version.store_root(),
                version,
                seeded.primary_root(),
                seeded.row_count(),
                &seed_scope,
            )
            .expect("publish seeded row root into relation map");
        graph.row_snapshot = load_format3_row_map(
            &graph.repository,
            graph.algorithm,
            seeded_store_root.clone(),
            graph.database_id,
            graph.snapshot_id,
            *version.relation_id(),
        )
        .expect("load seeded format-3 row map");
        graph.store_root = seeded_store_root;
    }

    /// A composite key pages in key order through the production row map:
    /// 300 tuple rows cross the 256-row page boundary and each is returned
    /// exactly once. A resume cursor on the first, a middle, the last or an
    /// absent key skips exactly the sorted suffix, and a cursor at or above
    /// the upper bound is refused.
    #[test]
    fn composite_keys_page_through_the_native_row_map_in_order() {
        let (_directory, mut graph) = capture_test_context();
        let keys: Vec<TypedKey> = (0..300)
            .map(|index| {
                TypedKey::Tuple(vec![
                    TypedKey::Text("north".to_owned()),
                    TypedKey::Text(format!("sku-{index:03}")),
                ])
            })
            .collect();
        publish_row_map(&mut graph, keys.clone());
        let scope = graph.open_read_scope().expect("composite query scope");
        let read = |range: &crate::row_store::KeyRange| -> Vec<TypedKey> {
            graph
                .range_rows(range, &scope)
                .expect("read one row-map page")
                .into_iter()
                .map(|row| row.key().clone())
                .collect()
        };

        let mut paged = Vec::new();
        let mut cursor: Option<TypedKey> = None;
        loop {
            let base = crate::row_store::KeyRange::new(None, None, 256).expect("valid page");
            let range = match cursor.take() {
                Some(after) => base.with_resume_after(after).expect("cursor inside the map"),
                None => base,
            };
            let page = read(&range);
            let short = page.len() < 256;
            cursor = page.last().cloned();
            paged.extend(page);
            if short {
                break;
            }
        }
        assert_eq!(paged, keys, "every composite key once, in key order");

        let resumed = |after: &TypedKey| {
            let range = crate::row_store::KeyRange::new(None, None, 256)
                .expect("valid page")
                .with_resume_after(after.clone())
                .expect("cursor inside the map");
            read(&range)
        };
        assert_eq!(resumed(&keys[0]), keys[1..257].to_vec(), "first key");
        assert_eq!(resumed(&keys[150]), keys[151..300].to_vec(), "middle key");
        let absent = TypedKey::Tuple(vec![
            TypedKey::Text("north".to_owned()),
            TypedKey::Text("sku-150x".to_owned()),
        ]);
        assert_eq!(resumed(&absent), keys[151..300].to_vec(), "absent key");
        assert!(resumed(&keys[299]).is_empty(), "last key");

        let bounded = crate::row_store::KeyRange::new(None, Some(keys[200].clone()), 256)
            .expect("valid bounded page");
        assert!(
            bounded.clone().with_resume_after(keys[200].clone()).is_err(),
            "a cursor at the upper bound is refused"
        );
        let inside = bounded
            .with_resume_after(keys[150].clone())
            .expect("cursor below the upper bound");
        assert_eq!(read(&inside), keys[151..200].to_vec(), "bounded middle");
    }

    /// A resume cursor is a full composite key, so it orders by its first
    /// component before its second. A cursor whose first component sorts
    /// below every row skips none of them, and one whose first component
    /// sorts above every row skips all of them.
    #[test]
    fn composite_resume_cursor_orders_by_first_component_first() {
        let (_directory, mut graph) = capture_test_context();
        let keys: Vec<TypedKey> = (0..3)
            .map(|index| {
                TypedKey::Tuple(vec![
                    TypedKey::Text("north".to_owned()),
                    TypedKey::Text(format!("sku-{index:03}")),
                ])
            })
            .collect();
        publish_row_map(&mut graph, keys.clone());
        let scope = graph.open_read_scope().expect("composite query scope");
        let resumed = |after: TypedKey| -> Vec<TypedKey> {
            let range = crate::row_store::KeyRange::new(None, None, 256)
                .expect("valid page")
                .with_resume_after(after)
                .expect("cursor inside the map");
            graph
                .range_rows(&range, &scope)
                .expect("read one row-map page")
                .into_iter()
                .map(|row| row.key().clone())
                .collect()
        };
        let first_below = TypedKey::Tuple(vec![
            TypedKey::Text("alpha".to_owned()),
            TypedKey::Text("zzz".to_owned()),
        ]);
        assert_eq!(resumed(first_below), keys, "a first component below every row skips none");
        let first_above = TypedKey::Tuple(vec![
            TypedKey::Text("south".to_owned()),
            TypedKey::Text("aaa".to_owned()),
        ]);
        assert!(resumed(first_above).is_empty(), "a first component above every row skips all");
    }

    #[test]
    fn bounded_range_reads_the_first_child_above_its_upper_bound_only() {
        let (_directory, mut graph) = capture_test_context();
        publish_row_map(
            &mut graph,
            (0..700).map(|index| TypedKey::Text(format!("row-{index:04}"))),
        );

        let bounded_scope = graph.open_read_scope().expect("bounded query scope");
        let bounded_range = crate::row_store::KeyRange::new(
            Some(TypedKey::Text("row-0150".to_owned())),
            Some(TypedKey::Text("row-0154".to_owned())),
            10,
        )
        .expect("valid bounded key interval");
        let bounded_rows = graph
            .range_rows(&bounded_range, &bounded_scope)
            .expect("read only the matching range and its upper boundary child");
        let bounded_keys: Vec<_> = bounded_rows
            .into_iter()
            .map(|row| row.key().clone())
            .collect();
        assert_eq!(
            bounded_keys,
            (150..154)
                .map(|index| TypedKey::Text(format!("row-{index:04}")))
                .collect::<Vec<_>>()
        );
        let bounded_objects = bounded_scope.objects_read();
        let key_scope = graph.open_read_scope().expect("key-only query scope");
        let key_only = graph
            .range_row_keys(&bounded_range, &key_scope)
            .expect("read matching keys without decoding their values");
        assert_eq!(key_only, bounded_keys);
        assert_eq!(key_scope.payload_bytes_read(), 0);
        assert_eq!(key_scope.objects_read(), bounded_objects);

        let full_scope = graph.open_read_scope().expect("full query scope");
        let mut full_rows = Vec::with_capacity(700);
        for (lower, upper) in [
            (None, Some(TypedKey::Text("row-0256".to_owned()))),
            (
                Some(TypedKey::Text("row-0256".to_owned())),
                Some(TypedKey::Text("row-0512".to_owned())),
            ),
            (Some(TypedKey::Text("row-0512".to_owned())), None),
        ] {
            let page = crate::row_store::KeyRange::new(lower, upper, 256)
                .expect("valid bounded key page");
            full_rows.extend(
                graph
                    .range_rows(&page, &full_scope)
                    .expect("read one valid row-map page"),
            );
        }
        assert_eq!(full_rows.len(), 700);
        assert!(
            bounded_objects < full_scope.objects_read(),
            "bounded traversal read {bounded_objects} Git objects, full traversal read {}",
            full_scope.objects_read()
        );
        assert_eq!(bounded_scope.payload_bytes_read(), 0);
    }

    #[test]
    fn foreign_root_row_is_rejected_before_graph_reads() {
        let (_directory, graph) = capture_test_context();
        let scope = graph.open_read_scope().expect("owner read scope");
        let admitted_version = graph.row_snapshot.version();
        let foreign_root =
            NativeObjectId::new(graph.algorithm, &[0x55; 20]).expect("foreign root OID");
        let foreign_version = crate::row_store::RowMapVersion::issue(
            *admitted_version.database_id(),
            *admitted_version.relation_id(),
            foreign_root.clone(),
            admitted_version.schema().clone(),
            admitted_version.primary_root().clone(),
            admitted_version.generation(),
            Some(0),
        )
        .expect("consistent foreign-root version");
        let row = crate::row_store::AdmittedRow::issue(
            &foreign_version,
            TypedKey::Text("foreign".to_owned()),
            RowValue::decode_canonical_fields(vec![0x80]).expect("canonical row value"),
        );

        let reads_before = scope.objects_read_for_test();
        assert!(matches!(
            graph.resolve_row_node(&row, &foreign_root, &scope),
            Err(GraphError::ContextMismatch)
        ));
        assert_eq!(scope.objects_read_for_test(), reads_before);
    }

    #[test]
    fn schema_generation_mismatch_is_rejected_before_native_lookup() {
        let (_directory, graph) = capture_test_context();
        let scope = graph.open_read_scope().expect("owner read scope");
        let admitted_version = graph.row_snapshot.version();
        let stale_schema = crate::row_store::SchemaGeneration::issue(
            *admitted_version.database_id(),
            *admitted_version.relation_id(),
            admitted_version.schema().schema_oid().clone(),
            *admitted_version.schema().schema_digest(),
            admitted_version
                .schema()
                .generation()
                .checked_add(1)
                .expect("fixture schema generation has room"),
        );
        let stale_version = crate::row_store::RowMapVersion::issue(
            *admitted_version.database_id(),
            *admitted_version.relation_id(),
            admitted_version.store_root().clone(),
            stale_schema,
            admitted_version.primary_root().clone(),
            admitted_version.generation(),
            Some(0),
        )
        .expect("consistent stale schema version");
        let row = crate::row_store::AdmittedRow::issue(
            &stale_version,
            TypedKey::Text("foreign".to_owned()),
            RowValue::decode_canonical_fields(vec![0x80]).expect("canonical row value"),
        );

        let reads_before = scope.objects_read_for_test();
        assert!(matches!(
            graph.resolve_row_node(&row, admitted_version.primary_root(), &scope),
            Err(GraphError::ContextMismatch)
        ));
        assert_eq!(scope.objects_read_for_test(), reads_before);
    }

    #[test]
    fn schema_generation_mismatch_is_rejected_before_row_history_lookup() {
        let (_directory, graph) = capture_test_context();
        let scope = graph.open_read_scope().expect("owner read scope");
        let admitted_version = graph.row_snapshot.version();
        let stale_schema = crate::row_store::SchemaGeneration::issue(
            *admitted_version.database_id(),
            *admitted_version.relation_id(),
            admitted_version.schema().schema_oid().clone(),
            *admitted_version.schema().schema_digest(),
            admitted_version
                .schema()
                .generation()
                .checked_add(1)
                .expect("fixture schema generation has room"),
        );
        let stale_version = crate::row_store::RowMapVersion::issue(
            *admitted_version.database_id(),
            *admitted_version.relation_id(),
            admitted_version.store_root().clone(),
            stale_schema,
            admitted_version.primary_root().clone(),
            admitted_version.generation(),
            Some(0),
        )
        .expect("consistent stale schema version");
        let row = crate::row_store::AdmittedRow::issue(
            &stale_version,
            TypedKey::Text("foreign".to_owned()),
            RowValue::decode_canonical_fields(vec![0x80]).expect("canonical row value"),
        );

        let reads_before = scope.objects_read_for_test();
        assert!(matches!(
            graph.list_row_revisions(&row, "HEAD", 1, &scope),
            Err(GraphError::ContextMismatch)
        ));
        assert_eq!(scope.objects_read_for_test(), reads_before);
    }

    #[test]
    fn prior_root_captured_blob_is_rejected_before_later_graph_reads() {
        let (directory, graph) = capture_test_context_with_blob_field();
        let capture_scope = graph.open_read_scope().expect("capture scope");
        let payload = b"captured OGB-2 payload on the earlier StoreRoot";
        let candidate = graph
            .capture_blob_candidate(&payload[..], payload.len() as u64, &capture_scope)
            .expect("capture native Blob");
        let pin = graph
            .protect_captured_blob(candidate, &capture_scope)
            .expect("protect captured Blob");
        let binding = graph
            .accept_protected_blob_pin(pin)
            .expect("admit captured descriptor");
        let descriptor_oid = binding.descriptor_oid().clone();
        let mut encoded_fields = vec![0x81];
        encoded_fields.extend_from_slice(binding.encoded_value());
        let blob_fields = encoded_fields;

        let source_version = graph.row_snapshot.version().clone();
        let source_snapshot_id = [0x39; 32];
        let row_key = TypedKey::Text("captured".to_owned());
        let source_store_root = graph
            .maintain_relation_rows(
                &source_version,
                &[RowIndexMutation::Insert {
                    key: row_key.clone(),
                    fields: blob_fields.clone(),
                }],
                &capture_scope,
            )
            .expect("write captured row under the source root");
        let source_rows = load_format3_row_map(
            &graph.repository,
            graph.algorithm,
            source_store_root.clone(),
            graph.database_id,
            source_snapshot_id,
            *source_version.relation_id(),
        )
        .expect("load source-root row map");
        let source_graph = NativeGraphContext::issue(
            graph.repository.clone(),
            graph.repository_id,
            [0x41; 32],
            graph.database_id,
            graph.owner_id,
            source_snapshot_id,
            graph.algorithm,
            source_store_root,
            graph.schema_digest,
            source_rows,
        )
        .expect("issue source-root graph");
        let source_scope = source_graph.open_read_scope().expect("source read scope");
        let old_row = source_graph
            .lookup_row(&row_key, &source_scope)
            .expect("read captured source row")
            .expect("captured source row exists");

        assert_eq!(old_row.version(), source_graph.row_snapshot.version());
        crate::test_support::configure_fixture_git_identity(directory.path());
        let revision_tree = encode_git_tree(vec![ParsedGitTreeEntry {
            mode: b"40000".to_vec(),
            name: b"descriptor".to_vec(),
            oid: descriptor_oid.clone(),
        }])
        .expect("encode the source-history tree");
        let revision_tree_oid = fixture_git_object(directory.path(), "tree", &revision_tree);
        let revision_tree_hex = revision_tree_oid.to_hex();
        let commit = Command::new("git")
            .current_dir(directory.path())
            .args([
                "-c",
                "commit.gpgsign=false",
                "commit-tree",
                &revision_tree_hex,
                "-m",
                "captured row history",
            ])
            .output()
            .expect("create source-history commit");
        assert!(
            commit.status.success(),
            "commit-tree failed: {}",
            String::from_utf8_lossy(&commit.stderr)
        );
        let commit_oid = String::from_utf8(commit.stdout)
            .expect("commit-tree prints UTF-8")
            .trim()
            .to_owned();
        let update_head = Command::new("git")
            .current_dir(directory.path())
            .args(["update-ref", "HEAD", &commit_oid])
            .status()
            .expect("set source-history commit as HEAD");
        assert!(update_head.success());
        let source_history = source_graph
            .list_row_revisions(&old_row, "HEAD", 1, &source_scope)
            .expect("traverse history for the source-admitted row");
        assert_eq!(source_history.len(), 1);
        assert_eq!(source_history[0].commit().to_hex(), commit_oid);
        assert_eq!(source_history[0].tree(), &revision_tree_oid);
        assert!(source_history[0].present());
        assert_eq!(source_scope.payload_bytes_read(), 0);

        let later_snapshot_id = [0x40; 32];
        let later_store_root = source_graph
            .maintain_relation_rows(
                source_graph.row_snapshot.version(),
                &[RowIndexMutation::Insert {
                    key: TypedKey::Text("later".to_owned()),
                    fields: blob_fields,
                }],
                &source_scope,
            )
            .expect("write a valid later StoreRoot");
        let later_rows = load_format3_row_map(
            &source_graph.repository,
            source_graph.algorithm,
            later_store_root.clone(),
            source_graph.database_id,
            later_snapshot_id,
            *source_graph.row_snapshot.version().relation_id(),
        )
        .expect("load later-root row map");
        let later_graph = NativeGraphContext::issue(
            source_graph.repository.clone(),
            source_graph.repository_id,
            [0x42; 32],
            source_graph.database_id,
            source_graph.owner_id,
            later_snapshot_id,
            source_graph.algorithm,
            later_store_root,
            source_graph.schema_digest,
            later_rows,
        )
        .expect("issue valid later-root graph");
        let later_scope = later_graph.open_read_scope().expect("later read scope");
        assert_ne!(old_row.version(), later_graph.row_snapshot.version());

        assert!(matches!(
            later_graph.resolve_row_node(&old_row, &descriptor_oid, &later_scope),
            Err(GraphError::ContextMismatch)
        ));
        assert!(matches!(
            later_graph.list_row_revisions(&old_row, "HEAD", 1, &later_scope),
            Err(GraphError::ContextMismatch)
        ));
        assert_eq!(later_scope.objects_read(), 0);
        assert_eq!(later_scope.payload_bytes_read(), 0);
    }
    #[test]
    fn requested_range_verifies_only_intersecting_real_media_chunks() {
        let (_directory, graph) = capture_test_context();
        let scope = graph.open_read_scope().expect("capture scope");
        let wave = include_bytes!("../tests/fixtures/media/tone.wav");
        assert_eq!(&wave[..4], b"RIFF");
        assert_eq!(&wave[8..12], b"WAVE");
        let mut payload = wave.to_vec();
        let pcm = payload[44..].to_vec();
        let target_length = 2 * crate::blob_store::GEAR_MAXIMUM + 73;
        while payload.len() < target_length {
            let remaining = target_length - payload.len();
            payload.extend_from_slice(&pcm[..remaining.min(pcm.len())]);
        }
        payload.truncate(target_length);
        let riff_length = (payload.len() - 8) as u32;
        let pcm_length = (payload.len() - 44) as u32;
        payload[4..8].copy_from_slice(&riff_length.to_le_bytes());
        payload[40..44].copy_from_slice(&pcm_length.to_le_bytes());
        let spans = crate::blob_store::GearChunker::new()
            .spans(&payload)
            .expect("chunk valid expanded PCM waveform");
        assert!(spans.len() > 1, "waveform fixture must span multiple chunks");
        let candidate = graph
            .capture_blob_candidate(&payload[..], payload.len() as u64, &scope)
            .expect("capture actual WAV media payload");
        let pin = graph
            .protect_captured_blob(candidate, &scope)
            .expect("verify and protect media closure");
        let binding = graph
            .accept_protected_blob_pin(pin)
            .expect("admit captured media descriptor");
        let mut objects = graph.stats_budget(&scope);
        let descriptor = graph
            .read_blob_descriptor(binding.descriptor_oid(), &scope, &mut objects)
            .expect("read pinned media descriptor");
        let reference = graph.admit_blob(descriptor).expect("bind media descriptor");
        let selected = spans[0].length()..spans[0].length() + 32;
        let stale_reference = AdmittedBlobReference {
            snapshot_id: [0xff; 32],
            ..reference.clone()
        };
        let stale_scope = graph.open_read_scope().expect("stale range scope");
        assert!(matches!(
            graph.read_blob_range(&stale_reference, selected.clone(), &stale_scope),
            Err(GraphError::ContextMismatch)
        ));
        assert_eq!(stale_scope.objects_read(), 0);
        assert_eq!(stale_scope.payload_bytes_read(), 0);
        let range_scope = graph.open_read_scope().expect("range scope");
        let before_objects = range_scope.objects_read();
        let result = graph
            .read_blob_range(&reference, selected.clone(), &range_scope)
            .expect("read media bytes from selected chunk");
        assert_eq!(
            result.bytes(),
            &payload[selected.start as usize..selected.end as usize]
        );
        assert_eq!(range_scope.payload_bytes_read(), spans[1].length());
        assert!(range_scope.objects_read() > before_objects);
        assert!(range_scope.payload_bytes_read() < payload.len() as u64);

        let empty_scope = graph.open_read_scope().expect("empty range scope");
        let before_empty = empty_scope.payload_bytes_read();
        let empty = graph
            .read_blob_range(&reference, 7..7, &empty_scope)
            .expect("empty requested range still validates its descriptor");
        assert!(empty.bytes().is_empty());
        assert!(empty_scope.objects_read() > 0);
        assert_eq!(empty_scope.payload_bytes_read() - before_empty, 0);
    }

    #[test]
    fn requested_range_rejects_a_fetched_chunk_with_wrong_raw_digest() {
        let (directory, graph) = capture_test_context();
        let chunk = b"content-addressed object with corrupt OGB-2 leaf digest";
        let target = fixture_git_object(directory.path(), "blob", chunk);
        let byte_root = fixture_git_node(
            directory.path(),
            &NodeData::ByteIndex {
                height: 0,
                total_length: chunk.len() as u64,
                entries: vec![ByteIndexEntry {
                    span_length: chunk.len() as u64,
                    target,
                    chunk_sha256: Some([0; 32]),
                }],
            },
        );
        let identity = crate::blob_store::digest_bytes(chunk);
        let descriptor_oid = fixture_git_node(
            directory.path(),
            &NodeData::BlobDescriptor {
                length: identity.length(),
                raw_sha256: identity.sha256(),
                byte_root: Some(byte_root),
            },
        );
        let reference = AdmittedBlobReference {
            context: graph.identity,
            database_id: graph.database_id,
            owner_id: graph.owner_id,
            snapshot_id: graph.snapshot_id,
            descriptor_oid,
            identity,
        };
        let scope = graph.open_read_scope().expect("range scope");
        assert!(matches!(
            graph.read_blob_range(&reference, 0..identity.length(), &scope),
            Err(GraphError::ChunkDigestMismatch)
        ));
    }

    #[test]
    fn requested_range_rejects_short_nonfinal_chunk_without_fetching_it() {
        let (directory, graph) = capture_test_context();
        let first = b"tiny";
        let last = b"tail";
        let first_oid = fixture_git_object(directory.path(), "blob", first);
        let last_oid = fixture_git_object(directory.path(), "blob", last);
        let byte_root = fixture_git_node(
            directory.path(),
            &NodeData::ByteIndex {
                height: 0,
                total_length: (first.len() + last.len()) as u64,
                entries: vec![
                    ByteIndexEntry {
                        span_length: first.len() as u64,
                        target: first_oid,
                        chunk_sha256: Some(crate::blob_store::digest_bytes(first).sha256()),
                    },
                    ByteIndexEntry {
                        span_length: last.len() as u64,
                        target: last_oid,
                        chunk_sha256: Some(crate::blob_store::digest_bytes(last).sha256()),
                    },
                ],
            },
        );
        let payload = [first.as_slice(), last.as_slice()].concat();
        let identity = crate::blob_store::digest_bytes(&payload);
        let descriptor_oid = fixture_git_node(
            directory.path(),
            &NodeData::BlobDescriptor {
                length: identity.length(),
                raw_sha256: identity.sha256(),
                byte_root: Some(byte_root),
            },
        );
        let reference = AdmittedBlobReference {
            context: graph.identity,
            database_id: graph.database_id,
            owner_id: graph.owner_id,
            snapshot_id: graph.snapshot_id,
            descriptor_oid,
            identity,
        };
        let scope = graph.open_read_scope().expect("range scope");
        assert!(matches!(
            graph.read_blob_range(&reference, 0..1, &scope),
            Err(GraphError::InvalidByteIndex)
        ));
        assert_eq!(scope.payload_bytes_read(), 0);
    }

    #[test]
    fn row_index_rewrite_reuses_blob_descriptor_without_reading_payload_bytes() {
        let (_directory, mut graph) = capture_test_context();
        let seed_scope = graph.open_read_scope().expect("owner seed scope");
        let payload = b"ORP page rewrite keeps this media payload pinned";
        let captured = graph
            .capture_blob_candidate(&payload[..], payload.len() as u64, &seed_scope)
            .expect("capture payload candidate");
        let pin = graph
            .protect_captured_blob(captured, &seed_scope)
            .expect("protect descriptor and payload closure");
        let binding = graph
            .accept_protected_blob_pin(pin)
            .expect("admit protected Blob for a row");
        let mut blob_row = vec![0x81];
        blob_row.extend_from_slice(binding.encoded_value());
        let empty_row = vec![0x80];
        let mutations: Vec<_> = (0..300)
            .map(|index| RowIndexMutation::Insert {
                key: TypedKey::Text(format!("row-{index:04}")),
                fields: if index == 150 {
                    blob_row.clone()
                } else {
                    empty_row.clone()
                },
            })
            .collect();
        let seeded = graph
            .maintain_row_index(&mutations, &seed_scope)
            .expect("build row pages containing the protected Blob reference");
        let version = graph.row_snapshot.version();
        let seeded_store_root = graph
            .replace_relation_primary_root(
                version.store_root(),
                version,
                seeded.primary_root(),
                seeded.row_count(),
                &seed_scope,
            )
            .expect("publish seeded row root into relation map");
        graph.row_snapshot = load_format3_row_map(
            &graph.repository,
            graph.algorithm,
            seeded_store_root.clone(),
            graph.database_id,
            graph.snapshot_id,
            *version.relation_id(),
        )
        .expect("load seeded format-3 row map");
        graph.store_root = seeded_store_root;
        drop(seed_scope);

        let query_scope = graph.open_read_scope().expect("row query scope");
        let blob_range = crate::row_store::KeyRange::new(
            Some(TypedKey::Text("row-0150".to_owned())),
            Some(TypedKey::Text("row-0151".to_owned())),
            1,
        )
        .expect("valid Blob row interval");
        let blob_rows = graph
            .range_rows(&blob_range, &query_scope)
            .expect("read Blob reference from its row page");
        assert_eq!(blob_rows.len(), 1);
        assert_eq!(blob_rows[0].key(), &TypedKey::Text("row-0150".to_owned()));
        assert_eq!(query_scope.payload_bytes_read(), 0);

        // The new row sorts next to the Blob row, so the rewritten page names
        // the descriptor again without the pass writing or reading it.
        let rewrite_scope = graph.open_read_scope().expect("rewrite scope");
        let rewrite = graph
            .maintain_row_index(
                &[RowIndexMutation::Insert {
                    key: TypedKey::Text("row-0150-a".to_owned()),
                    fields: empty_row,
                }],
                &rewrite_scope,
            )
            .expect("rewrite the index while preserving the Blob row");
        assert_eq!(
            rewrite.payload_objects_reused(),
            std::slice::from_ref(binding.descriptor_oid())
        );
        assert_eq!(rewrite.row_count(), 301);
        assert_eq!(rewrite_scope.payload_bytes_read(), 0);
    }

    /// Publishes a maintenance result into the test graph so later lookups
    /// read the new root.
    fn adopt_maintenance(graph: &mut NativeGraphContext, maintenance: &IndexMaintenance) {
        let scope = graph.open_read_scope().expect("adoption scope");
        let version = graph.row_snapshot.version().clone();
        let store_root = graph
            .replace_relation_primary_root(
                version.store_root(),
                &version,
                maintenance.primary_root(),
                maintenance.row_count(),
                &scope,
            )
            .expect("name the maintained root from the relation map");
        graph.row_snapshot = load_format3_row_map(
            &graph.repository,
            graph.algorithm,
            store_root.clone(),
            graph.database_id,
            graph.snapshot_id,
            *version.relation_id(),
        )
        .expect("load the maintained row map");
        graph.store_root = store_root;
    }

    /// store-format-3 section 5 and ORNA-ROW-005 (A07-storage-rows-05): the
    /// writer lowers values bottom-up. A field over 8 KiB, alone or inside a
    /// List, is lowered on its own and leaves its row inline; a row still over
    /// 8 KiB after that, or holding more than 32 direct Blob references, is
    /// stored through tag 60113 as a whole. Every lookup re-verifies the
    /// writer's lengths, SOV-3 digests and dependency indexes, and the stored
    /// fields expand back to exactly the tuples that were inserted.
    #[test]
    fn maintenance_lowers_large_values_and_rows_to_the_overflow_graph() {
        let (_directory, mut graph) = capture_test_context();
        let scope = graph.open_read_scope().expect("owner scope");
        let payload = b"one descriptor shared by many Blob fields";
        let captured = graph
            .capture_blob_candidate(&payload[..], payload.len() as u64, &scope)
            .expect("capture payload candidate");
        let pin = graph
            .protect_captured_blob(captured, &scope)
            .expect("protect descriptor and payload closure");
        let binding = graph
            .accept_protected_blob_pin(pin)
            .expect("admit protected Blob for rows");
        let blob = decode_canonical_cbor(binding.encoded_value()).expect("stored Blob reference");
        let text = |length: usize| CborValue::Text("x".repeat(length));
        let tuple = |fields: Vec<CborValue>| canonical_value_bytes(&CborValue::Array(fields));
        let rows = [
            ("blobs", tuple(vec![blob.clone(); 40])),
            ("list", tuple(vec![CborValue::Array(vec![text(10_000), text(3)])])),
            ("long", tuple(vec![text(20_000), CborValue::Unsigned(7)])),
            ("small", tuple(vec![text(10)])),
            ("wide", tuple(vec![text(5_000), text(5_000), text(5_000)])),
        ];
        let maintenance = graph
            .maintain_row_index(
                &rows
                    .iter()
                    .map(|(key, fields)| RowIndexMutation::Insert {
                        key: TypedKey::Text((*key).to_owned()),
                        fields: fields.clone(),
                    })
                    .collect::<Vec<_>>(),
                &scope,
            )
            .expect("lower and store large rows");
        adopt_maintenance(&mut graph, &maintenance);

        let read_scope = graph.open_read_scope().expect("read scope");
        for (key, fields) in &rows {
            let row = graph
                .lookup_row(&TypedKey::Text((*key).to_owned()), &read_scope)
                .expect("the reader verifies the stored row")
                .expect("the row is stored");
            let whole_row_lowered = matches!(row.value(), RowValue::Overflow(_));
            assert_eq!(
                whole_row_lowered,
                matches!(*key, "blobs" | "wide"),
                "{key}: whole-row overflow"
            );
            if let RowValue::Inline { encoded, .. } = row.value() {
                assert!(encoded.len() <= crate::row_store::ROW_INLINE_LIMIT, "{key}");
                let mut nested = BTreeSet::new();
                collect_overflow_oids(
                    &decode_canonical_cbor(encoded).expect("inline tuple"),
                    graph.algorithm,
                    &mut nested,
                )
                .expect("overflow references");
                assert_eq!(
                    nested.len(),
                    usize::from(matches!(*key, "list" | "long")),
                    "{key}: lowered fields"
                );
            }
            assert_eq!(
                &graph
                    .stored_row_fields(&row, &read_scope)
                    .expect("expand the stored fields"),
                fields,
                "{key}: stored fields expand to the inserted tuple"
            );
        }
        let blob_row = graph
            .lookup_row(&TypedKey::Text("blobs".to_owned()), &read_scope)
            .expect("lookup")
            .expect("row");
        assert_eq!(
            graph
                .row_blob_fields(&blob_row, &read_scope)
                .expect("project the overflowed row's Blob fields")
                .len(),
            40
        );
        assert_eq!(read_scope.payload_bytes_read(), 0, "no Blob payload is read");

        // A tuple that already names an overflow node is not a logical value.
        let RowValue::Overflow(stored) = blob_row.value() else {
            panic!("the Blob row is an overflow value");
        };
        let forged = tuple(vec![crate::overflow::overflow_reference(
            stored.native_root().expect("resolved overflow root"),
        )]);
        assert_eq!(
            graph.maintain_row_index(
                &[RowIndexMutation::Insert {
                    key: TypedKey::Text("forged".to_owned()),
                    fields: forged,
                }],
                &read_scope,
            ),
            Err(GraphError::NonCanonicalData)
        );
    }

    /// store-format-3 §3 ("Writers may rebuild incrementally but must match
    /// the canonical complete build"), ORNA-ROW-006 and ORNA-ROW-010
    /// (A07-storage-rows-04): one update reads the path to its row and the
    /// pages beside it, never the table; its root is the complete build of
    /// the same rows; and rewriting a row to the value it already holds is no
    /// change at all.
    #[test]
    fn maintenance_rewrites_only_the_pages_around_a_change() {
        let (_directory, mut graph) = capture_test_context();
        let key = |index: usize| TypedKey::Text(format!("row-{index:05}"));
        let fields = |index: usize, label: &str| {
            canonical_value_bytes(&CborValue::Array(vec![CborValue::Text(format!(
                "{label} {index}"
            ))]))
        };
        let scope = graph.open_read_scope().expect("seed scope");
        let seeded = graph
            .maintain_row_index(
                &(0..6_000)
                    .map(|index| RowIndexMutation::Insert {
                        key: key(index),
                        fields: fields(index, "seed"),
                    })
                    .collect::<Vec<_>>(),
                &scope,
            )
            .expect("seed rows");
        let nodes = seeded.index_objects_created().len();
        adopt_maintenance(&mut graph, &seeded);

        let scope = graph.open_read_scope().expect("same-value scope");
        let same = graph
            .maintain_row_index(
                &[
                    RowIndexMutation::Delete { key: key(1_234) },
                    RowIndexMutation::Insert {
                        key: key(1_234),
                        fields: fields(1_234, "seed"),
                    },
                ],
                &scope,
            )
            .expect("rewrite a row to its stored value");
        assert_eq!(same.primary_root(), same.previous_root());
        assert!(same.objects_written().is_empty(), "{same:?}");
        assert_eq!(same.row_count(), 6_000);

        let scope = graph.open_read_scope().expect("lookup scope");
        graph
            .lookup_row(&key(4_321), &scope)
            .expect("point lookup")
            .expect("row");
        let path_reads = scope.objects_read();
        let scope = graph.open_read_scope().expect("update scope");
        let update = graph
            .maintain_row_index(
                &[
                    RowIndexMutation::Delete { key: key(4_321) },
                    RowIndexMutation::Insert {
                        key: key(4_321),
                        fields: fields(4_321, "updated"),
                    },
                ],
                &scope,
            )
            .expect("one update");
        let update_reads = scope.objects_read();
        assert!(
            update_reads <= 3 * path_reads && (update_reads as usize) < nodes,
            "one update read {update_reads} objects; a lookup reads {path_reads}, the map has {nodes} nodes"
        );
        assert!(
            update.index_objects_created().len() <= 6,
            "one update wrote {} index nodes",
            update.index_objects_created().len()
        );
        assert!(!update.index_objects_reused().is_empty());

        let entries: Vec<_> = (0..6_000)
            .map(|index| {
                let label = if index == 4_321 { "updated" } else { "seed" };
                crate::row_store::RowEntry::new(
                    key(index),
                    RowValue::decode_canonical_fields(fields(index, label)).expect("inline row"),
                )
            })
            .collect();
        let mut sink = BTreeSet::new();
        let complete = NativeGraphContext::build_canonical_ordered_map(
            &rows_domain_for_version(graph.row_snapshot.version()),
            &entries,
            &mut |node| graph.write_capture_node(node, &mut sink, u64::MAX),
        )
        .expect("complete build");
        assert_eq!(update.primary_root(), &complete);
    }

    /// store-format-3 §3 ("Writers may rebuild incrementally but must match
    /// the canonical complete build"): an append, the shape the concurrent
    /// writers in `tests/row_store_scale.rs` produce, reads only the path to
    /// the last page and its root is the complete build of the same rows.
    #[test]
    fn append_maintenance_matches_the_complete_build() {
        let (_directory, mut graph) = capture_test_context();
        let key = |index: usize| TypedKey::Text(format!("row-{index:05}"));
        let fields = |index: usize, label: &str| {
            canonical_value_bytes(&CborValue::Array(vec![CborValue::Text(format!(
                "{label} {index}"
            ))]))
        };
        let scope = graph.open_read_scope().expect("seed scope");
        let seeded = graph
            .maintain_row_index(
                &(0..6_000)
                    .map(|index| RowIndexMutation::Insert {
                        key: key(index),
                        fields: fields(index, "seed"),
                    })
                    .collect::<Vec<_>>(),
                &scope,
            )
            .expect("seed rows");
        let nodes = seeded.index_objects_created().len();
        adopt_maintenance(&mut graph, &seeded);

        let scope = graph.open_read_scope().expect("append scope");
        let appended = graph
            .maintain_row_index(
                &[RowIndexMutation::Insert {
                    key: key(6_000),
                    fields: fields(6_000, "tail"),
                }],
                &scope,
            )
            .expect("append one row");
        let append_reads = scope.objects_read();
        assert_eq!(appended.previous_root(), seeded.primary_root());
        assert_eq!(appended.row_count(), 6_001);
        assert!(
            (append_reads as usize) < nodes,
            "an append read {append_reads} objects of {nodes} nodes"
        );

        let entries: Vec<_> = (0..=6_000)
            .map(|index| {
                let label = if index == 6_000 { "tail" } else { "seed" };
                crate::row_store::RowEntry::new(
                    key(index),
                    RowValue::decode_canonical_fields(fields(index, label)).expect("inline row"),
                )
            })
            .collect();
        let mut sink = BTreeSet::new();
        let complete = NativeGraphContext::build_canonical_ordered_map(
            &rows_domain_for_version(graph.row_snapshot.version()),
            &entries,
            &mut |node| graph.write_capture_node(node, &mut sink, u64::MAX),
        )
        .expect("complete build");
        assert_eq!(appended.primary_root(), &complete);
    }

    /// store-format-3 §3 ("Writers may rebuild incrementally but must match
    /// the canonical complete build"), ORNA-ROW-006 and ORNA-OGB-005 (W75
    /// F2/H1): one insert into a generated 100,000-row store reads and
    /// writes a bounded number of nodes per level of the map, never the
    /// table, and its root is the complete build of the new row set.
    #[test]
    fn one_insert_into_a_100k_row_store_touches_a_bounded_path() {
        const ROWS: usize = 100_000;
        // Nodes a pass may read or write per level: the node on the path and
        // one neighbour on each side where a page boundary moves.
        const NODES_PER_LEVEL: u64 = 3;
        // A node is a tree, its data blob and, when it names refs, a refs tree.
        const OBJECTS_PER_NODE: u64 = 3;
        let (_directory, mut graph) = capture_test_context();
        let key = |name: &str| TypedKey::Text(name.to_owned());
        let name = |index: usize| format!("row-{index:06}");
        let fields = |name: &str| {
            canonical_value_bytes(&CborValue::Array(vec![CborValue::Text(format!(
                "value of {name}"
            ))]))
        };
        let scope = graph.open_read_scope().expect("seed scope");
        let seeded = graph
            .maintain_row_index(
                &(0..ROWS)
                    .map(|index| RowIndexMutation::Insert {
                        key: key(&name(index)),
                        fields: fields(&name(index)),
                    })
                    .collect::<Vec<_>>(),
                &scope,
            )
            .expect("seed 100,000 rows");
        let nodes = seeded.index_objects_created().len() as u64;
        adopt_maintenance(&mut graph, &seeded);

        let scope = graph.open_read_scope().expect("height scope");
        let mut root_objects = ObjectBudget::new(
            scope.max_objects,
            scope.max_objects,
            Arc::clone(&scope.objects_used),
            scope.metadata_quota,
            Arc::clone(&scope.metadata_used),
        );
        let height = match graph
            .read_native_node(seeded.primary_root(), &scope, &mut root_objects)
            .expect("read the seeded root")
        {
            NodeData::OrderedBranch { height, .. } => u64::from(height),
            NodeData::OrderedLeaf { .. } => 0,
            other => panic!("the seeded root is an ordered node, not {other:?}"),
        };
        assert!(height >= 2, "100,000 rows need at least two branch levels");
        let levels = height + 1;

        // An insert between two existing rows, in the middle of a page.
        let inserted = format!("{}-inserted", name(54_321));
        let scope = graph.open_read_scope().expect("insert scope");
        let insert = graph
            .maintain_row_index(
                &[RowIndexMutation::Insert {
                    key: key(&inserted),
                    fields: fields(&inserted),
                }],
                &scope,
            )
            .expect("insert one row");
        let insert_reads = scope.objects_read();
        assert_eq!(insert.previous_root(), seeded.primary_root());
        assert_eq!(insert.row_count(), ROWS as u64 + 1);
        assert!(
            insert_reads <= NODES_PER_LEVEL * OBJECTS_PER_NODE * levels,
            "one insert read {insert_reads} objects; the map has {levels} levels and {nodes} nodes"
        );
        let created = insert.index_objects_created().len() as u64;
        assert!(
            (1..=NODES_PER_LEVEL * levels).contains(&created),
            "one insert wrote {created} index nodes; the map has {levels} levels"
        );
        assert!(!insert.index_objects_reused().is_empty());

        let mut names: Vec<String> = (0..ROWS).map(name).collect();
        names.push(inserted);
        names.sort();
        let entries: Vec<_> = names
            .iter()
            .map(|name| {
                crate::row_store::RowEntry::new(
                    key(name),
                    RowValue::decode_canonical_fields(fields(name)).expect("inline row"),
                )
            })
            .collect();
        let mut sink = BTreeSet::new();
        let complete = NativeGraphContext::build_canonical_ordered_map(
            &rows_domain_for_version(graph.row_snapshot.version()),
            &entries,
            &mut |node| graph.write_capture_node(node, &mut sink, u64::MAX),
        )
        .expect("complete build");
        assert_eq!(insert.primary_root(), &complete);
    }

    /// store-format-3 section 5 (A07-storage-rows-07): the row identity is
    /// `orna.row.v3` over the SOV-3 row `[database_uuid, table_uuid,
    /// typed_key, stored_fields]`. A Blob enters by length, SHA-256 and
    /// annotations, so rows naming different descriptor objects for the same
    /// content and annotations share an identity ("No physical object ID
    /// enters semantic type/row identity"), while an annotation, key or table
    /// change gives another.
    #[test]
    fn row_identity_binds_logical_content_not_physical_objects() {
        let database = [0x41; 16];
        let table = [0x42; 16];
        let key = TypedKey::Text("cover".to_owned());
        let blob = |media_type: &str, descriptor: u8| {
            CborValue::Tag(
                60111,
                Box::new(CborValue::Array(vec![
                    CborValue::Unsigned(68),
                    CborValue::Bytes(vec![0x5a; 32]),
                    CborValue::Text(media_type.to_owned()),
                    CborValue::Null,
                    CborValue::Bytes(vec![descriptor; 20]),
                ])),
            )
        };
        let row = |field: CborValue| {
            canonical_value_bytes(&CborValue::Array(vec![
                CborValue::Text("Ridge".to_owned()),
                field,
            ]))
        };
        let identity = |table: &[u8; 16], key: &TypedKey, fields: &[u8]| {
            row_identity_v3(&database, table, key, fields, 20).expect("row identity")
        };

        let stored = identity(&table, &key, &row(blob("image/png", 0x01)));
        assert_eq!(
            stored,
            identity(&table, &key, &row(blob("image/png", 0x02))),
            "a descriptor object ID does not enter the identity"
        );
        assert_ne!(
            stored,
            identity(&table, &key, &row(blob("image/jpeg", 0x01))),
            "annotations do"
        );
        assert_ne!(
            stored,
            identity(
                &table,
                &TypedKey::Text("other".to_owned()),
                &row(blob("image/png", 0x01))
            )
        );
        assert_ne!(
            stored,
            identity(&[0x43; 16], &key, &row(blob("image/png", 0x01)))
        );

        // ASCII domain, NUL and the canonical SOV-3 payload.
        let uuid = |id: [u8; 16]| CborValue::Tag(37, Box::new(CborValue::Bytes(id.to_vec())));
        let payload = canonical_value_bytes(&CborValue::Array(vec![
            uuid(database),
            uuid(table),
            CborValue::Text("cover".to_owned()),
            CborValue::Array(vec![
                CborValue::Text("Ridge".to_owned()),
                CborValue::Tag(
                    60112,
                    Box::new(CborValue::Array(vec![
                        CborValue::Unsigned(68),
                        CborValue::Bytes(vec![0x5a; 32]),
                        CborValue::Text("image/png".to_owned()),
                        CborValue::Null,
                    ])),
                ),
            ]),
        ]));
        let mut expected = Sha256::new();
        expected.update(b"orna.row.v3\0");
        expected.update(&payload);
        assert_eq!(stored, <[u8; 32]>::from(expected.finalize()));

        // Fields that still name an overflow node are not a logical row.
        let overflow = CborValue::Tag(
            60113,
            Box::new(CborValue::Array(vec![CborValue::Bytes(vec![0x09; 20])])),
        );
        assert_eq!(
            row_identity_v3(&database, &table, &key, &row(overflow), 20),
            Err(GraphError::NonCanonicalData)
        );
    }

    /// store-format-3 section 5 (A07-storage-rows-07): a kind-6 node records
    /// the `orna.schema.v3` identity of its closed descriptor, and every rows
    /// domain binds it. A store whose schema node records the plain SHA-256 of
    /// the descriptor, as builds before this one wrote, is refused by the
    /// row-map loader instead of being read under the wrong identity.
    #[test]
    fn schema_nodes_record_the_v3_schema_identity() {
        let (directory, graph) = capture_test_context();
        let root = directory.path();
        let version = graph.row_snapshot.version().clone();
        let scope = graph.open_read_scope().expect("schema scope");
        let bytes = graph
            .read_schema_descriptor(&scope)
            .expect("the issued graph verified its schema node")
            .encode()
            .expect("canonical descriptor bytes");
        let mut expected = Sha256::new();
        expected.update(b"orna.schema.v3\0");
        expected.update(&bytes);
        let identity: [u8; 32] = expected.finalize().into();
        assert_eq!(version.schema().schema_digest(), &identity);

        let store_with = |schema_digest: [u8; 32]| {
            let chunk = fixture_git_object(root, "blob", &bytes);
            let byte_root = fixture_git_node(
                root,
                &NodeData::ByteIndex {
                    height: 0,
                    total_length: bytes.len() as u64,
                    entries: vec![ByteIndexEntry {
                        span_length: bytes.len() as u64,
                        target: chunk,
                        chunk_sha256: Some(Sha256::digest(&bytes).into()),
                    }],
                },
            );
            let schema_oid = fixture_git_node(
                root,
                &NodeData::Schema {
                    encoded_length: bytes.len() as u64,
                    schema_digest,
                    byte_root,
                },
            );
            let primary_root = fixture_git_node(
                root,
                &NodeData::OrderedLeaf {
                    domain: rows_domain(*version.relation_id(), schema_digest),
                    entries: Vec::new(),
                },
            );
            let relation_map = fixture_git_node(
                root,
                &NodeData::OrderedLeaf {
                    domain: canonical_value_bytes(&CborValue::Array(vec![
                        CborValue::Text("relations".to_owned()),
                        CborValue::Bytes(version.database_id().to_vec()),
                    ])),
                    entries: vec![OrderedLeafEntry {
                        key: TypedKey::Bytes(version.relation_id().to_vec())
                            .canonical_bytes()
                            .expect("canonical relation key"),
                        value: canonical_value_bytes(&CborValue::Array(vec![
                            CborValue::Bytes(schema_oid.as_bytes().to_vec()),
                            CborValue::Bytes(primary_root.as_bytes().to_vec()),
                            CborValue::Null,
                            CborValue::Unsigned(0),
                        ])),
                    }],
                },
            );
            fixture_git_node(root, &NodeData::StoreRoot { relation_map })
        };
        let load = |store_root: NativeOid| {
            load_format3_row_map(
                &graph.repository,
                graph.algorithm,
                store_root,
                *version.database_id(),
                graph.snapshot_id,
                *version.relation_id(),
            )
        };
        assert!(load(store_with(identity)).is_ok());
        assert_eq!(
            load(store_with(Sha256::digest(&bytes).into())).err(),
            Some(GraphError::ContentIdentityMismatch)
        );
    }

    fn fixture_git_stdout(directory: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(directory)
            .env("GIT_NO_LAZY_FETCH", "1")
            .args(args)
            .output()
            .expect("run fixture git command");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("git prints UTF-8")
            .trim()
            .to_owned()
    }

    /// Removes one loose object file, so its payload stops being local while
    /// its id stays usable. The capture fixtures write with `git hash-object -w`
    /// and never repack, so every fixture object is loose.
    fn remove_loose_object(directory: &Path, oid: &NativeOid) -> bool {
        let hex = oid.to_hex();
        let path = directory
            .join(".git")
            .join("objects")
            .join(&hex[..2])
            .join(&hex[2..]);
        if !path.is_file() {
            return false;
        }
        std::fs::remove_file(path).expect("remove deferred loose object");
        true
    }

    fn fixture_object_is_absent(directory: &Path, oid: &NativeOid) -> bool {
        !Command::new("git")
            .current_dir(directory)
            .env("GIT_NO_LAZY_FETCH", "1")
            .args(["cat-file", "-e", &oid.to_hex()])
            .output()
            .expect("probe fixture object")
            .status
            .success()
    }

    /// True when local promisor evidence proves the id reachable, using exactly
    /// the walk `Repository::observe_git_object` performs. A configuration-only
    /// repository never satisfies this.
    fn fixture_promisor_evidence(directory: &Path, oid: &NativeOid) -> bool {
        let promised = format!("?{}", oid.to_hex());
        fixture_git_stdout(
            directory,
            &[
                "rev-list",
                "--objects",
                "--missing=print",
                "--no-object-names",
                "--all",
            ],
        )
        .lines()
        .any(|line| line.eq_ignore_ascii_case(&promised))
    }

    /// Defers one object through local promisor evidence: a commit keeps the id
    /// reachable, `origin` is a promisor remote, and the payload is removed.
    /// No network access, clone or fetch is involved.
    fn defer_object_through_a_promisor(directory: &Path, oid: &NativeOid) {
        let entry = format!("040000 tree {}\tstore\n", oid.to_hex());
        let mut child = Command::new("git")
            .current_dir(directory)
            .env("GIT_NO_LAZY_FETCH", "1")
            .args(["mktree"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn fixture mktree");
        child
            .stdin
            .take()
            .expect("mktree stdin")
            .write_all(entry.as_bytes())
            .expect("write fixture tree entry");
        let output = child.wait_with_output().expect("wait for fixture mktree");
        assert!(
            output.status.success(),
            "git mktree failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let tree = String::from_utf8(output.stdout).expect("mktree prints UTF-8");
        let commit = fixture_git_stdout(
            directory,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit-tree",
                tree.trim(),
                "-m",
                "deferred",
            ],
        );
        fixture_git_stdout(
            directory,
            &["update-ref", "refs/heads/main", commit.as_str()],
        );
        fixture_git_stdout(directory, &["remote", "add", "origin", "."]);
        fixture_git_stdout(directory, &["config", "remote.origin.promisor", "true"]);
        fixture_git_stdout(
            directory,
            &["config", "remote.origin.partialclonefilter", "blob:none"],
        );
        fixture_git_stdout(directory, &["config", "extensions.partialClone", "origin"]);
        assert!(
            remove_loose_object(directory, oid),
            "a deferred object must still be a loose object"
        );
    }

    #[test]
    fn partial_clone_object_availability_is_observed_without_hydration() {
        let (directory, graph) = capture_test_context();
        let store_root = graph.store_root().clone();
        let scope = graph.open_read_scope().expect("availability scope");
        assert_eq!(
            graph
                .observe_object_availability(&scope, &store_root)
                .expect("observe a materialized store root"),
            ObjectAvailability::Materialized
        );

        defer_object_through_a_promisor(directory.path(), &store_root);
        assert!(fixture_object_is_absent(directory.path(), &store_root));
        assert!(fixture_promisor_evidence(directory.path(), &store_root));

        let promised_scope = graph.open_read_scope().expect("promised scope");
        assert_eq!(
            graph
                .observe_object_availability(&promised_scope, &store_root)
                .expect("observe a deferred object"),
            ObjectAvailability::Promised
        );
        // Promisor configuration alone never promises an id nothing references.
        let unreferenced = NativeOid::from_hex(graph.algorithm, &"2a".repeat(20))
            .expect("valid unreferenced object id");
        assert_eq!(
            graph
                .observe_object_availability(&promised_scope, &unreferenced)
                .expect("observe an unreferenced object"),
            ObjectAvailability::Unavailable
        );
        assert!(fixture_object_is_absent(directory.path(), &store_root));
        assert_eq!(promised_scope.payload_bytes_read(), 0);
    }

/// ORNA-CAP-002: promisor evidence distinguishes inherited promised objects from unknown missing content.
    #[test]
    fn a_deferred_node_reads_as_promised_and_never_as_an_unknown_missing_object() {
        let (directory, graph) = capture_test_context();
        let store_root = graph.store_root().clone();
        defer_object_through_a_promisor(directory.path(), &store_root);

        let scope = graph.open_read_scope().expect("deferred read scope");
        let error = graph
            .object_stats(&scope)
            .expect_err("a deferred node cannot be read locally");
        assert!(
            matches!(
                &error,
                GraphError::PromisedObjectNotMaterialized { object_id }
                    if *object_id == store_root.to_hex()
            ),
            "{error:?}"
        );
        assert_eq!(error.exit_code(), 3);
        assert_eq!(scope.payload_bytes_read(), 0);
        // The read named the deferred object without hydrating it.
        assert!(fixture_object_is_absent(directory.path(), &store_root));

        // The same read on an object nothing references, and no promisor remote
        // configured, stays a plain missing object.
        let (plain_directory, plain_graph) = capture_test_context();
        let plain_root = plain_graph.store_root().clone();
        assert!(remove_loose_object(plain_directory.path(), &plain_root));
        let plain_scope = plain_graph.open_read_scope().expect("plain read scope");
        let plain_error = plain_graph
            .object_stats(&plain_scope)
            .expect_err("an absent node cannot be read locally");
        assert!(
            matches!(
                &plain_error,
                GraphError::MissingObject { object_id } if *object_id == plain_root.to_hex()
            ),
            "{plain_error:?}"
        );
        assert_eq!(plain_error.exit_code(), 3);
    }

    #[test]
    fn stored_table_reference_resolves_against_containing_row_snapshot_after_rekey() {
        let (directory, mut graph) = capture_test_context();
        let scope = graph.open_read_scope().expect("owner read scope");
        let original = graph.row_snapshot.version().clone();
        let old_key = TypedKey::Text("old-address".to_owned());
        let new_key = TypedKey::Text("new-address".to_owned());
        let owner_key = TypedKey::Text("owner".to_owned());
        let reference_value = CborValue::Tag(
            60021,
            Box::new(CborValue::Array(vec![
                CborValue::Tag(37, Box::new(CborValue::Bytes(original.database_id().to_vec()))),
                CborValue::Tag(37, Box::new(CborValue::Bytes(original.relation_id().to_vec()))),
                CborValue::Text("old-address".to_owned()),
            ])),
        );
        let owner_fields = canonical_value_bytes(&CborValue::Array(vec![
            reference_value.clone(),
        ]));
        row_fields_dependencies(&owner_fields)
            .expect("stored table reference has no native-object dependencies");
        let initial = [
            RowIndexMutation::Insert {
                key: old_key.clone(),
                fields: vec![0x80],
            },
            RowIndexMutation::Insert {
                key: owner_key.clone(),
                fields: owner_fields,
            },
        ];
        let seeded = graph
            .maintain_row_index(&initial, &scope)
            .expect("seed owner and target rows");
        let seeded_root = graph
            .maintain_relations_rows(&[(&original, &seeded)], &scope)
            .expect("publish seeded rows");
        let seeded_snapshot = load_format3_row_map(
            &graph.repository,
            graph.algorithm,
            seeded_root.clone(),
            *original.database_id(),
            graph.snapshot_id,
            *original.relation_id(),
        )
        .expect("load seeded snapshot");
        graph.store_root = seeded_root;
        graph.row_snapshot = seeded_snapshot;
        let owner_before_rekey = graph
            .lookup_row(&owner_key, &scope)
            .expect("read owner row")
            .expect("owner exists");
        let target_before_rekey = graph
            .lookup_row(&old_key, &scope)
            .expect("read referenced row")
            .expect("target exists");
        let encoded_reference = canonical_value_bytes(&reference_value);
        assert_eq!(
            graph
                .resolve_table_reference(&owner_before_rekey, &encoded_reference, &scope)
                .expect("resolve before rekey"),
            Some(target_before_rekey.clone())
        );

        let rekey = [RowIndexMutation::Rekey {
            from: old_key.clone(),
            to: new_key.clone(),
        }];
        let current_version = graph.row_snapshot.version().clone();
        let maintenance = graph
            .maintain_row_index(&rekey, &scope)
            .expect("prepare target rekey");
        let current_root = graph
            .maintain_relations_rows(&[(&current_version, &maintenance)], &scope)
            .expect("publish target rekey");
        let current_snapshot = load_format3_row_map(
            &graph.repository,
            graph.algorithm,
            current_root.clone(),
            *original.database_id(),
            graph.snapshot_id,
            *original.relation_id(),
        )
        .expect("load rekeyed snapshot");
        graph.store_root = current_root;
        graph.row_snapshot = current_snapshot;

        assert_eq!(
            graph
                .resolve_table_reference(&owner_before_rekey, &encoded_reference, &scope)
                .expect("resolve in the containing row's older snapshot"),
            Some(target_before_rekey)
        );
        let owner_after_rekey = graph
            .lookup_row(&owner_key, &scope)
            .expect("read current owner row")
            .expect("owner remains present");
        assert!(graph
            .resolve_table_reference(&owner_after_rekey, &encoded_reference, &scope)
            .expect("resolve the old address in current snapshot")
            .is_none());
        assert!(graph
            .lookup_row(&new_key, &scope)
            .expect("read moved target")
            .is_some());
        drop(maintenance);
        drop(seeded);
        drop(directory);
    }

    /// The registered-tag rule of store-format-3 §2 and `final_model.decode`:
    /// node data admits bignums 2 and 3, UUID 37, the value tags
    /// 60000..=60025 and the profile tags 60110..=60113, and every negative
    /// 64-bit head.
    #[test]
    fn canonical_reader_admits_exactly_the_format3_registered_tags() {
        let tagged = |tag: u64, payload: &[u8]| {
            let mut bytes = Vec::new();
            cbor_head(&mut bytes, 6, tag);
            bytes.extend_from_slice(payload);
            bytes
        };
        for tag in [
            37, 60_000, 60_001, 60_002, 60_005, 60_008, 60_009, 60_015, 60_025, 60_110, 60_113,
        ] {
            assert!(
                decode_canonical_cbor(&tagged(tag, &[0x80])).is_ok(),
                "tag {tag} is registered for format-3 nodes"
            );
        }
        for tag in [0, 1, 4, 36, 38, 59_999, 60_026, 60_027, 60_109, 60_114] {
            assert!(
                matches!(
                    decode_canonical_cbor(&tagged(tag, &[0x80])),
                    Err(GraphError::NonCanonicalData)
                ),
                "tag {tag} is not registered for format-3 nodes"
            );
        }

        // A bignum spells only an Int beyond the 64-bit heads.
        let mut two_to_the_64 = vec![0x49, 0x01];
        two_to_the_64.extend_from_slice(&[0; 8]);
        let mut leading_zero = vec![0x49, 0x00];
        leading_zero.extend_from_slice(&[0xff; 8]);
        let mut narrow = vec![0x48];
        narrow.extend_from_slice(&[0xff; 8]);
        for tag in [2, 3] {
            assert!(decode_canonical_cbor(&tagged(tag, &two_to_the_64)).is_ok());
            for payload in [&leading_zero[..], &narrow[..], &[0x61, b'x'][..]] {
                assert!(matches!(
                    decode_canonical_cbor(&tagged(tag, payload)),
                    Err(GraphError::NonCanonicalData)
                ));
            }
        }
        // -2^64 is the last Int a negative head spells.
        let mut most_negative = vec![0x3b];
        most_negative.extend_from_slice(&[0xff; 8]);
        assert_eq!(
            decode_canonical_cbor(&most_negative),
            Ok(CborValue::Negative(u64::MAX))
        );
    }

    /// Blob and overflow references nested in records, enum payloads, tuples
    /// and options are row dependencies (`final_model.blob_references`);
    /// other typed values add none, and OVB-2 or SOV-3 Blob tags are refused.
    #[test]
    fn row_dependencies_recurse_into_typed_value_payloads() {
        let algorithm = GitHashAlgorithm::Sha1;
        let descriptor = NativeObjectId::new(algorithm, &[0x44; 20]).unwrap();
        let overflow = NativeObjectId::new(algorithm, &[0x55; 20]).unwrap();
        let uuid = |byte: u8| CborValue::Tag(37, Box::new(CborValue::Bytes(vec![byte; 16])));
        let blob = CborValue::Tag(
            60111,
            Box::new(CborValue::Array(vec![
                CborValue::Unsigned(1),
                CborValue::Bytes(vec![0x66; 32]),
                CborValue::Text("text/plain".to_owned()),
                CborValue::Null,
                CborValue::Bytes(descriptor.as_bytes().to_vec()),
            ])),
        );
        let record = CborValue::Tag(
            60009,
            Box::new(CborValue::Array(vec![
                uuid(0x10),
                CborValue::Array(vec![CborValue::Array(vec![uuid(0x11), blob])]),
            ])),
        );
        let decimal = CborValue::Tag(
            60000,
            Box::new(CborValue::Array(vec![
                CborValue::Unsigned(15),
                CborValue::Negative(0),
            ])),
        );
        let overflow_value = CborValue::Tag(
            60113,
            Box::new(CborValue::Array(vec![CborValue::Bytes(
                overflow.as_bytes().to_vec(),
            )])),
        );
        let fields = CborValue::Array(vec![
            decimal.clone(),
            CborValue::Tag(
                60008,
                Box::new(CborValue::Array(vec![uuid(0x20), uuid(0x21), record])),
            ),
            CborValue::Tag(
                60015,
                Box::new(CborValue::Array(vec![
                    decimal,
                    CborValue::Tag(
                        60013,
                        Box::new(CborValue::Array(vec![
                            CborValue::Unsigned(1),
                            overflow_value,
                        ])),
                    ),
                ])),
            ),
        ]);
        let dependencies = row_fields_dependencies(&canonical_value_bytes(&fields))
            .expect("typed values are admitted in a stored row");
        assert_eq!(
            dependencies,
            vec![
                NativeDependency::new(descriptor, NativeObjectKind::Tree),
                NativeDependency::new(overflow, NativeObjectKind::Tree),
            ]
        );
        // The nested kind-3 value is charged to its own inline threshold.
        assert_eq!(direct_blob_reference_count(&fields), Ok(1));

        for refused in [60110, 60112] {
            let nested = CborValue::Array(vec![CborValue::Tag(
                60015,
                Box::new(CborValue::Array(vec![CborValue::Tag(
                    refused,
                    Box::new(CborValue::Array(Vec::new())),
                )])),
            )]);
            assert!(matches!(
                row_fields_dependencies(&canonical_value_bytes(&nested)),
                Err(GraphError::NonCanonicalData)
            ));
        }
    }

    fn uuid_raw(id: [u8; 16]) -> Raw {
        Raw::Tag(37, Box::new(Raw::Bytes(id.to_vec())))
    }

    fn primitive_type(name: &str) -> Raw {
        Raw::Array(vec![Raw::Int(0.into()), Raw::Text(name.to_owned())])
    }

    fn field_descriptor(id: [u8; 16], name: &str, ty: Raw, role: u64) -> Raw {
        Raw::Array(vec![
            uuid_raw(id),
            Raw::Text(name.to_owned()),
            ty,
            Raw::Int(role.into()),
            Raw::Array(vec![Raw::Int(0.into())]),
        ])
    }

    /// A closed table descriptor; `fields` are sorted by id and key fields
    /// have role 0.
    fn typed_schema(
        relation_id: [u8; 16],
        key_ids: &[[u8; 16]],
        fields: Vec<Raw>,
        definitions: Vec<Raw>,
    ) -> SchemaDescriptor {
        SchemaDescriptor::new(Raw::Map(vec![
            (Raw::Int(0.into()), Raw::Int(1.into())),
            (Raw::Int(1.into()), uuid_raw(relation_id)),
            (
                Raw::Int(2.into()),
                Raw::Array(key_ids.iter().copied().map(uuid_raw).collect()),
            ),
            (Raw::Int(3.into()), Raw::Array(fields)),
            (Raw::Int(4.into()), Raw::Array(definitions)),
        ]))
        .expect("valid typed table descriptor")
    }

    fn canonical_raw(raw: Raw) -> Vec<u8> {
        orna_value_v1::Value::new(raw)
            .expect("registered canonical value")
            .encode()
            .expect("canonical value bytes")
    }

    /// Stores rows of canonical key and stored-field bytes in a fresh store
    /// whose one relation uses `descriptor`, through the mutation and
    /// relation-swap path publication uses, then reopens the published root.
    fn store_typed_rows(
        relation_id: [u8; 16],
        descriptor: &SchemaDescriptor,
        rows: &[(Vec<u8>, Vec<u8>)],
    ) -> (tempfile::TempDir, NativeGraphContext) {
        let directory = tempfile::tempdir().expect("create typed row repository");
        init_fixture_repository(directory.path());
        let (schema_oid, schema_digest) = fixture_descriptor_node(directory.path(), descriptor);
        let mut graph =
            fixture_relation_context(directory.path(), relation_id, schema_oid, schema_digest);
        let mutations = rows
            .iter()
            .map(|(key, fields)| {
                RowIndexMutation::insert_canonical(key, fields).expect("typed row is admitted")
            })
            .collect::<Vec<_>>();
        let scope = graph.open_read_scope().expect("owner write scope");
        let maintained = graph
            .maintain_row_index(&mutations, &scope)
            .expect("maintain the typed row map");
        let version = graph.row_snapshot.version();
        let store_root = graph
            .replace_relation_primary_root(
                version.store_root(),
                version,
                maintained.primary_root(),
                maintained.row_count(),
                &scope,
            )
            .expect("publish typed rows into the relation map");
        graph.row_snapshot = load_format3_row_map(
            &graph.repository,
            graph.algorithm,
            store_root.clone(),
            graph.database_id,
            graph.snapshot_id,
            relation_id,
        )
        .expect("reload the published typed row map");
        graph.store_root = store_root;
        drop(scope);
        (directory, graph)
    }

    /// Reads every row back in key order, by range and by point lookup, and
    /// checks each against `expected` and the table descriptor.
    fn assert_typed_rows(
        graph: &NativeGraphContext,
        relation_id: [u8; 16],
        descriptor: &SchemaDescriptor,
        expected: &[(TypedKey, Vec<u8>)],
    ) {
        let scope = graph.open_read_scope().expect("typed read scope");
        let all = crate::row_store::KeyRange::new(None, None, 64).expect("full typed range");
        let rows = graph
            .range_rows(&all, &scope)
            .expect("range the typed rows");
        assert_eq!(rows.len(), expected.len());
        for (row, (key, fields)) in rows.iter().zip(expected) {
            assert_eq!(row.key(), key, "rows come back in logical key order");
            assert_eq!(row.value().encoded_fields(), Some(fields.as_slice()));
            let canonical_key = row.key().canonical_bytes().expect("canonical typed key");
            descriptor
                .validate_stored_row(relation_id, &canonical_key, fields)
                .expect("the stored row keeps its schema types");
            let found = graph
                .lookup_row(key, &scope)
                .expect("look up a typed key")
                .expect("the typed key is present");
            assert_eq!(found.value().encoded_fields(), Some(fields.as_slice()));
        }
        let last = expected.len() - 1;
        let inner = crate::row_store::KeyRange::new(
            Some(expected[1].0.clone()),
            Some(expected[last].0.clone()),
            64,
        )
        .expect("bounded typed range");
        assert_eq!(
            graph
                .range_row_keys(&inner, &scope)
                .expect("range the inner typed keys"),
            expected[1..last]
                .iter()
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>()
        );
    }

    /// A07-storage-rows-02, ORNA-ROW-001: rows holding Decimal, Date, Instant,
    /// Duration, Uuid, enum, record, tuple, Option and wide Int values are
    /// stored, published and read back unchanged from a Uuid-keyed table.
    #[test]
    fn typed_row_values_round_trip_through_a_published_row_map() {
        const MOOD: [u8; 16] = [0xe0; 16];
        const CALM: [u8; 16] = [0xe1; 16];
        const WILD: [u8; 16] = [0xe2; 16];
        const POINT: [u8; 16] = [0xf0; 16];
        let relation_id = [0x52; 16];
        let stored = |byte: u8, name: &str, ty: Raw| field_descriptor([byte; 16], name, ty, 1);
        let nominal = |id: [u8; 16]| Raw::Array(vec![Raw::Int(5.into()), uuid_raw(id)]);
        let descriptor = typed_schema(
            relation_id,
            &[[0x01; 16]],
            vec![
                field_descriptor([0x01; 16], "id", primitive_type("Uuid"), 0),
                stored(0x02, "amount", primitive_type("Decimal")),
                stored(0x03, "day", primitive_type("Date")),
                stored(0x04, "at", primitive_type("Instant")),
                stored(0x05, "span", primitive_type("Duration")),
                stored(0x06, "owner", primitive_type("Uuid")),
                stored(0x07, "mood", nominal(MOOD)),
                stored(0x08, "point", nominal(POINT)),
                stored(
                    0x09,
                    "pair",
                    Raw::Array(vec![
                        Raw::Int(3.into()),
                        Raw::Array(vec![primitive_type("Int"), primitive_type("Str")]),
                    ]),
                ),
                stored(
                    0x0a,
                    "discount",
                    Raw::Array(vec![Raw::Int(2.into()), primitive_type("Decimal")]),
                ),
                stored(0x0b, "count", primitive_type("Int")),
            ],
            vec![
                Raw::Array(vec![
                    uuid_raw(MOOD),
                    Raw::Int(1.into()),
                    Raw::Array(vec![
                        Raw::Array(vec![
                            uuid_raw(CALM),
                            Raw::Text("calm".to_owned()),
                            Raw::Array(Vec::new()),
                        ]),
                        Raw::Array(vec![
                            uuid_raw(WILD),
                            Raw::Text("wild".to_owned()),
                            Raw::Array(Vec::new()),
                        ]),
                    ]),
                ]),
                Raw::Array(vec![
                    uuid_raw(POINT),
                    Raw::Int(0.into()),
                    Raw::Array(vec![
                        field_descriptor([0xf1; 16], "x", primitive_type("Int"), 1),
                        field_descriptor([0xf2; 16], "label", primitive_type("Str"), 1),
                    ]),
                ]),
            ],
        );
        let decimal = |coefficient: i64, exponent: i64| {
            Raw::Tag(
                60_000,
                Box::new(Raw::Array(vec![
                    Raw::Int(coefficient.into()),
                    Raw::Int(exponent.into()),
                ])),
            )
        };
        let time = |tag: u64, seconds: i64, nanos: u32| {
            Raw::Tag(
                tag,
                Box::new(Raw::Array(vec![
                    Raw::Int(seconds.into()),
                    Raw::Int(nanos.into()),
                ])),
            )
        };
        let fields = |index: u8, mood: [u8; 16]| {
            let ordinal = i64::from(index);
            let discount = if index == 0 {
                Raw::Array(vec![Raw::Int(0.into())])
            } else {
                Raw::Array(vec![Raw::Int(1.into()), decimal(5, -2)])
            };
            canonical_raw(Raw::Array(vec![
                decimal(1_999 + 2 * ordinal, -2),
                Raw::Tag(
                    60_001,
                    Box::new(Raw::Text(format!("2026-10-{}", 20 + index))),
                ),
                time(60_002, -86_400 * ordinal - 1, 500),
                time(60_005, 90 + ordinal, 250_000_000),
                uuid_raw([0x60 + index; 16]),
                Raw::Tag(
                    60_008,
                    Box::new(Raw::Array(vec![uuid_raw(MOOD), uuid_raw(mood), Raw::Null])),
                ),
                Raw::Tag(
                    60_009,
                    Box::new(Raw::Array(vec![
                        uuid_raw(POINT),
                        Raw::Array(vec![
                            Raw::Array(vec![uuid_raw([0xf1; 16]), Raw::Int((-3 - ordinal).into())]),
                            Raw::Array(vec![
                                uuid_raw([0xf2; 16]),
                                Raw::Text(format!("north-{index}")),
                            ]),
                        ]),
                    ])),
                ),
                Raw::Tag(
                    60_015,
                    Box::new(Raw::Array(vec![
                        Raw::Int(ordinal.into()),
                        Raw::Text("pair".to_owned()),
                    ])),
                ),
                Raw::Tag(60_013, Box::new(discount)),
                // Beyond u64: a tag-2 bignum.
                Raw::Int(num_bigint::BigInt::from(u128::MAX) - ordinal),
            ]))
        };
        let rows = [
            (canonical_raw(uuid_raw([0x9a; 16])), fields(0, CALM)),
            (canonical_raw(uuid_raw([0x15; 16])), fields(1, WILD)),
            (canonical_raw(uuid_raw([0x47; 16])), fields(2, CALM)),
        ];
        let (_directory, graph) = store_typed_rows(relation_id, &descriptor, &rows);
        let expected = [1, 2, 0].map(|index| {
            let (key, fields) = &rows[index];
            (
                TypedKey::decode_canonical(key).expect("Uuid key decodes"),
                fields.clone(),
            )
        });
        assert!(matches!(&expected[0].0, TypedKey::Value(_)));
        assert_typed_rows(&graph, relation_id, &descriptor, &expected);
    }

    /// A07-storage-rows-02, ORNA-ROW-002: tables keyed by Date, Instant,
    /// Decimal, a tuple value and a composite (Date, Int) key keep their rows
    /// in logical key order, which differs from CBOR byte order.
    #[test]
    fn tables_keyed_by_tagged_values_round_trip_in_logical_key_order() {
        let relation_id = [0x53; 16];
        let date = |text: &str| Raw::Tag(60_001, Box::new(Raw::Text(text.to_owned())));
        let pair = |tag: u64, first: Raw, second: Raw| {
            Raw::Tag(tag, Box::new(Raw::Array(vec![first, second])))
        };
        let instant = |seconds: i64, nanos: u32| {
            pair(60_002, Raw::Int(seconds.into()), Raw::Int(nanos.into()))
        };
        let decimal = |coefficient: i64, exponent: i64| {
            pair(
                60_000,
                Raw::Int(coefficient.into()),
                Raw::Int(exponent.into()),
            )
        };
        let tuple = |number: i64, text: &str| {
            pair(60_015, Raw::Int(number.into()), Raw::Text(text.to_owned()))
        };
        let composite =
            |day: &str, number: i64| Raw::Array(vec![date(day), Raw::Int(number.into())]);
        let tuple_type = Raw::Array(vec![
            Raw::Int(3.into()),
            Raw::Array(vec![primitive_type("Int"), primitive_type("Str")]),
        ]);
        // Key field types, keys in insertion order, the same keys in logical
        // order, and one absent key of the same type.
        let cases = [
            (
                vec![primitive_type("Date")],
                vec![date("2026-10-20"), date("1999-12-31"), date("2026-01-05")],
                vec![date("1999-12-31"), date("2026-01-05"), date("2026-10-20")],
                date("2000-01-01"),
            ),
            (
                vec![primitive_type("Instant")],
                vec![instant(5, 0), instant(-3, 500), instant(0, 1)],
                vec![instant(-3, 500), instant(0, 1), instant(5, 0)],
                instant(1, 0),
            ),
            (
                vec![primitive_type("Decimal")],
                vec![decimal(1, 1), decimal(2, 0), decimal(-5, -1), decimal(0, 0)],
                vec![decimal(-5, -1), decimal(0, 0), decimal(2, 0), decimal(1, 1)],
                decimal(3, 0),
            ),
            (
                vec![tuple_type],
                vec![tuple(1, "b"), tuple(-2, "z"), tuple(1, "a"), tuple(10, "a")],
                vec![tuple(-2, "z"), tuple(1, "a"), tuple(1, "b"), tuple(10, "a")],
                tuple(2, "a"),
            ),
            (
                vec![primitive_type("Date"), primitive_type("Int")],
                vec![
                    composite("2026-01-01", 5),
                    composite("2026-01-01", -1),
                    composite("2025-12-31", 7),
                ],
                vec![
                    composite("2025-12-31", 7),
                    composite("2026-01-01", -1),
                    composite("2026-01-01", 5),
                ],
                composite("2026-01-01", 0),
            ),
        ];
        for (key_types, inserted, ordered, absent) in cases {
            let key_ids = (1..=key_types.len() as u8)
                .map(|byte| [byte; 16])
                .collect::<Vec<_>>();
            let mut fields = key_ids
                .iter()
                .zip(key_types)
                .map(|(id, ty)| field_descriptor(*id, &format!("key-{}", id[0]), ty, 0))
                .collect::<Vec<_>>();
            fields.push(field_descriptor(
                [0x09; 16],
                "note",
                primitive_type("Str"),
                1,
            ));
            let descriptor = typed_schema(relation_id, &key_ids, fields, Vec::new());
            let rows = inserted
                .into_iter()
                .enumerate()
                .map(|(index, key)| {
                    let note = canonical_raw(Raw::Array(vec![Raw::Text(format!("row-{index}"))]));
                    (canonical_raw(key), note)
                })
                .collect::<Vec<_>>();
            let (_directory, graph) = store_typed_rows(relation_id, &descriptor, &rows);
            let expected = ordered
                .into_iter()
                .map(|key| {
                    let key = canonical_raw(key);
                    let (_, note) = rows
                        .iter()
                        .find(|(inserted, _)| inserted == &key)
                        .expect("every ordered key was inserted");
                    (
                        TypedKey::decode_canonical(&key).expect("typed key decodes"),
                        note.clone(),
                    )
                })
                .collect::<Vec<_>>();
            assert_typed_rows(&graph, relation_id, &descriptor, &expected);
            let scope = graph.open_read_scope().expect("absent key scope");
            let absent =
                TypedKey::decode_canonical(&canonical_raw(absent)).expect("absent key decodes");
            assert!(
                graph
                    .lookup_row(&absent, &scope)
                    .expect("look up an absent typed key")
                    .is_none()
            );
        }
    }

}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GraphError {
    InvalidOidWidth {
        expected: usize,
        actual: usize,
    },
    MalformedOid,
    UnknownNodeKind(u64),
    NonCanonicalData,
    NodeDataTooLarge(usize),
    FanoutExceeded(usize),
    HeightExceeded(u8),
    InvalidLength(u64),
    InvalidCount(usize),
    InvalidDomain,
    /// Adjacent branch entries have non-increasing inclusive fences, so their
    /// child ranges overlap (store-format-3 section 2).
    OverlappingChildRange,
    /// A root branch with exactly one child; the canonical form collapses it.
    UncollapsedRoot,
    InvalidDigestLength,
    InvalidEmptyBlob,
    SourceTreeLimit,
    InvalidByteIndex,
    InvalidRange,
    InvalidBlobAnnotation,
    DuplicateRowKey,
    /// A delete or rekey named a logical key the pinned root does not hold.
    RowKeyAbsent,
    CandidateRequiresBranchRewrite,
    ContextMismatch,
    InvalidReadScope,
    DescriptorNotInRow,
    /// A stored row value form this reader does not resolve.
    UnsupportedRowValueForm,
    ReadQuotaExceeded,
    MetadataQuotaExceeded,
    ReadCancelled,
    ContentIdentityMismatch,
    ChunkDigestMismatch,
    WrongNodeKind {
        expected: NodeKind,
        actual: NodeKind,
    },
    InvalidTreeEnvelope,
    MissingNodeData,
    MalformedReferenceName,
    GitObjectMalformed,
    GitObjectHashMismatch,
    ChunkTooLarge(u64),
    GitCommandFailed,
    CaptureReadFailed,
    ObjectUnavailable,
    DurabilityFailed,
    ObjectDurabilityUnavailable,
    InventoryQuotaExceeded,
    PinReceiptMismatch,
    UnsupportedGitObjectType,
    InvalidProtectedRef,
    PublicationNotTerminal,
    DuplicateReference,
    MissingReference,
    UnusedReference,
    WrongReferenceKind,
    WrongObjectKind {
        expected: NativeObjectKind,
        actual: NativeObjectKind,
    },
    UnavailableObject,
    UnknownObjectAvailability,
    /// A required native object is deferred by a partial clone. Local promisor
    /// evidence proves the id is reachable through a promisor remote, but its
    /// payload is not materialized. A read never fetches it implicitly, so this
    /// state stays distinct from an unknown missing object (ORNA-GIT-002).
    PromisedObjectNotMaterialized {
        object_id: String,
    },
    /// A `cat-file` read named one object that is absent from this repository.
    /// The id is carried so a payload read of an object an offline copy does
    /// not hold names what is missing instead of reporting an anonymous
    /// availability failure (ORNA-GIT-002, ORNA-ROW-007).
    MissingObject {
        object_id: String,
    },
    LegacyFormatReadOnly(u8),
    UnsupportedWriterProfile,
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidOidWidth { expected, actual } => {
                write!(f, "native OID width is {actual}, expected {expected}")
            }
            Self::MalformedOid => f.write_str("malformed native OID"),
            Self::UnknownNodeKind(kind) => write!(f, "unknown format-3 node kind {kind}"),
            Self::NonCanonicalData => f.write_str("node data is not canonical CBOR"),
            Self::NodeDataTooLarge(size) => write!(f, "node data is {size} bytes, over 65536"),
            Self::FanoutExceeded(count) => write!(f, "native graph fanout is {count}, over 256"),
            Self::HeightExceeded(height) => write!(f, "native graph height {height} is over 64"),
            Self::InvalidLength(length) => write!(f, "length {length} exceeds the signed bound"),
            Self::InvalidCount(count) => write!(f, "invalid count {count}"),
            Self::InvalidDomain => f.write_str("invalid ORP domain"),
            Self::OverlappingChildRange => f.write_str("branch child ranges overlap"),
            Self::UncollapsedRoot => f.write_str("root branch has one child and must collapse"),
            Self::InvalidDigestLength => f.write_str("digest is not 32 bytes"),
            Self::InvalidEmptyBlob => f.write_str("empty Blob descriptor has a byte root"),
            Self::InvalidByteIndex => f.write_str("invalid byte-index spans"),
            Self::InvalidRange => f.write_str("invalid or out-of-bounds content range"),
            Self::InvalidBlobAnnotation => {
                f.write_str("Blob MIME type or suffix is not canonical MIME-1")
            }
            Self::DuplicateRowKey => f.write_str("ORP row insertion key already exists"),
            Self::RowKeyAbsent => {
                f.write_str("ORP index maintenance names a key the pinned root does not hold")
            }
            Self::CandidateRequiresBranchRewrite => {
                f.write_str("ORP candidate requires a branch or page rewrite")
            }
            Self::ContextMismatch => f.write_str("graph capability belongs to another context"),
            Self::InvalidReadScope => f.write_str("invalid owner-bound graph read scope"),
            Self::DescriptorNotInRow => {
                f.write_str("Blob descriptor is not a dependency of the admitted row")
            }
            Self::UnsupportedRowValueForm => {
                f.write_str("stored row value form is not readable here")
            }
            Self::ReadQuotaExceeded => f.write_str("graph read exceeds owner quota"),
            Self::MetadataQuotaExceeded => f.write_str("graph metadata exceeds owner quota"),
            Self::ReadCancelled => f.write_str("graph read was cancelled"),
            Self::ContentIdentityMismatch => {
                f.write_str("complete Blob length or SHA-256 does not match its descriptor")
            }
            Self::ChunkDigestMismatch => {
                f.write_str("chunk bytes do not match native object, length or SHA-256")
            }
            Self::WrongNodeKind { expected, actual } => {
                write!(
                    f,
                    "native graph node kind {actual:?}, expected {expected:?}"
                )
            }
            Self::InvalidTreeEnvelope => {
                f.write_str("native graph tree is not a canonical data/refs envelope")
            }
            Self::MissingNodeData => f.write_str("native graph tree has no data blob"),
            Self::MalformedReferenceName => {
                f.write_str("refs entry name does not match its native object ID")
            }
            Self::GitObjectMalformed => f.write_str("Git object has malformed size or encoding"),
            Self::GitObjectHashMismatch => {
                f.write_str("Git object bytes do not match their native ID")
            }
            Self::ChunkTooLarge(size) => write!(f, "chunk size {size} exceeds OGB-2 bound"),
            Self::SourceTreeLimit => {
                f.write_str("committed source tree exceeds the bounded walk limit")
            }
            Self::GitCommandFailed => f.write_str("required local Git command failed"),
            Self::CaptureReadFailed => f.write_str("authorized Blob capture input failed"),
            Self::ObjectUnavailable => f.write_str("required native Git object is unavailable"),
            Self::DurabilityFailed => f.write_str("native graph durability sync failed"),
            Self::ObjectDurabilityUnavailable => {
                f.write_str("object is not in a locally syncable Git object store")
            }
            Self::InventoryQuotaExceeded => {
                f.write_str("native graph verification exceeded its object/edge quota")
            }
            Self::PinReceiptMismatch => {
                f.write_str("PUB-3 release receipt does not authorize this pin")
            }
            Self::UnsupportedGitObjectType => {
                f.write_str("Git commit/tag objects are not valid native graph dependencies")
            }
            Self::InvalidProtectedRef => f.write_str("invalid protected-content Git ref"),
            Self::PublicationNotTerminal => {
                f.write_str("publication journal has not reached its terminal state")
            }
            Self::DuplicateReference => f.write_str("duplicate native reference"),
            Self::MissingReference => f.write_str("data names a missing native reference"),
            Self::UnusedReference => f.write_str("refs contains an unused native reference"),
            Self::WrongReferenceKind => f.write_str("native reference kind does not match data"),
            Self::WrongObjectKind { expected, actual } => {
                write!(f, "native object kind {actual:?}, expected {expected:?}")
            }
            Self::UnavailableObject => f.write_str("native object is unavailable"),
            Self::MissingObject { object_id } => {
                write!(
                    f,
                    "native object {object_id} is missing from this repository"
                )
            }
            Self::UnknownObjectAvailability => f.write_str("native object availability is unknown"),
            Self::PromisedObjectNotMaterialized { object_id } => {
                write!(
                    f,
                    "native object {object_id} is promised by a partial clone, not materialized"
                )
            }
            Self::LegacyFormatReadOnly(format) => {
                write!(f, "repository format {format} is reader-only")
            }
            Self::UnsupportedWriterProfile => f.write_str("legacy writer profile is unsupported"),
        }
    }
}

/// The single object id a `cat-file` invocation asks Git about, when the call
/// is a `cat-file` one that names exactly one object. `-t`, `-s` and the kind
/// readers (`blob`, `tree`, ...) all take the object id last, so the final
/// argument is it; the batch forms read object ids from stdin and name none
/// here, so they report no object rather than a guessed one.
fn cat_file_object_id<'a>(args: &[&'a str]) -> Option<&'a str> {
    if args.first() != Some(&"cat-file") {
        return None;
    }
    let object_id = *args.last()?;
    (!object_id.starts_with('-')).then_some(object_id)
}

/// Maps this repository's local Git observation of one object id onto the
/// graph's availability classification. Nothing is fetched or hydrated:
/// `observe_git_object` runs git with lazy fetch disabled and proves a promised
/// id through local promisor evidence only. An unreadable or malformed object
/// database stays `Unknown` rather than being reported as absent.
fn observed_object_availability(
    repository: &crate::Repository,
    object_id: &str,
) -> ObjectAvailability {
    match repository.observe_git_object(object_id) {
        Ok(crate::GitObjectState::Materialized { .. }) => ObjectAvailability::Materialized,
        Ok(crate::GitObjectState::Promised) => ObjectAvailability::Promised,
        Ok(crate::GitObjectState::Unavailable) => ObjectAvailability::Unavailable,
        Ok(crate::GitObjectState::Malformed) | Err(_) => ObjectAvailability::Unknown,
    }
}

/// Largest total of decoded bytes one graph context keeps in its node cache.
const NODE_CACHE_MAX_BYTES: usize = 8 * 1024 * 1024;
/// Largest number of objects one graph context keeps in its node cache.
const NODE_CACHE_MAX_ENTRIES: usize = 512;

/// A bounded least-recently-used cache of decoded Git trees and blobs for one
/// graph context. Entries are content-addressed, so they never go stale. Every
/// read still passes the caller's admission check before its bytes are
/// returned, so the cache changes where bytes come from, never what is
/// admitted.
#[derive(Debug, Default)]
struct NodeCache {
    entries: HashMap<(NativeObjectKind, NativeOid), (u64, Arc<Vec<u8>>)>,
    recency: BTreeMap<u64, (NativeObjectKind, NativeOid)>,
    bytes: usize,
    clock: u64,
    #[cfg(test)]
    hits: usize,
}

impl NodeCache {
    fn get(&mut self, kind: NativeObjectKind, oid: &NativeOid) -> Option<Arc<Vec<u8>>> {
        let key = (kind, oid.clone());
        let (tick, data) = self.entries.get_mut(&key)?;
        self.recency.remove(tick);
        self.clock += 1;
        *tick = self.clock;
        self.recency.insert(self.clock, key);
        #[cfg(test)]
        {
            self.hits += 1;
        }
        Some(Arc::clone(data))
    }

    fn insert(&mut self, kind: NativeObjectKind, oid: &NativeOid, data: Vec<u8>) {
        if data.len() > NODE_CACHE_MAX_BYTES {
            return;
        }
        let key = (kind, oid.clone());
        if self.entries.contains_key(&key) {
            return;
        }
        self.clock += 1;
        self.bytes += data.len();
        self.recency.insert(self.clock, key.clone());
        self.entries.insert(key, (self.clock, Arc::new(data)));
        while self.entries.len() > NODE_CACHE_MAX_ENTRIES || self.bytes > NODE_CACHE_MAX_BYTES {
            let Some((_, oldest)) = self.recency.pop_first() else {
                break;
            };
            if let Some((_, evicted)) = self.entries.remove(&oldest) {
                self.bytes -= evicted.len();
            }
        }
    }
}

#[cfg(test)]
mod node_cache_tests {
    use super::{
        GitHashAlgorithm, NODE_CACHE_MAX_BYTES, NODE_CACHE_MAX_ENTRIES, NativeObjectKind,
        NativeOid, NodeCache,
    };

    fn sha1(bytes: [u8; 20]) -> NativeOid {
        NativeOid::new(GitHashAlgorithm::Sha1, bytes).expect("a 20-byte SHA-1 id")
    }

    fn oid(byte: u8) -> NativeOid {
        sha1([byte; 20])
    }

    /// Builds the id whose 20 bytes are the big-endian index repeated.
    fn indexed(index: u32) -> NativeOid {
        let word = index.to_be_bytes();
        let mut bytes = [0u8; 20];
        for chunk in bytes.chunks_exact_mut(4) {
            chunk.copy_from_slice(&word);
        }
        sha1(bytes)
    }

    #[test]
    fn a_repeated_read_is_a_hit_and_returns_the_same_bytes() {
        let mut cache = NodeCache::default();
        cache.insert(NativeObjectKind::Blob, &oid(1), b"payload".to_vec());
        assert_eq!(cache.hits, 0);
        let first = cache.get(NativeObjectKind::Blob, &oid(1)).expect("cached");
        let second = cache.get(NativeObjectKind::Blob, &oid(1)).expect("cached");
        assert_eq!(first.as_slice(), b"payload");
        assert_eq!(second.as_slice(), b"payload");
        assert_eq!(cache.hits, 2);
    }

    #[test]
    fn kind_is_part_of_the_key() {
        let mut cache = NodeCache::default();
        cache.insert(NativeObjectKind::Blob, &oid(2), b"x".to_vec());
        assert!(cache.get(NativeObjectKind::Tree, &oid(2)).is_none());
    }

    #[test]
    fn the_entry_cap_evicts_the_least_recently_used_object() {
        let mut cache = NodeCache::default();
        for index in 0..NODE_CACHE_MAX_ENTRIES as u32 {
            cache.insert(NativeObjectKind::Blob, &indexed(index), vec![1]);
        }
        // Touch the oldest so the next-oldest becomes the eviction victim.
        assert!(cache.get(NativeObjectKind::Blob, &indexed(0)).is_some());
        cache.insert(NativeObjectKind::Blob, &indexed(u32::MAX), vec![1]);
        assert_eq!(cache.entries.len(), NODE_CACHE_MAX_ENTRIES);
        assert!(cache.get(NativeObjectKind::Blob, &indexed(1)).is_none());
        assert!(cache.get(NativeObjectKind::Blob, &indexed(0)).is_some());
    }

    #[test]
    fn the_byte_cap_evicts_least_recent_entries_until_the_bytes_fit() {
        let mut cache = NodeCache::default();
        let half = NODE_CACHE_MAX_BYTES / 2;
        cache.insert(NativeObjectKind::Blob, &indexed(1), vec![0; half]);
        cache.insert(NativeObjectKind::Blob, &indexed(2), vec![0; half]);
        assert_eq!(cache.bytes, NODE_CACHE_MAX_BYTES);
        cache.insert(NativeObjectKind::Blob, &indexed(3), vec![0; half]);
        assert!(cache.bytes <= NODE_CACHE_MAX_BYTES);
        assert_eq!(cache.entries.len(), 2);
        assert!(cache.get(NativeObjectKind::Blob, &indexed(1)).is_none());
        assert!(cache.get(NativeObjectKind::Blob, &indexed(2)).is_some());
        assert!(cache.get(NativeObjectKind::Blob, &indexed(3)).is_some());
    }

    #[test]
    fn an_entry_larger_than_the_byte_cap_is_never_cached() {
        let mut cache = NodeCache::default();
        cache.insert(
            NativeObjectKind::Blob,
            &indexed(1),
            vec![0; NODE_CACHE_MAX_BYTES + 1],
        );
        assert!(cache.entries.is_empty());
        assert_eq!(cache.bytes, 0);
    }
}

/// Reads one tree or blob through the repository's shared batch reader and
/// checks its bytes against `oid`. The header's kind and size are admitted by
/// `admit` before any byte is read; the digest is computed in-process, so an
/// object costs no process spawn of its own (ORNA-ROW-009).
fn read_checked_object(
    repository: &crate::Repository,
    algorithm: GitHashAlgorithm,
    kind: NativeObjectKind,
    oid: &NativeOid,
    admit: impl FnOnce(u64) -> Result<(), GraphError>,
) -> Result<Vec<u8>, GraphError> {
    let hex = oid.to_hex();
    let header = repository
        .objects()
        .info(&hex)
        .map_err(|_| GraphError::GitCommandFailed)?
        .ok_or_else(|| absent_object_error(repository, &hex))?;
    let actual_kind = match header.kind() {
        crate::object_reader::ObjectKind::Blob => NativeObjectKind::Blob,
        crate::object_reader::ObjectKind::Tree => NativeObjectKind::Tree,
        _ => return Err(GraphError::UnsupportedGitObjectType),
    };
    if actual_kind != kind {
        return Err(GraphError::WrongObjectKind {
            expected: kind,
            actual: actual_kind,
        });
    }
    admit(header.size())?;
    let data = repository
        .objects()
        .read_exact(&hex, &header, algorithm)
        .map_err(|_| GraphError::GitCommandFailed)?;
    if git_object_id(algorithm, kind, &data)? != *oid {
        return Err(GraphError::GitObjectHashMismatch);
    }
    Ok(data)
}

/// The Git object id of `data` as a `kind` object: the digest of the
/// `<type> <size>\0` header followed by the bytes.
fn git_object_id(
    algorithm: GitHashAlgorithm,
    kind: NativeObjectKind,
    data: &[u8],
) -> Result<NativeOid, GraphError> {
    let name = match kind {
        NativeObjectKind::Tree => "tree",
        NativeObjectKind::Blob => "blob",
    };
    let header = format!("{name} {}\0", data.len());
    let digest = match algorithm {
        GitHashAlgorithm::Sha1 => {
            use sha1::Digest as _;
            let mut hasher = sha1::Sha1::new();
            hasher.update(header.as_bytes());
            hasher.update(data);
            hasher.finalize().to_vec()
        }
        GitHashAlgorithm::Sha256 => {
            let mut hasher = Sha256::new();
            hasher.update(header.as_bytes());
            hasher.update(data);
            hasher.finalize().to_vec()
        }
    };
    NativeOid::new(algorithm, digest)
}

/// The error for one object id this repository cannot read locally. A partial
/// clone that defers the payload is reported as promised, so it is never
/// conflated with an object the repository does not know at all (ORNA-GIT-002).
/// The id is always named.
fn absent_object_error(repository: &crate::Repository, object_id: &str) -> GraphError {
    let object_id = object_id.to_ascii_lowercase();
    match observed_object_availability(repository, &object_id) {
        ObjectAvailability::Promised => GraphError::PromisedObjectNotMaterialized { object_id },
        _ => GraphError::MissingObject { object_id },
    }
}

impl std::error::Error for GraphError {}

impl GraphError {
    /// True when the bytes on disk do not match the format: a malformed or
    /// mismatched object, a node of the wrong kind, or a broken envelope.
    /// Missing objects, quota limits and git failures are not corruption.
    pub const fn is_corrupt_index(&self) -> bool {
        matches!(
            self,
            Self::MalformedOid
                | Self::UnknownNodeKind(_)
                | Self::NonCanonicalData
                | Self::InvalidTreeEnvelope
                | Self::MissingNodeData
                | Self::GitObjectMalformed
                | Self::GitObjectHashMismatch
                | Self::ContentIdentityMismatch
                | Self::ChunkDigestMismatch
                | Self::WrongNodeKind { .. }
                | Self::WrongObjectKind { .. }
                | Self::InvalidByteIndex
        )
    }

    /// True when a referenced object cannot be found in the repository: git
    /// does not know the OID, or a reference names something absent. The
    /// index itself may be intact.
    pub const fn is_unresolved_object(&self) -> bool {
        matches!(
            self,
            Self::UnknownObjectAvailability
                | Self::ObjectUnavailable
                | Self::UnavailableObject
                | Self::PromisedObjectNotMaterialized { .. }
                | Self::MissingObject { .. }
                | Self::MissingReference
        )
    }

    /// Process exit code for `ogs stats`: 2 for a corrupt index, matching the
    /// fixture manifest check's "could not verify" code; 3 for an object id
    /// that does not resolve, so scripts can tell it from corruption; and 1
    /// for anything else.
    pub const fn exit_code(&self) -> i32 {
        if self.is_corrupt_index() {
            2
        } else if self.is_unresolved_object() {
            3
        } else {
            1
        }
    }
}

#[cfg(test)]
mod exit_code_tests {
    use super::GraphError;

    #[test]
    fn help_documents_every_exit_code_the_stats_return() {
        let help = super::OGS_STATS_EXIT_CODES_HELP;
        assert_eq!(super::ObjectStats::default().exit_code(), 3);
        assert_eq!(GraphError::GitObjectHashMismatch.exit_code(), 2);
        assert_eq!(GraphError::ReadQuotaExceeded.exit_code(), 1);
        for code in [0, 1, 2, 3] {
            assert!(
                help.lines().any(|line| line.starts_with(&format!("  {code}  "))),
                "help must document exit code {code}: {help}"
            );
        }
    }

    #[test]
    fn corrupt_index_exits_2_and_other_failures_exit_1() {
        assert_eq!(GraphError::GitObjectHashMismatch.exit_code(), 2);
        assert_eq!(GraphError::InvalidByteIndex.exit_code(), 2);
        assert_eq!(GraphError::WrongNodeKind {
            expected: super::NodeKind::StoreRoot,
            actual: super::NodeKind::OrderedLeaf,
        }
        .exit_code(), 2);
        assert_eq!(GraphError::ReadQuotaExceeded.exit_code(), 1);
        assert_eq!(GraphError::ObjectUnavailable.exit_code(), 3);
        assert_eq!(GraphError::UnknownObjectAvailability.exit_code(), 3);
        assert_eq!(GraphError::GitCommandFailed.exit_code(), 1);
        assert_eq!(GraphError::GitCommandFailed.exit_code(), 1);
    }
}

fn check_length(length: u64) -> Result<(), GraphError> {
    if length > MAX_SIGNED_LENGTH {
        Err(GraphError::InvalidLength(length))
    } else {
        Ok(())
    }
}

fn check_count(count: usize) -> Result<(), GraphError> {
    if count > MAX_REFS {
        Err(GraphError::FanoutExceeded(count))
    } else {
        Ok(())
    }
}

fn check_height(height: u8) -> Result<(), GraphError> {
    if height > MAX_GRAPH_HEIGHT {
        Err(GraphError::HeightExceeded(height))
    } else {
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum OrderedDomain {
    Relations([u8; 16]),
    Rows([u8; 16], [u8; 32]),
}

fn parse_ordered_domain(domain: &[u8]) -> Result<OrderedDomain, GraphError> {
    if domain.is_empty() || domain.len() > 256 {
        return Err(GraphError::InvalidDomain);
    }
    let CborValue::Array(fields) = decode_canonical_cbor(domain)? else {
        return Err(GraphError::InvalidDomain);
    };
    let Some(CborValue::Text(name)) = fields.first() else {
        return Err(GraphError::InvalidDomain);
    };
    match name.as_str() {
        "relations" if fields.len() == 2 => {
            let Some(CborValue::Bytes(database_id)) = fields.get(1) else {
                return Err(GraphError::InvalidDomain);
            };
            Ok(OrderedDomain::Relations(
                database_id
                    .as_slice()
                    .try_into()
                    .map_err(|_| GraphError::InvalidDomain)?,
            ))
        }
        "rows" if fields.len() == 3 => {
            let (Some(CborValue::Bytes(relation_id)), Some(CborValue::Bytes(schema_digest))) =
                (fields.get(1), fields.get(2))
            else {
                return Err(GraphError::InvalidDomain);
            };
            Ok(OrderedDomain::Rows(
                relation_id
                    .as_slice()
                    .try_into()
                    .map_err(|_| GraphError::InvalidDomain)?,
                schema_digest
                    .as_slice()
                    .try_into()
                    .map_err(|_| GraphError::InvalidDomain)?,
            ))
        }
        _ => Err(GraphError::InvalidDomain),
    }
}

fn check_domain(domain: &[u8]) -> Result<(), GraphError> {
    parse_ordered_domain(domain).map(|_| ())
}

fn check_leaf_key_order(domain: &[u8], entries: &[OrderedLeafEntry]) -> Result<(), GraphError> {
    let domain = parse_ordered_domain(domain)?;
    let mut previous: Option<crate::row_store::TypedKey> = None;
    for entry in entries {
        let key = crate::row_store::TypedKey::decode_canonical(&entry.key)
            .map_err(|_| GraphError::NonCanonicalData)?;
        if matches!(domain, OrderedDomain::Relations(_))
            && !matches!(&key, crate::row_store::TypedKey::Bytes(bytes) if bytes.len() == 16)
        {
            return Err(GraphError::InvalidDomain);
        }
        if previous.as_ref().is_some_and(|previous| previous >= &key) {
            return Err(GraphError::NonCanonicalData);
        }
        previous = Some(key);
    }
    Ok(())
}

fn check_branch_fence_order(
    domain: &[u8],
    entries: &[OrderedBranchEntry],
) -> Result<(), GraphError> {
    let domain = parse_ordered_domain(domain)?;
    let mut previous: Option<crate::row_store::TypedKey> = None;
    for entry in entries {
        let fence = crate::row_store::TypedKey::decode_canonical(&entry.inclusive_max_key)
            .map_err(|_| GraphError::NonCanonicalData)?;
        if matches!(domain, OrderedDomain::Relations(_))
            && !matches!(&fence, crate::row_store::TypedKey::Bytes(bytes) if bytes.len() == 16)
        {
            return Err(GraphError::InvalidDomain);
        }
        if previous.as_ref().is_some_and(|previous| previous >= &fence) {
            return Err(GraphError::OverlappingChildRange);
        }
        previous = Some(fence);
    }
    Ok(())
}

fn validate_canonical_value(bytes: &[u8]) -> Result<(), GraphError> {
    decode_canonical_cbor(bytes).map(|_| ())
}

pub(crate) fn canonical_value_bytes(value: &CborValue) -> Vec<u8> {
    let mut bytes = Vec::new();
    value.encode(&mut bytes);
    bytes
}

pub(crate) fn decode_canonical_cbor(bytes: &[u8]) -> Result<CborValue, GraphError> {
    decode_canonical_cbor_limited(bytes, NODE_DATA_LIMIT)
}

pub(crate) fn decode_canonical_content(bytes: &[u8]) -> Result<CborValue, GraphError> {
    decode_canonical_cbor_limited(bytes, crate::row_store::ROW_CANONICAL_LIMIT)
}

fn decode_canonical_cbor_limited(bytes: &[u8], maximum: usize) -> Result<CborValue, GraphError> {
    if bytes.len() > maximum {
        return Err(GraphError::InvalidLength(bytes.len() as u64));
    }
    let mut reader = CborReader::new(bytes);
    let value = reader.value()?;
    if !reader.is_finished() || canonical_value_bytes(&value) != bytes {
        return Err(GraphError::NonCanonicalData);
    }
    Ok(value)
}

fn oid_from_width(bytes: &[u8]) -> Result<NativeObjectId, GraphError> {
    let algorithm = match bytes.len() {
        20 => GitHashAlgorithm::Sha1,
        32 => GitHashAlgorithm::Sha256,
        actual => {
            return Err(GraphError::InvalidOidWidth {
                expected: 20,
                actual,
            });
        }
    };
    NativeObjectId::new(algorithm, bytes)
}

pub(crate) fn leaf_value_dependencies(
    domain: &[u8],
    value: &[u8],
) -> Result<Vec<NativeDependency>, GraphError> {
    let parsed = decode_canonical_cbor(value)?;
    let mut dependencies = BTreeMap::<NativeObjectId, NativeObjectKind>::new();
    match parse_ordered_domain(domain)? {
        OrderedDomain::Relations(_) => {
            let CborValue::Array(fields) = parsed else {
                return Err(GraphError::NonCanonicalData);
            };
            if fields.len() != 4
                || !matches!(
                    fields.get(3),
                    Some(CborValue::Unsigned(count)) if *count <= MAX_SIGNED_LENGTH
                )
            {
                return Err(GraphError::NonCanonicalData);
            }
            for index in [0usize, 1] {
                let CborValue::Bytes(oid) =
                    fields.get(index).ok_or(GraphError::NonCanonicalData)?
                else {
                    return Err(GraphError::NonCanonicalData);
                };
                insert_leaf_dependency(
                    &mut dependencies,
                    oid_from_width(oid)?,
                    NativeObjectKind::Tree,
                )?;
            }
            match fields.get(2).ok_or(GraphError::NonCanonicalData)? {
                CborValue::Null => {}
                CborValue::Bytes(oid) => insert_leaf_dependency(
                    &mut dependencies,
                    oid_from_width(oid)?,
                    NativeObjectKind::Tree,
                )?,
                _ => return Err(GraphError::NonCanonicalData),
            }
        }
        OrderedDomain::Rows(_, _) => {
            if !matches!(parsed, CborValue::Array(_) | CborValue::Tag(60113, _)) {
                return Err(GraphError::NonCanonicalData);
            }
            collect_row_dependencies(&parsed, &mut dependencies)?;
        }
    }
    Ok(dependencies
        .into_iter()
        .map(|(oid, kind)| NativeDependency::new(oid, kind))
        .collect())
}

struct VerifiedValueOverflow {
    reference: crate::row_store::ValueOverflowRef,
    semantic_value: CborValue,
    encoded_value: Option<Vec<u8>>,
}

fn stored_field_object_ids(descriptor: &SchemaDescriptor) -> Result<Vec<[u8; 16]>, GraphError> {
    let Raw::Map(schema) = descriptor.raw() else {
        return Err(GraphError::NonCanonicalData);
    };
    let fields_key = Raw::Int(3u8.into());
    let Raw::Array(declared_fields) = schema
        .iter()
        .find_map(|(key, value)| (key == &fields_key).then_some(value))
        .ok_or(GraphError::NonCanonicalData)?
    else {
        return Err(GraphError::NonCanonicalData);
    };

    let stored_role = Raw::Int(1u8.into());
    let mut identities = Vec::new();
    let mut unique = BTreeSet::new();
    for field in declared_fields {
        let Raw::Array(parts) = field else {
            return Err(GraphError::NonCanonicalData);
        };
        let [identity, _, _, role, _flags] = parts.as_slice() else {
            return Err(GraphError::NonCanonicalData);
        };
        if role != &stored_role {
            continue;
        }
        let Raw::Tag(37, identity) = identity else {
            return Err(GraphError::NonCanonicalData);
        };
        let Raw::Bytes(identity) = identity.as_ref() else {
            return Err(GraphError::NonCanonicalData);
        };
        let identity: [u8; 16] = identity
            .as_slice()
            .try_into()
            .map_err(|_| GraphError::NonCanonicalData)?;
        if !unique.insert(identity) {
            return Err(GraphError::NonCanonicalData);
        }
        identities.push(identity);
    }
    Ok(identities)
}

/// Reads the payload of a canonical Orna option value.
fn option_payload_value(value: &CborValue) -> Result<Option<&CborValue>, GraphError> {
    let CborValue::Tag(60013, payload) = value else {
        return Err(GraphError::NonCanonicalData);
    };
    let CborValue::Array(fields) = payload.as_ref() else {
        return Err(GraphError::NonCanonicalData);
    };
    match fields.as_slice() {
        [CborValue::Unsigned(0)] => Ok(None),
        [CborValue::Unsigned(1), value] => Ok(Some(value)),
        _ => Err(GraphError::NonCanonicalData),
    }
}

/// Collects normalized Blob metadata reachable inside a canonical stored row.
/// Option None contributes nothing; Some recursively visits its value.
fn collect_blob_value_fields(
    value: &CborValue,
    output: &mut Vec<(MediaAnnotation, crate::blob_store::ContentIdentity)>,
) -> Result<(), GraphError> {
    match value {
        CborValue::Tag(60013, _) => match option_payload_value(value)? {
            None => Ok(()),
            Some(value) => collect_blob_value_fields(value, output),
        },
        CborValue::Tag(60112, payload) => {
            let CborValue::Array(fields) = payload.as_ref() else {
                return Err(GraphError::NonCanonicalData);
            };
            if fields.len() != 4 {
                return Err(GraphError::NonCanonicalData);
            }
            let (Some(CborValue::Unsigned(length)), Some(CborValue::Bytes(digest))) =
                (fields.first(), fields.get(1))
            else {
                return Err(GraphError::NonCanonicalData);
            };
            let Some(CborValue::Text(media_type)) = fields.get(2) else {
                return Err(GraphError::NonCanonicalData);
            };
            if *length > MAX_SIGNED_LENGTH || digest.len() != 32 {
                return Err(GraphError::NonCanonicalData);
            }
            let suffix = match fields.get(3) {
                Some(CborValue::Null) => None,
                Some(CborValue::Text(suffix)) => Some(suffix.as_str()),
                _ => return Err(GraphError::NonCanonicalData),
            };
            let annotation = MediaAnnotation::new(media_type, suffix)
                .map_err(|_| GraphError::InvalidBlobAnnotation)?;
            let mut digest_bytes = [0u8; 32];
            digest_bytes.copy_from_slice(digest);
            let identity = crate::blob_store::ContentIdentity::new(*length, digest_bytes)
                .map_err(|_| GraphError::InvalidLength(*length))?;
            output.push((annotation, identity));
            Ok(())
        }
        CborValue::Array(values) => {
            for item in values {
                collect_blob_value_fields(item, output)?;
            }
            Ok(())
        }
        CborValue::Map(entries) => {
            for (key, item) in entries {
                collect_blob_value_fields(key, output)?;
                collect_blob_value_fields(item, output)?;
            }
            Ok(())
        }
        // A canonical overflow value cannot appear here: `to_sov_value`
        // dereferences it before this walk sees the value.
        CborValue::Tag(60113, _) => Err(GraphError::UnsupportedRowValueForm),
        // A semantic value carries only SOV-3 Blob tags.
        CborValue::Tag(60110 | 60111, _) => Err(GraphError::NonCanonicalData),
        // Records, enum payloads, tuples and other typed values can hold Blobs.
        CborValue::Tag(_, payload) => collect_blob_value_fields(payload, output),
        _ => Ok(()),
    }
}

fn rows_domain_for_version(version: &crate::row_store::RowMapVersion) -> Vec<u8> {
    rows_domain(version.relation_id(), version.schema().schema_digest())
}

fn rows_domain_for_parts(relation_id: &[u8; 16], schema_digest: &[u8; 32]) -> Vec<u8> {
    rows_domain(relation_id, schema_digest)
}

fn decode_candidate_blob_metadata(
    encoded_row: &[u8],
    transfer: &ProtectedContentTransfer,
    admitted_media_type: &str,
    admitted_suffix: Option<&str>,
) -> Result<ProtectedBlobMetadata, GraphError> {
    let CborValue::Array(mut fields) = decode_canonical_cbor(encoded_row)? else {
        return Err(GraphError::NonCanonicalData);
    };
    if fields.len() != 1 {
        return Err(GraphError::NonCanonicalData);
    }
    let CborValue::Tag(60111, payload) = fields.remove(0) else {
        return Err(GraphError::NonCanonicalData);
    };
    let CborValue::Array(fields) = *payload else {
        return Err(GraphError::NonCanonicalData);
    };
    if fields.len() != 5 {
        return Err(GraphError::NonCanonicalData);
    }
    let [
        CborValue::Unsigned(length),
        CborValue::Bytes(digest),
        CborValue::Text(media_type),
        suffix,
        CborValue::Bytes(descriptor_oid),
    ] = fields.as_slice()
    else {
        return Err(GraphError::NonCanonicalData);
    };
    if !matches!(suffix, CborValue::Null | CborValue::Text(_)) {
        return Err(GraphError::NonCanonicalData);
    }
    let expected_digest = transfer.content_identity().sha256();
    if digest.len() != 32
        || *length != transfer.content_identity().length()
        || digest.as_slice() != expected_digest.as_slice()
        || descriptor_oid.as_slice() != transfer.descriptor_oid().as_bytes()
    {
        return Err(GraphError::ContentIdentityMismatch);
    }
    let mut digest_bytes = [0; 32];
    digest_bytes.copy_from_slice(digest);
    let identity = crate::ContentIdentity::new(*length, digest_bytes)
        .map_err(|_| GraphError::ContentIdentityMismatch)?;
    let suffix = match suffix {
        CborValue::Null => None,
        CborValue::Text(suffix) => Some(suffix.clone()),
        _ => unreachable!("suffix variant checked above"),
    };
    bind_candidate_blob_annotation(
        media_type,
        suffix.as_deref(),
        admitted_media_type,
        admitted_suffix,
    )?;
    Ok(ProtectedBlobMetadata {
        content_identity: identity,
        media_type: media_type.clone(),
        suffix,
    })
}

fn overflow_tag_oid(
    payload: &CborValue,
    algorithm: GitHashAlgorithm,
) -> Result<NativeOid, GraphError> {
    let CborValue::Array(fields) = payload else {
        return Err(GraphError::NonCanonicalData);
    };
    if fields.len() != 1 {
        return Err(GraphError::NonCanonicalData);
    }
    let CborValue::Bytes(bytes) = fields.first().ok_or(GraphError::NonCanonicalData)? else {
        return Err(GraphError::NonCanonicalData);
    };
    NativeOid::new(algorithm, bytes)
}

fn collect_overflow_oids(
    value: &CborValue,
    algorithm: GitHashAlgorithm,
    output: &mut BTreeSet<NativeOid>,
) -> Result<(), GraphError> {
    match value {
        CborValue::Tag(60013, _) => {
            if let Some(value) = option_payload_value(value)? {
                collect_overflow_oids(value, algorithm, output)?;
            }
        }
        CborValue::Tag(60113, payload) => {
            output.insert(overflow_tag_oid(payload, algorithm)?);
        }
        CborValue::Tag(60111 | 60021, _) => {}
        // OVB-2 and SOV-3 Blob tags never appear in a stored ROV-3 value.
        CborValue::Tag(60110 | 60112, _) => return Err(GraphError::NonCanonicalData),
        CborValue::Tag(_, payload) => collect_overflow_oids(payload, algorithm, output)?,
        CborValue::Array(values) => {
            for value in values {
                collect_overflow_oids(value, algorithm, output)?;
            }
        }
        CborValue::Map(entries) => {
            for (key, value) in entries {
                collect_overflow_oids(key, algorithm, output)?;
                collect_overflow_oids(value, algorithm, output)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// The SOV-3 form of a stored ROV-3 value (store-format-3 section 5). Each
/// stored Blob reference (tag 60111) drops its native descriptor and becomes
/// tag 60112, each overflow reference (tag 60113) is replaced by the semantic
/// value `resolve_overflow` returns for its payload, and maps are re-sorted by
/// their converted keys. `oid_width` is the repository's native object width,
/// which a stored descriptor reference must have. The reader verifying a
/// kind-3 digest and the writer computing one both convert through here.
pub(crate) fn sov3_value(
    value: CborValue,
    oid_width: usize,
    resolve_overflow: &mut impl FnMut(CborValue) -> Result<CborValue, GraphError>,
) -> Result<CborValue, GraphError> {
    match value {
        CborValue::Tag(60013, payload) => {
            let CborValue::Array(mut fields) = *payload else {
                return Err(GraphError::NonCanonicalData);
            };
            match fields.as_slice() {
                [CborValue::Unsigned(0)] => {
                    Ok(CborValue::Tag(60013, Box::new(CborValue::Array(fields))))
                }
                [CborValue::Unsigned(1), _] => {
                    let value = fields.pop().ok_or(GraphError::NonCanonicalData)?;
                    fields.push(sov3_value(value, oid_width, resolve_overflow)?);
                    Ok(CborValue::Tag(60013, Box::new(CborValue::Array(fields))))
                }
                _ => Err(GraphError::NonCanonicalData),
            }
        }
        CborValue::Tag(60111, payload) => {
            let CborValue::Array(fields) = *payload else {
                return Err(GraphError::NonCanonicalData);
            };
            if fields.len() != 5
                || !matches!(fields.first(), Some(CborValue::Unsigned(length)) if *length <= MAX_SIGNED_LENGTH)
                || !matches!(fields.get(1), Some(CborValue::Bytes(digest)) if digest.len() == 32)
                || !matches!(fields.get(2), Some(CborValue::Text(_)))
                || !matches!(fields.get(3), Some(CborValue::Null | CborValue::Text(_)))
                || !matches!(fields.get(4), Some(CborValue::Bytes(oid)) if oid.len() == oid_width)
            {
                return Err(GraphError::NonCanonicalData);
            }
            let Some(CborValue::Text(media_type)) = fields.get(2) else {
                return Err(GraphError::NonCanonicalData);
            };
            let suffix = match fields.get(3) {
                Some(CborValue::Null) => None,
                Some(CborValue::Text(suffix)) => Some(suffix.as_str()),
                _ => return Err(GraphError::NonCanonicalData),
            };
            validate_canonical_mime1_annotation(media_type, suffix)?;
            Ok(CborValue::Tag(
                60112,
                Box::new(CborValue::Array(fields.into_iter().take(4).collect())),
            ))
        }
        CborValue::Tag(60021, payload) => {
            if !matches!(payload.as_ref(), CborValue::Array(fields) if fields.len() == 3) {
                return Err(GraphError::NonCanonicalData);
            }
            Ok(CborValue::Tag(60021, payload))
        }
        CborValue::Tag(60113, payload) => resolve_overflow(*payload),
        CborValue::Array(values) => Ok(CborValue::Array(
            values
                .into_iter()
                .map(|value| sov3_value(value, oid_width, resolve_overflow))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        CborValue::Map(entries) => {
            let mut transformed = entries
                .into_iter()
                .map(|(key, value)| {
                    Ok((
                        sov3_value(key, oid_width, resolve_overflow)?,
                        sov3_value(value, oid_width, resolve_overflow)?,
                    ))
                })
                .collect::<Result<Vec<_>, GraphError>>()?;
            transformed.sort_by_cached_key(|(key, _)| canonical_value_bytes(key));
            if transformed.windows(2).any(|pair| {
                canonical_value_bytes(&pair[0].0) == canonical_value_bytes(&pair[1].0)
            }) {
                return Err(GraphError::NonCanonicalData);
            }
            Ok(CborValue::Map(transformed))
        }
        // OVB-2 and SOV-3 Blob tags never appear in a stored ROV-3 value.
        CborValue::Tag(60110 | 60112, _) => Err(GraphError::NonCanonicalData),
        // Other typed values keep their tag; only nested Blob and overflow
        // references change between ROV-3 and SOV-3.
        CborValue::Tag(tag, payload) => Ok(CborValue::Tag(
            tag,
            Box::new(sov3_value(*payload, oid_width, resolve_overflow)?),
        )),
        scalar => Ok(scalar),
    }
}

/// `MediaAnnotation::new` admits only the canonical wire pair, so its success
/// is the whole MIME-1 check.
fn validate_canonical_mime1_annotation(
    media_type: &str,
    suffix: Option<&str>,
) -> Result<(), GraphError> {
    MediaAnnotation::new(media_type, suffix)
        .map(drop)
        .map_err(|_| GraphError::InvalidBlobAnnotation)
}

/// Binds the annotation of a candidate row to the annotation its pin was
/// admitted with.
///
/// The transfer record carries content identity alone, so the row's own bytes
/// are the only place a candidate annotation appears. Accepting whatever that
/// row spells would let a candidate claim an annotation the pin never admitted,
/// so the stored spelling must be canonical MIME-1 and must equal the admitted
/// `(media_type, suffix)` pair exactly.
fn bind_candidate_blob_annotation(
    stored_media_type: &str,
    stored_suffix: Option<&str>,
    admitted_media_type: &str,
    admitted_suffix: Option<&str>,
) -> Result<(), GraphError> {
    validate_canonical_mime1_annotation(stored_media_type, stored_suffix)?;
    if stored_media_type != admitted_media_type || stored_suffix != admitted_suffix {
        return Err(GraphError::InvalidBlobAnnotation);
    }
    Ok(())
}

fn stored_uuid(value: &CborValue) -> Result<[u8; 16], GraphError> {
    let CborValue::Tag(37, payload) = value else {
        return Err(GraphError::NonCanonicalData);
    };
    let CborValue::Bytes(bytes) = payload.as_ref() else {
        return Err(GraphError::NonCanonicalData);
    };
    bytes.as_slice().try_into().map_err(|_| GraphError::NonCanonicalData)
}

pub(crate) fn row_fields_dependencies(value: &[u8]) -> Result<Vec<NativeDependency>, GraphError> {
    let parsed = decode_canonical_cbor(value)?;
    if !matches!(parsed, CborValue::Array(_) | CborValue::Tag(60113, _)) {
        return Err(GraphError::NonCanonicalData);
    }
    let mut dependencies = BTreeMap::new();
    collect_row_dependencies(&parsed, &mut dependencies)?;
    Ok(dependencies
        .into_iter()
        .map(|(oid, kind)| NativeDependency::new(oid, kind))
        .collect())
}

pub(crate) fn collect_row_dependencies(
    value: &CborValue,
    dependencies: &mut BTreeMap<NativeObjectId, NativeObjectKind>,
) -> Result<(), GraphError> {
    match value {
        CborValue::Tag(60013, _) => {
            if let Some(value) = option_payload_value(value)? {
                collect_row_dependencies(value, dependencies)?;
            }
        }
        CborValue::Tag(60111, payload) => {
            let CborValue::Array(fields) = payload.as_ref() else {
                return Err(GraphError::NonCanonicalData);
            };
            if fields.len() != 5
                || !matches!(
                    fields.first(),
                    Some(CborValue::Unsigned(length)) if *length <= MAX_SIGNED_LENGTH
                )
                || !matches!(
                    fields.get(1),
                    Some(CborValue::Bytes(digest)) if digest.len() == 32
                )
                || !matches!(fields.get(2), Some(CborValue::Text(_)))
                || !matches!(fields.get(3), Some(CborValue::Null | CborValue::Text(_)))
            {
                return Err(GraphError::NonCanonicalData);
            }
            let Some(CborValue::Text(media_type)) = fields.get(2) else {
                return Err(GraphError::NonCanonicalData);
            };
            let suffix = match fields.get(3) {
                Some(CborValue::Null) => None,
                Some(CborValue::Text(suffix)) => Some(suffix.as_str()),
                _ => return Err(GraphError::NonCanonicalData),
            };
            validate_canonical_mime1_annotation(media_type, suffix)?;
            let CborValue::Bytes(oid_bytes) = fields.get(4).ok_or(GraphError::NonCanonicalData)?
            else {
                return Err(GraphError::NonCanonicalData);
            };
            insert_leaf_dependency(
                dependencies,
                oid_from_width(oid_bytes)?,
                NativeObjectKind::Tree,
            )?;
        }
        CborValue::Tag(60113, payload) => {
            let CborValue::Array(fields) = payload.as_ref() else {
                return Err(GraphError::NonCanonicalData);
            };
            if fields.len() != 1 {
                return Err(GraphError::NonCanonicalData);
            }
            let CborValue::Bytes(oid_bytes) = fields.first().ok_or(GraphError::NonCanonicalData)?
            else {
                return Err(GraphError::NonCanonicalData);
            };
            insert_leaf_dependency(
                dependencies,
                oid_from_width(oid_bytes)?,
                NativeObjectKind::Tree,
            )?;
        }
        CborValue::Tag(60021, payload) => {
            let CborValue::Array(fields) = payload.as_ref() else {
                return Err(GraphError::NonCanonicalData);
            };
            let [database, relation, key] = fields.as_slice() else {
                return Err(GraphError::NonCanonicalData);
            };
            stored_uuid(database)?;
            stored_uuid(relation)?;
            crate::row_store::TypedKey::decode_canonical(&canonical_value_bytes(key))
                .map_err(|_| GraphError::NonCanonicalData)?;
        }
        // OVB-2 and SOV-3 Blob tags never appear in a stored ROV-3 value.
        CborValue::Tag(60110 | 60112, _) => return Err(GraphError::NonCanonicalData),
        // Every other admitted tag is a typed value; its references, if any,
        // are nested in its payload (store-format-3 §2, final_model.py).
        CborValue::Tag(_, payload) => collect_row_dependencies(payload, dependencies)?,
        CborValue::Array(values) => {
            for value in values {
                collect_row_dependencies(value, dependencies)?;
            }
        }
        CborValue::Map(entries) => {
            for (key, value) in entries {
                collect_row_dependencies(key, dependencies)?;
                collect_row_dependencies(value, dependencies)?;
            }
        }
        CborValue::Unsigned(_)
        | CborValue::Negative(_)
        | CborValue::Bytes(_)
        | CborValue::Text(_)
        | CborValue::Bool(_)
        | CborValue::Float64(_)
        | CborValue::Null => {}
    }
    Ok(())
}
/// Counts Blob references belonging directly to this encoded value. A nested
/// kind-3 reference is a separate bounded value and its dependencies are not
/// charged to this parent's inline threshold.
pub(crate) fn direct_blob_reference_count(value: &CborValue) -> Result<usize, GraphError> {
    match value {
        CborValue::Tag(60111, _) => Ok(1),
        CborValue::Tag(60113, _) => Ok(0),
        CborValue::Tag(60013, payload) => {
            let CborValue::Array(fields) = payload.as_ref() else {
                return Err(GraphError::NonCanonicalData);
            };
            match fields.as_slice() {
                [CborValue::Unsigned(0)] => Ok(0),
                [CborValue::Unsigned(1), value] => direct_blob_reference_count(value),
                _ => Err(GraphError::NonCanonicalData),
            }
        },
        CborValue::Tag(60110 | 60112, _) => Err(GraphError::NonCanonicalData),
        CborValue::Tag(_, payload) => direct_blob_reference_count(payload),
        CborValue::Array(values) => values.iter().try_fold(0usize, |count, value| {
            count
                .checked_add(direct_blob_reference_count(value)?)
                .ok_or(GraphError::InvalidCount(usize::MAX))
        }),
        CborValue::Map(entries) => entries.iter().try_fold(0usize, |count, (key, value)| {
            let key_count = direct_blob_reference_count(key)?;
            let value_count = direct_blob_reference_count(value)?;
            count
                .checked_add(key_count)
                .and_then(|count| count.checked_add(value_count))
                .ok_or(GraphError::InvalidCount(usize::MAX))
        }),
        _ => Ok(0),
    }
}

fn insert_leaf_dependency(
    dependencies: &mut BTreeMap<NativeObjectId, NativeObjectKind>,
    oid: NativeObjectId,
    kind: NativeObjectKind,
) -> Result<(), GraphError> {
    if dependencies
        .insert(oid, kind)
        .is_some_and(|previous| previous != kind)
    {
        return Err(GraphError::WrongReferenceKind);
    }
    Ok(())
}

fn validate_byte_entries(
    height: u8,
    total_length: u64,
    entries: &[ByteIndexEntry],
) -> Result<(), GraphError> {
    check_count(entries.len())?;
    let expected_hash = height == 0;
    let mut total = 0u64;
    for entry in entries {
        if entry.span_length == 0 || (entry.chunk_sha256.is_some() != expected_hash) {
            return Err(GraphError::InvalidByteIndex);
        }
        total = total
            .checked_add(entry.span_length)
            .ok_or(GraphError::InvalidByteIndex)?;
        if let Some(digest) = entry.chunk_sha256 {
            let _ = digest;
        }
    }
    if total != total_length || (total_length == 0 && !entries.is_empty()) {
        return Err(GraphError::InvalidByteIndex);
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CborValue {
    Unsigned(u64),
    Negative(u64),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<CborValue>),
    Map(Vec<(CborValue, CborValue)>),
    Tag(u64, Box<CborValue>),
    Bool(bool),
    Float64(u64),
    Null,
}

impl CborValue {
    fn encode(&self, output: &mut Vec<u8>) {
        match self {
            Self::Unsigned(value) => uint(output, *value),
            Self::Negative(value) => {
                cbor_head(output, 1, *value);
            }
            Self::Bytes(value) => bytes(output, value),
            Self::Text(value) => {
                cbor_head(output, 3, value.len() as u64);
                output.extend_from_slice(value.as_bytes());
            }
            Self::Array(values) => {
                array(output, values.len() as u64);
                for value in values {
                    value.encode(output);
                }
            }
            Self::Map(entries) => {
                cbor_head(output, 5, entries.len() as u64);
                for (key, value) in entries {
                    key.encode(output);
                    value.encode(output);
                }
            }
            Self::Tag(tag, value) => {
                cbor_head(output, 6, *tag);
                value.encode(output);
            }
            Self::Bool(value) => output.push(if *value { 0xf5 } else { 0xf4 }),
            Self::Float64(value) => {
                output.push(0xfb);
                output.extend_from_slice(&value.to_be_bytes());
            }
            Self::Null => null(output),
        }
    }
}

struct CborReader<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> CborReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    fn is_finished(&self) -> bool {
        self.cursor == self.bytes.len()
    }

    fn value(&mut self) -> Result<CborValue, GraphError> {
        let head = *self
            .bytes
            .get(self.cursor)
            .ok_or(GraphError::NonCanonicalData)?;
        self.cursor += 1;
        let major = head >> 5;
        let additional = head & 0x1f;
        if major == 7 {
            return match additional {
                20 => Ok(CborValue::Bool(false)),
                21 => Ok(CborValue::Bool(true)),
                22 => Ok(CborValue::Null),
                27 => Ok(CborValue::Float64(self.read_u64()?)),
                _ => Err(GraphError::NonCanonicalData),
            };
        }
        let length = self.argument(additional)?;
        match major {
            0 => Ok(CborValue::Unsigned(length)),
            // Every negative head is an Orna Int; values below -2^64 use tag 3.
            1 => Ok(CborValue::Negative(length)),
            2 => {
                let length = usize::try_from(length).map_err(|_| GraphError::NonCanonicalData)?;
                let end = self
                    .cursor
                    .checked_add(length)
                    .ok_or(GraphError::NonCanonicalData)?;
                let bytes = self
                    .bytes
                    .get(self.cursor..end)
                    .ok_or(GraphError::NonCanonicalData)?
                    .to_vec();
                self.cursor = end;
                Ok(CborValue::Bytes(bytes))
            }
            3 => {
                let length = usize::try_from(length).map_err(|_| GraphError::NonCanonicalData)?;
                let end = self
                    .cursor
                    .checked_add(length)
                    .ok_or(GraphError::NonCanonicalData)?;
                let text = std::str::from_utf8(
                    self.bytes
                        .get(self.cursor..end)
                        .ok_or(GraphError::NonCanonicalData)?,
                )
                .map_err(|_| GraphError::NonCanonicalData)?
                .to_owned();
                self.cursor = end;
                Ok(CborValue::Text(text))
            }
            4 => {
                let length = usize::try_from(length).map_err(|_| GraphError::NonCanonicalData)?;
                if length > NODE_DATA_LIMIT {
                    return Err(GraphError::NodeDataTooLarge(length));
                }
                let mut values = Vec::with_capacity(length);
                for _ in 0..length {
                    values.push(self.value()?);
                }
                Ok(CborValue::Array(values))
            }
            5 => {
                let length = usize::try_from(length).map_err(|_| GraphError::NonCanonicalData)?;
                if length > NODE_DATA_LIMIT {
                    return Err(GraphError::NodeDataTooLarge(length));
                }
                let mut entries = Vec::with_capacity(length);
                let mut previous_key = None;
                for _ in 0..length {
                    let key_start = self.cursor;
                    let key = self.value()?;
                    let key_bytes = self.bytes[key_start..self.cursor].to_vec();
                    if previous_key
                        .as_ref()
                        .is_some_and(|previous| previous >= &key_bytes)
                    {
                        return Err(GraphError::NonCanonicalData);
                    }
                    previous_key = Some(key_bytes);
                    entries.push((key, self.value()?));
                }
                Ok(CborValue::Map(entries))
            }
            6 => {
                if !orna_value_v1::is_format3_node_tag(length) {
                    return Err(GraphError::NonCanonicalData);
                }
                let payload = self.value()?;
                // A bignum spells only an Int beyond the 64-bit heads: at least
                // nine big-endian bytes without a leading zero.
                if matches!(length, 2 | 3)
                    && !matches!(&payload, CborValue::Bytes(bytes) if bytes.len() >= 9 && bytes[0] != 0)
                {
                    return Err(GraphError::NonCanonicalData);
                }
                Ok(CborValue::Tag(length, Box::new(payload)))
            }
            _ => Err(GraphError::NonCanonicalData),
        }
    }

    fn argument(&mut self, additional: u8) -> Result<u64, GraphError> {
        let (value, width) = match additional {
            0..=23 => (u64::from(additional), 0),
            24 => (u64::from(self.read_u8()?), 1),
            25 => (u64::from(self.read_u16()?), 2),
            26 => (u64::from(self.read_u32()?), 4),
            27 => (self.read_u64()?, 8),
            _ => return Err(GraphError::NonCanonicalData),
        };
        if width != 0
            && ((width == 1 && value < 24)
                || (width == 2 && value <= u64::from(u8::MAX))
                || (width == 4 && value <= u64::from(u16::MAX))
                || (width == 8 && value <= u64::from(u32::MAX)))
        {
            return Err(GraphError::NonCanonicalData);
        }
        Ok(value)
    }

    fn read_u8(&mut self) -> Result<u8, GraphError> {
        let value = *self
            .bytes
            .get(self.cursor)
            .ok_or(GraphError::NonCanonicalData)?;
        self.cursor += 1;
        Ok(value)
    }

    fn read_u16(&mut self) -> Result<u16, GraphError> {
        let bytes = self
            .bytes
            .get(self.cursor..self.cursor + 2)
            .ok_or(GraphError::NonCanonicalData)?;
        self.cursor += 2;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn read_u32(&mut self) -> Result<u32, GraphError> {
        let bytes = self
            .bytes
            .get(self.cursor..self.cursor + 4)
            .ok_or(GraphError::NonCanonicalData)?;
        self.cursor += 4;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_u64(&mut self) -> Result<u64, GraphError> {
        let bytes = self
            .bytes
            .get(self.cursor..self.cursor + 8)
            .ok_or(GraphError::NonCanonicalData)?;
        self.cursor += 8;
        Ok(u64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }
}

fn cbor_head(output: &mut Vec<u8>, major: u8, value: u64) {
    let prefix = major << 5;
    match value {
        0..=23 => output.push(prefix | value as u8),
        24..=255 => {
            output.extend_from_slice(&[prefix | 24, value as u8]);
        }
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

fn decode_node(value: CborValue, algorithm: GitHashAlgorithm) -> Result<NodeData, GraphError> {
    let fields = array_fields(value)?;
    let version = unsigned(fields.first())?;
    if version != 1 {
        return Err(GraphError::NonCanonicalData);
    }
    let kind = NodeKind::from_number(unsigned(fields.get(1))?)?;
    match kind {
        NodeKind::StoreRoot => Ok(NodeData::StoreRoot {
            relation_map: oid(bytes_field(fields.get(2))?, algorithm)?,
        }),
        NodeKind::OrderedLeaf => {
            let domain = canonical_value_bytes(fields.get(2).ok_or(GraphError::NonCanonicalData)?);
            check_domain(&domain)?;
            let entries =
                array_fields(fields.get(3).cloned().ok_or(GraphError::NonCanonicalData)?)?
                    .into_iter()
                    .map(|value| {
                        let fields = array_fields(value)?;
                        if fields.len() != 2 {
                            return Err(GraphError::NonCanonicalData);
                        }
                        Ok(OrderedLeafEntry {
                            key: canonical_value_bytes(
                                fields.first().ok_or(GraphError::NonCanonicalData)?,
                            ),
                            value: canonical_value_bytes(
                                fields.get(1).ok_or(GraphError::NonCanonicalData)?,
                            ),
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
            Ok(NodeData::OrderedLeaf { domain, entries })
        }
        NodeKind::OrderedBranch => {
            let domain = canonical_value_bytes(fields.get(2).ok_or(GraphError::NonCanonicalData)?);
            check_domain(&domain)?;
            let height = bounded_height(unsigned(fields.get(3))?)?;
            let entries =
                array_fields(fields.get(4).cloned().ok_or(GraphError::NonCanonicalData)?)?
                    .into_iter()
                    .map(|value| {
                        let fields = array_fields(value)?;
                        if fields.len() != 3 {
                            return Err(GraphError::NonCanonicalData);
                        }
                        let row_count = unsigned(fields.get(2))?;
                        check_length(row_count)?;
                        Ok(OrderedBranchEntry {
                            inclusive_max_key: canonical_value_bytes(
                                fields.first().ok_or(GraphError::NonCanonicalData)?,
                            ),
                            child: oid(bytes_field(fields.get(1))?, algorithm)?,
                            row_count,
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
            Ok(NodeData::OrderedBranch {
                domain,
                height,
                entries,
            })
        }
        NodeKind::ValueOverflow => Ok(NodeData::ValueOverflow {
            encoded_length: checked_length(unsigned(fields.get(2))?)?,
            semantic_digest: digest(bytes_field(fields.get(3))?)?,
            byte_root: oid(bytes_field(fields.get(4))?, algorithm)?,
            dependency_root: match fields.get(5).ok_or(GraphError::NonCanonicalData)? {
                CborValue::Null => None,
                CborValue::Bytes(bytes) => Some(oid(bytes, algorithm)?),
                _ => return Err(GraphError::NonCanonicalData),
            },
        }),
        NodeKind::ByteIndex => {
            let height = bounded_height(unsigned(fields.get(2))?)?;
            let total_length = checked_length(unsigned(fields.get(3))?)?;
            let entries = decode_byte_entries(fields.get(4), algorithm)?;
            Ok(NodeData::ByteIndex {
                height,
                total_length,
                entries,
            })
        }
        NodeKind::BlobDescriptor => {
            let length = checked_length(unsigned(fields.get(2))?)?;
            let raw_sha256 = digest(bytes_field(fields.get(3))?)?;
            let byte_root = match fields.get(4).ok_or(GraphError::NonCanonicalData)? {
                CborValue::Null => None,
                CborValue::Bytes(bytes) => Some(oid(bytes, algorithm)?),
                _ => return Err(GraphError::NonCanonicalData),
            };
            if (length == 0) != byte_root.is_none() {
                return Err(GraphError::InvalidEmptyBlob);
            }
            Ok(NodeData::BlobDescriptor {
                length,
                raw_sha256,
                byte_root,
            })
        }
        NodeKind::Schema => Ok(NodeData::Schema {
            encoded_length: checked_length(unsigned(fields.get(2))?)?,
            schema_digest: digest(bytes_field(fields.get(3))?)?,
            byte_root: oid(bytes_field(fields.get(4))?, algorithm)?,
        }),
        NodeKind::DependencyIndex => {
            let height = bounded_height(unsigned(fields.get(2))?)?;
            let children =
                array_fields(fields.get(3).cloned().ok_or(GraphError::NonCanonicalData)?)?
                    .into_iter()
                    .map(|value| oid(bytes_value(&value)?, algorithm))
                    .collect::<Result<Vec<_>, _>>()?;
            Ok(NodeData::DependencyIndex { height, children })
        }
    }
}

fn decode_byte_entries(
    value: Option<&CborValue>,
    algorithm: GitHashAlgorithm,
) -> Result<Vec<ByteIndexEntry>, GraphError> {
    let entries = array_fields(value.cloned().ok_or(GraphError::NonCanonicalData)?)?
        .into_iter()
        .map(|value| {
            let fields = array_fields(value)?;
            if fields.len() != 3 {
                return Err(GraphError::NonCanonicalData);
            }
            let chunk_sha256 = match fields.get(2).ok_or(GraphError::NonCanonicalData)? {
                CborValue::Null => None,
                CborValue::Bytes(bytes) => Some(digest(bytes)?),
                _ => return Err(GraphError::NonCanonicalData),
            };
            Ok(ByteIndexEntry {
                span_length: unsigned(fields.first())?,
                target: oid(bytes_field(fields.get(1))?, algorithm)?,
                chunk_sha256,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(entries)
}

fn array_fields(value: CborValue) -> Result<Vec<CborValue>, GraphError> {
    match value {
        CborValue::Array(values) => Ok(values),
        _ => Err(GraphError::NonCanonicalData),
    }
}

fn unsigned(value: Option<&CborValue>) -> Result<u64, GraphError> {
    match value.ok_or(GraphError::NonCanonicalData)? {
        CborValue::Unsigned(value) => Ok(*value),
        _ => Err(GraphError::NonCanonicalData),
    }
}

fn bytes_value(value: &CborValue) -> Result<&[u8], GraphError> {
    match value {
        CborValue::Bytes(value) => Ok(value),
        _ => Err(GraphError::NonCanonicalData),
    }
}

fn bytes_field(value: Option<&CborValue>) -> Result<&[u8], GraphError> {
    bytes_value(value.ok_or(GraphError::NonCanonicalData)?)
}

fn digest(value: &[u8]) -> Result<[u8; 32], GraphError> {
    value
        .try_into()
        .map_err(|_| GraphError::InvalidDigestLength)
}

fn oid(value: &[u8], algorithm: GitHashAlgorithm) -> Result<NativeObjectId, GraphError> {
    NativeObjectId::new(algorithm, value)
}

fn bounded_height(value: u64) -> Result<u8, GraphError> {
    let height = u8::try_from(value).map_err(|_| GraphError::HeightExceeded(u8::MAX))?;
    check_height(height)?;
    Ok(height)
}

fn checked_length(value: u64) -> Result<u64, GraphError> {
    check_length(value)?;
    Ok(value)
}

fn array(output: &mut Vec<u8>, length: u64) {
    head(output, 4, length);
}

fn bytes(output: &mut Vec<u8>, value: &[u8]) {
    head(output, 2, value.len() as u64);
    output.extend_from_slice(value);
}

fn uint(output: &mut Vec<u8>, value: u64) {
    head(output, 0, value);
}

fn null(output: &mut Vec<u8>) {
    output.push(0xf6);
}

fn head(output: &mut Vec<u8>, major: u8, value: u64) {
    let base = major << 5;
    match value {
        0..=23 => output.push(base | value as u8),
        24..=255 => {
            output.push(base | 24);
            output.push(value as u8);
        }
        256..=65_535 => {
            output.push(base | 25);
            output.extend_from_slice(&(value as u16).to_be_bytes());
        }
        65_536..=4_294_967_295 => {
            output.push(base | 26);
            output.extend_from_slice(&(value as u32).to_be_bytes());
        }
        _ => {
            output.push(base | 27);
            output.extend_from_slice(&value.to_be_bytes());
        }
    }
}

#[cfg(test)]
mod option_value_tests {
    use super::*;

    fn rov3_blob(descriptor: Vec<u8>) -> CborValue {
        CborValue::Tag(
            60111,
            Box::new(CborValue::Array(vec![
                CborValue::Unsigned(7),
                CborValue::Bytes(vec![0x22; 32]),
                CborValue::Text("audio/mpeg".into()),
                CborValue::Null,
                CborValue::Bytes(descriptor),
            ])),
        )
    }

    fn option_none() -> CborValue {
        CborValue::Tag(60013, Box::new(CborValue::Array(vec![CborValue::Unsigned(0)])))
    }

    #[test]
    fn optional_blob_metadata_traversal_handles_none_and_some() {
        let mut metadata = Vec::new();
        collect_blob_value_fields(&option_none(), &mut metadata).unwrap();
        assert!(metadata.is_empty());

        let blob_metadata = CborValue::Tag(
            60112,
            Box::new(CborValue::Array(vec![
                CborValue::Unsigned(7),
                CborValue::Bytes(vec![0x22; 32]),
                CborValue::Text("audio/mpeg".into()),
                CborValue::Null,
            ])),
        );
        let option_some = CborValue::Tag(
            60013,
            Box::new(CborValue::Array(vec![CborValue::Unsigned(1), blob_metadata])),
        );
        collect_blob_value_fields(&option_some, &mut metadata).unwrap();
        assert_eq!(metadata.len(), 1);
    }

    /// ORNA-MIME-002/-004 (A08-09): every annotation MIME-1 admits when a row is
    /// written stays readable from the stored row, including a quoted
    /// parameter, an `_` hint and a hint on octet-stream.
    #[test]
    fn stored_blob_metadata_keeps_every_canonical_mime1_annotation() {
        for (media_type, suffix) in [
            ("text/plain;title=\"a b\"", None),
            ("application/x-example", Some("my_ext")),
            ("application/octet-stream", Some("dat")),
        ] {
            let stored = CborValue::Tag(
                60112,
                Box::new(CborValue::Array(vec![
                    CborValue::Unsigned(7),
                    CborValue::Bytes(vec![0x22; 32]),
                    CborValue::Text(media_type.into()),
                    suffix.map_or(CborValue::Null, |suffix| CborValue::Text(suffix.into())),
                ])),
            );
            let mut metadata = Vec::new();
            collect_blob_value_fields(&stored, &mut metadata).unwrap();
            let [(annotation, identity)] = metadata.as_slice() else {
                panic!("one Blob value yields one annotation");
            };
            assert_eq!(annotation.media_type(), media_type);
            assert_eq!(annotation.suffix(), suffix);
            assert_eq!(identity.length(), 7);
        }
    }

    #[test]
    fn optional_blob_row_dependencies_handle_none_and_some() {
        let mut none = Vec::new();
        CborValue::Array(vec![option_none()]).encode(&mut none);
        assert!(row_fields_dependencies(&none).unwrap().is_empty());

        let mut some = Vec::new();
        CborValue::Array(vec![CborValue::Tag(
            60013,
            Box::new(CborValue::Array(vec![CborValue::Unsigned(1), rov3_blob(vec![0x44; 20])])),
        )])
        .encode(&mut some);
        assert_eq!(row_fields_dependencies(&some).unwrap().len(), 1);
    }
    #[test]
    fn direct_blob_reference_count_traverses_option_some_values() {
        assert_eq!(direct_blob_reference_count(&option_none()).unwrap(), 0);
        let references = (0..33)
            .map(|_| {
                CborValue::Tag(
                    60013,
                    Box::new(CborValue::Array(vec![CborValue::Unsigned(1), rov3_blob(vec![0x44; 20])])),
                )
            })
            .collect();
        assert_eq!(direct_blob_reference_count(&CborValue::Array(references)).unwrap(), 33);
    }
}
