//! Format-3 native Git graph validation.
//!
//! This module is deliberately a seam for the repository context owner.  The
//! public values describe native objects and decoded node data, but admission
//! is `pub(crate)` and requires an owner-issued [`ValidatedRepositoryContext`].
//! In particular, a raw OID or a caller-provided resolver is not a repository
//! authority.

use std::{
    collections::{BTreeMap, BTreeSet},
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
    AvailabilityState, BlobMetadata, BlobResolver, ContentReference, Error as ValueError,
    Format3Context, Format3Quota, GitHash, MediaAnnotation, MimeRegistry,
    NativeOid as ValueNativeOid, OwnerLifetime, Raw, SchemaDescriptor,
    Snapshot as ValueSnapshot,
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
    snapshot_id: [u8; 32],
    algorithm: GitHashAlgorithm,
    store_root: NativeOid,
    schema_digest: [u8; 32],
    row_snapshot: crate::row_store::RowMapSnapshot,
    cancelled: Arc<AtomicBool>,
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
}

/// Shared owner-issued graph and read-scope authority for one Blob capture
/// pipeline. Clones share the graph owner and the scope's cumulative quotas.
#[derive(Clone, Debug)]
pub struct RepositoryCaptureCapability {
    graph: Arc<NativeGraphContext>,
    scope: RepositoryReadScope,
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
        Ok(Self { graph, scope })
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
            snapshot_id,
            algorithm,
            store_root,
            schema_digest,
            row_snapshot,
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        context.validate_sealed_schema_node()?;
        Ok(context)
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
        let mut digest = crate::blob_store::ContentDigest::new();
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
            find_ordered_entry_with(&mut read_node, root_node, &domain, None, None, key)?
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
                None,
                None,
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
        let actual_schema_digest: [u8; 32] = Sha256::digest(&schema_bytes).into();
        if actual_schema_digest != schema_digest {
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
    /// Applies ordered mutations to the pinned primary ORP-1 root and returns
    /// the new root together with the object accounting of the pass.
    ///
    /// Mutations apply in the canonical order: deletes, then rekeys (a delete
    /// of the old key plus an insert of the new one), then inserts. Each
    /// mutation is checked against the pinned root, so a delete of an absent
    /// key or an insert of a present one is rejected instead of silently
    /// changing the row set.
    ///
    /// The pass rebuilds the canonical complete ORP-1 pages for the domain.
    /// The boundary rule is position-independent, so every page the mutation
    /// did not touch keeps its previous object id and is reported as an index
    /// object that was reused: `index_objects_created` counts the new index
    /// nodes (the measured index churn) while `payload_objects_reused` counts
    /// the stored value and Blob-descriptor objects the rewrite kept, which no
    /// maintenance pass rewrites. The new root stays unreachable until the
    /// caller names it from a row-map version or publication candidate.

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

        let (previous_nodes, previous_payload, mut rows) = {
            let mut objects = ObjectBudget::new(
                scope.max_objects.min(MAX_FULL_VERIFY_OBJECTS),
                scope.max_objects,
                Arc::clone(&scope.objects_used),
                scope.metadata_quota.min(MAX_FULL_METADATA_BYTES),
                Arc::clone(&scope.metadata_used),
            );
            let mut read_node = |oid: &NativeOid| self.read_native_node(oid, scope, &mut objects);
            let root_node = read_node(&root)?;
            validate_ordered_root_node(&root_node, &domain, expected_count)?;
            let mut nodes = BTreeSet::new();
            nodes.insert(root.clone());
            let mut payload = BTreeSet::new();
            let mut validated_contexts = BTreeSet::new();
            collect_ordered_index_objects_with(
                &mut read_node,
                &root_node,
                &root,
                &domain,
                None,
                &mut nodes,
                &mut payload,
                &mut validated_contexts,
            )?;
            let limit = usize::try_from(expected_count).unwrap_or(usize::MAX);
            let mut raw = Vec::new();
            collect_ordered_range_with(
                &mut read_node,
                root_node,
                &domain,
                None,
                None,
                None,
                None,
                limit,
                &mut raw,
            )?;
            let mut rows = Vec::with_capacity(raw.len());
            for entry in raw {
                let key = crate::row_store::TypedKey::decode_canonical(&entry.key)
                    .map_err(|_| GraphError::NonCanonicalData)?;
                let value = self.decode_persisted_row_value(entry.value, scope, &mut objects)?;
                rows.push((key, value));
            }
            (nodes, payload, rows)
        };

        let logical_rekeys = apply_row_index_mutations(&mut rows, mutations)?;
        if rows.len() as u64 > MAX_SIGNED_LENGTH {
            return Err(GraphError::InvalidCount(rows.len()));
        }
        let row_count = rows.len() as u64;

        let entries: Vec<crate::row_store::RowEntry> = rows
            .into_iter()
            .map(|(key, value)| crate::row_store::RowEntry::new(key, value))
            .collect();
        let leaf_pages = crate::row_store::partition_pages(&entries, 0)
            .map_err(|_| GraphError::NonCanonicalData)?;

        let mut written = BTreeSet::new();
        // Envelope object ids only. A data blob carries the same value bytes
        // whenever a page is rebuilt with unchanged stored values, so counting
        // it as churn would report media as an index object.
        let mut written_nodes = BTreeSet::new();
        let mut new_payload = BTreeSet::new();
        let mut level: Vec<BranchCandidate> = Vec::new();
        if leaf_pages.is_empty() {
            // The empty table is one empty leaf, not zero pages.
            let node = NodeData::OrderedLeaf {
                domain: domain.clone(),
                entries: Vec::new(),
            };
            let oid = self.write_capture_node(&node, &mut written, scope.max_objects)?;
            written_nodes.insert(oid.clone());
            level.push(BranchCandidate {
                max_key: Vec::new(),
                oid,
                row_count: 0,
            });
        }
        for page in &leaf_pages {
            let mut page_entries = Vec::with_capacity(page.entries().len());
            for entry in page.entries() {
                page_entries.push(OrderedLeafEntry {
                    key: entry
                        .key()
                        .canonical_bytes()
                        .map_err(|_| GraphError::NonCanonicalData)?,
                    value: entry
                        .value()
                        .canonical_page_bytes()
                        .map_err(|_| GraphError::NonCanonicalData)?,
                });
            }
            let node = NodeData::OrderedLeaf {
                domain: domain.clone(),
                entries: page_entries,
            };
            for dependency in node.dependencies()? {
                new_payload.insert(dependency.oid);
            }
            let NodeData::OrderedLeaf { entries, .. } = &node else {
                unreachable!("constructed ordered leaf")
            };
            // The fence is the canonical encoding of the page's inclusive
            // maximum key. `entry.key` is already that encoding, so it is kept
            // as bytes: decoding it to a typed key and re-encoding it would be
            // the same bytes, and the branch encoder and every fence comparison
            // consume the canonical spelling rather than a typed key (ORP-1).
            let max_key = entries.last().map_or(Vec::new(), |entry| entry.key.clone());
            let oid = self.write_capture_node(&node, &mut written, scope.max_objects)?;
            written_nodes.insert(oid.clone());
            level.push(BranchCandidate {
                max_key,
                oid,
                row_count: entries.len() as u64,
            });
        }

        let mut height = 1u8;
        while level.len() > 1 {
            if height > MAX_GRAPH_HEIGHT {
                return Err(GraphError::HeightExceeded(height));
            }
            let groups = partition_branch_candidates(&domain, height, &level)?;
            if groups.len() >= level.len() {
                return Err(GraphError::FanoutExceeded(level.len()));
            }
            let mut next = Vec::with_capacity(groups.len());
            for group in groups {
                let node = NodeData::OrderedBranch {
                    domain: domain.clone(),
                    height,
                    entries: group
                        .iter()
                        .map(|candidate| OrderedBranchEntry {
                            inclusive_max_key: candidate.max_key.clone(),
                            child: candidate.oid.clone(),
                            row_count: candidate.row_count,
                        })
                        .collect(),
                };
                let oid = self.write_capture_node(&node, &mut written, scope.max_objects)?;
                written_nodes.insert(oid.clone());
                next.push(BranchCandidate {
                    max_key: group
                        .last()
                        .ok_or(GraphError::InvalidCount(0))?
                        .max_key
                        .clone(),
                    oid,
                    row_count: group.iter().map(|candidate| candidate.row_count).sum(),
                });
            }
            level = next;
            height += 1;
        }
        let primary_root = level
            .first()
            .ok_or(GraphError::InvalidCount(0))?
            .oid
            .clone();
        self.sync_object_closure(&written, scope)?;

        let index_objects_created: Vec<NativeOid> = written_nodes
            .iter()
            .filter(|oid| !previous_nodes.contains(*oid))
            .cloned()
            .collect();
        let index_objects_reused: Vec<NativeOid> = written_nodes
            .iter()
            .filter(|oid| previous_nodes.contains(*oid))
            .cloned()
            .collect();
        let payload_objects_reused: Vec<NativeOid> = previous_payload
            .iter()
            .filter(|oid| new_payload.contains(*oid))
            .cloned()
            .collect();
        Ok(IndexMaintenance {
            previous_root: root,
            primary_root,
            row_count,
            logical_rekeys,
            objects_written: written.into_iter().collect(),
            index_objects_created,
            index_objects_reused,
            payload_objects_reused,
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


    /// Projects the payload-free Blob metadata of one admitted row field.
    ///
    /// This is the measured metadata-only read path a query over media rows
    /// takes. The row itself is the only object the lookup reads, and its own
    /// field tuple carries the length, SHA-256 and MIME-1 annotation of the
    /// Blob reference, so the projection returns without entering a descriptor
    /// or content read. `scope.payload_bytes_read()` therefore stays at zero
    /// for the whole projection, while `scope.objects_read()` still counts the
    /// row pages the lookup genuinely walked.
    ///
    /// `field` is the index of the Blob field in the stored tuple; a tuple
    /// without that field yields `None`. A row whose fields overflowed to the
    /// shared graph has no local tuple and is rejected, because reporting the
    /// bounded overflow metadata as the row's fields would be a lie.
    pub fn project_blob_metadata(
        &self,
        key: &crate::row_store::TypedKey,
        field: usize,
        scope: &RepositoryReadScope,
    ) -> Result<Option<(crate::row_store::TypedKey, BlobMetadata)>, GraphError> {
        let Some(row) = self.lookup_row(key, scope)? else {
            return Ok(None);
        };
        let metadata = row
            .blob_metadata(field)
            .map_err(|_| GraphError::NonCanonicalData)?
            .ok_or(GraphError::NonCanonicalData)?;
        Ok(Some((row.key().clone(), metadata)))
    }

    /// Projects the payload-free Blob metadata of a bounded row interval.
    ///
    /// Rows are returned in canonical key order, exactly as
    /// [`Self::range_rows`] orders them, but only the interval's own field
    /// tuples are decoded: no descriptor, byte index or payload chunk is read.
    /// A field that is absent from a row's tuple is skipped rather than
    /// fabricated, and an overflowed row is rejected by the same rule as
    /// [`Self::project_blob_metadata`].
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
            let Some(metadata) = row
                .blob_metadata(field)
                .map_err(|_| GraphError::NonCanonicalData)?
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
        let canonical_fields = match row.value() {
            crate::row_store::RowValue::Inline { encoded, .. } => {
                scope.charge_structured_metadata_read(encoded.len() as u64)?;
                encoded.clone()
            }
            crate::row_store::RowValue::Overflow(reference) => {
                let root = reference
                    .native_root()
                    .ok_or(GraphError::UnsupportedRowValueForm)?;
                let mut objects = ObjectBudget::new(
                    scope.max_objects.min(MAX_FULL_VERIFY_OBJECTS),
                    scope.max_objects,
                    Arc::clone(&scope.objects_used),
                    scope.metadata_quota.min(MAX_FULL_METADATA_BYTES),
                    Arc::clone(&scope.metadata_used),
                );
                self.verify_value_overflow(root, scope, &mut objects, &mut BTreeSet::new(), true)?
                    .encoded_value
                    .ok_or(GraphError::ContextMismatch)?
            }
        };

        binding
            .descriptor()
            .validate_stored_row(*version.relation_id(), &canonical_key, &canonical_fields)
            .map_err(|_| GraphError::NonCanonicalData)?;
        let CborValue::Array(values) = decode_canonical_cbor(&canonical_fields)? else {
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
                && direct_blob_reference_count(&rov_value)? <= 32
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
                        let value = self.to_sov_value(value, scope, objects, active)?;
                        fields.push(value);
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
                    || !matches!(fields.get(4), Some(CborValue::Bytes(oid)) if oid.len() == self.algorithm.width())
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
            CborValue::Tag(60113, payload) => {
                let oid = overflow_tag_oid(&payload, self.algorithm)?;
                Ok(self
                    .verify_value_overflow(&oid, scope, objects, active, false)?
                    .semantic_value)
            }
            CborValue::Array(values) => Ok(CborValue::Array(
                values
                    .into_iter()
                    .map(|value| self.to_sov_value(value, scope, objects, active))
                    .collect::<Result<Vec<_>, _>>()?,
            )),
            CborValue::Map(entries) => {
                let mut transformed = entries
                    .into_iter()
                    .map(|(key, value)| {
                        Ok((
                            self.to_sov_value(key, scope, objects, active)?,
                            self.to_sov_value(value, scope, objects, active)?,
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
            CborValue::Tag(_, _) => Err(GraphError::NonCanonicalData),
            scalar => Ok(scalar),
        }
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
            find_ordered_entry_with(
                &mut read_node,
                relation_node.clone(),
                &domain,
                None,
                None,
                &key,
            )?
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
            let value = crate::row_store::RowValue::decode_canonical_fields(encoded_row)
                .map_err(|_| GraphError::NonCanonicalData)?;
            let candidate = self.maintain_relation_row_candidate(
                version,
                &[RowIndexMutation::Insert {
                    key: key.clone(),
                    value,
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
        let entry = find_ordered_entry_with(&mut read_node, node, &domain, None, None, key)?;
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
        let actual_kind = self.git_object_kind(oid)?;
        if actual_kind != kind {
            return Err(GraphError::WrongObjectKind {
                expected: kind,
                actual: actual_kind,
            });
        }
        let hex = oid.to_hex();
        let size_output = self.git_output(&["cat-file", "-s", &hex])?;
        let size = std::str::from_utf8(&size_output)
            .map_err(|_| GraphError::GitObjectMalformed)?
            .trim()
            .parse::<u64>()
            .map_err(|_| GraphError::GitObjectMalformed)?;
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
        let kind_name = match kind {
            NativeObjectKind::Tree => "tree",
            NativeObjectKind::Blob => "blob",
        };
        let mut child = self
            .git_command()
            .args(["cat-file", kind_name, &hex])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| GraphError::GitCommandFailed)?;
        let mut data = Vec::with_capacity(size as usize);
        let mut stdout = child.stdout.take().ok_or(GraphError::GitCommandFailed)?;
        let read_result = stdout
            .by_ref()
            .take(maximum.saturating_add(1))
            .read_to_end(&mut data);
        drop(stdout);
        read_result.map_err(|_| GraphError::GitCommandFailed)?;
        if data.len() as u64 > maximum {
            let _ = child.kill();
            let _ = child.wait();
            return Err(
                if kind == NativeObjectKind::Blob
                    && maximum == crate::blob_store::GEAR_MAXIMUM as u64
                {
                    GraphError::ChunkTooLarge(data.len() as u64)
                } else {
                    GraphError::NodeDataTooLarge(data.len())
                },
            );
        }
        let output = child
            .wait_with_output()
            .map_err(|_| GraphError::GitCommandFailed)?;
        if !output.status.success() {
            return Err(absent_object_error(&self.repository, &hex));
        }
        if data.len() as u64 != size {
            return Err(GraphError::GitObjectMalformed);
        }
        let hashed = self.git_hash_object(kind_name, &data)?;
        scope.check_cancelled()?;
        if hashed != *oid {
            return Err(GraphError::GitObjectHashMismatch);
        }
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

    /// The single object id a `cat-file` invocation asks Git about, when the
    /// call is a `cat-file` one that names exactly one object. `-t`, `-s` and
    /// the kind readers (`blob`, `tree`, ...) all take the object id last, so
    /// the final argument is it; the batch forms read object ids from stdin and
    /// name none here.
    fn git_hash_object(&self, kind: &str, bytes: &[u8]) -> Result<NativeOid, GraphError> {
        let mut child = self
            .git_command()
            .args(["hash-object", "-t", kind, "--stdin"])
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
        NativeOid::from_hex(self.algorithm, hex)
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

struct CaptureIndexBuilder<'a> {
    graph: &'a NativeGraphContext,
    written: &'a mut BTreeSet<NativeOid>,
    object_limit: u64,
    scope: &'a RepositoryReadScope,
    levels: Vec<Vec<ByteIndexEntry>>,
}

impl<'a> CaptureIndexBuilder<'a> {
    fn new(
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

    fn push_chunk(
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

    fn finish(mut self) -> Result<Option<NativeOid>, GraphError> {
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

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
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
/// loose, then the object directory. A non-empty `objects/info/alternates` is
/// refused rather than half-synced, because those objects may live in
/// another repository. `before_each` runs ahead of every object.
pub(crate) fn sync_object_files<'a>(
    objects_dir: &Path,
    objects: impl IntoIterator<Item = &'a NativeOid>,
    mut before_each: impl FnMut() -> Result<(), GraphError>,
) -> Result<(), GraphError> {
    let pack_dir = objects_dir.join("pack");
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
        sync_all_pack_files(&pack_dir)?;
    }
    sync_directory(objects_dir)
}

/// Persists `reference` after a ref transaction wrote it: the loose ref
/// file, each directory from it up to the common Git directory, and
/// `packed-refs`.
pub(crate) fn sync_git_ref_durably(
    repository: &crate::Repository,
    reference: &str,
) -> Result<(), GraphError> {
    PrivateRefCleanupOwner {
        repository: repository.clone(),
    }
    .sync_ref_file(reference)
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
    /// Inserts one logical key. The key must be absent from the pinned root.
    Insert {
        key: crate::row_store::TypedKey,
        value: crate::row_store::RowValue,
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
    /// The key must be absent from the pinned row map when the mutation is applied.
    pub fn insert_canonical(key: &[u8], value: &[u8]) -> Result<Self, GraphError> {
        let key = crate::row_store::TypedKey::decode_canonical(key)
            .map_err(|_| GraphError::NonCanonicalData)?;
        let value = crate::row_store::RowValue::decode_canonical_fields(value.to_vec())
            .map_err(|_| GraphError::NonCanonicalData)?;
        Ok(Self::Insert { key, value })
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
    annotation: crate::blob_store::BlobAnnotation,
    reference: AdmittedBlobReference,
}

impl AdmittedStoredBlob {
    /// The canonical declared metadata stored beside this reference.
    pub fn annotation(&self) -> &crate::blob_store::BlobAnnotation {
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
                check_branch_fence_order(domain, entries)?;
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
                if children
                    .windows(2)
                    .any(|pair| pair[0].as_bytes() >= pair[1].as_bytes())
                {
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
    let schema_bytes = reader.read_full_byte_index(&byte_root, encoded_length, schema_digest)?;
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
                if domain != expected_domain || entries.len() as u64 != expected_count {
                    return Err(GraphError::InvalidCount(entries.len()));
                }
            }
            NodeData::OrderedBranch {
                domain, entries, ..
            } => {
                if domain != expected_domain || entries.len() == 1 {
                    return Err(GraphError::InvalidDomain);
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
        find_ordered_entry_with(&mut read_node, node, expected_domain, None, None, key)
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
        let hex = oid.to_hex();
        let kind_output = self.git_output(&["cat-file", "-t", &hex])?;
        let actual_kind = match kind_output.as_slice() {
            b"tree\n" => NativeObjectKind::Tree,
            b"blob\n" => NativeObjectKind::Blob,
            _ => return Err(GraphError::UnsupportedGitObjectType),
        };
        if actual_kind != kind {
            return Err(GraphError::WrongObjectKind {
                expected: kind,
                actual: actual_kind,
            });
        }
        let size_output = self.git_output(&["cat-file", "-s", &hex])?;
        let size = std::str::from_utf8(&size_output)
            .map_err(|_| GraphError::GitObjectMalformed)?
            .trim()
            .parse::<u64>()
            .map_err(|_| GraphError::GitObjectMalformed)?;
        if size > maximum {
            return Err(GraphError::NodeDataTooLarge(
                usize::try_from(size).unwrap_or(usize::MAX),
            ));
        }
        if self.seen.insert(oid.clone()) {
            if self.seen.len() as u64 > MAX_FULL_VERIFY_OBJECTS {
                return Err(GraphError::InventoryQuotaExceeded);
            }
            self.metadata_bytes = self
                .metadata_bytes
                .checked_add(size)
                .ok_or(GraphError::MetadataQuotaExceeded)?;
            if self.metadata_bytes > MAX_FULL_METADATA_BYTES {
                return Err(GraphError::MetadataQuotaExceeded);
            }
        }
        let type_name = match kind {
            NativeObjectKind::Tree => "tree",
            NativeObjectKind::Blob => "blob",
        };
        let output = self.git_output(&["cat-file", type_name, &hex])?;
        if output.len() as u64 != size {
            return Err(GraphError::GitObjectMalformed);
        }
        let hash = self.hash_object(type_name, &output)?;
        if hash != *oid {
            return Err(GraphError::GitObjectHashMismatch);
        }
        Ok(output)
    }


    fn read_full_byte_index(
        &mut self,
        root: &NativeObjectId,
        expected_length: u64,
        expected_digest: [u8; 32],
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
        let (length, _) = self.walk_byte_index(
            root,
            None,
            &mut hasher,
            Some(&mut output),
            Some(expected_length),
        )?;
        let digest: [u8; 32] = hasher.finalize().into();
        if length != expected_length || digest != expected_digest {
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

    fn hash_object(&self, kind: &str, data: &[u8]) -> Result<NativeObjectId, GraphError> {
        let mut child = self
            .git_command()
            .args(["hash-object", "-t", kind, "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| GraphError::GitCommandFailed)?;
        child
            .stdin
            .take()
            .ok_or(GraphError::GitCommandFailed)?
            .write_all(data)
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
        NativeObjectId::from_hex(self.algorithm, hex)
    }

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
        command
    }
}

fn find_ordered_entry_with(
    read_node: &mut impl FnMut(&NativeOid) -> Result<NodeData, GraphError>,
    node: NodeData,
    expected_domain: &[u8],
    expected_height: Option<u8>,
    lower_exclusive: Option<&crate::row_store::TypedKey>,
    key: &crate::row_store::TypedKey,
) -> Result<Option<OrderedLeafEntry>, GraphError> {
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
            if lower_exclusive
                .is_some_and(|lower| decoded.first().is_some_and(|(minimum, _)| minimum <= lower))
            {
                return Err(GraphError::NonCanonicalData);
            }
            Ok(decoded
                .binary_search_by(|(candidate, _)| candidate.cmp(key))
                .ok()
                .map(|index| decoded[index].1.clone()))
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
            // The caller proved this node's minimum sorts above the lower
            // exclusive bound when it descended here, so a point lookup does
            // not recompute whole-branch bounds: it validates each child it
            // probes and stops at the first child that answers. Recomputing
            // them would read the first and last descendant of every level,
            // which is what made a lookup on a deep index cost the whole
            // index rather than the path to the key.
            let mut previous_fence = None;
            for entry in &entries {
                let fence = crate::row_store::TypedKey::decode_canonical(&entry.inclusive_max_key)
                    .map_err(|_| GraphError::NonCanonicalData)?;
                if previous_fence
                    .as_ref()
                    .is_some_and(|previous| previous >= &fence)
                {
                    return Err(GraphError::NonCanonicalData);
                }
                // A fence is the child's own inclusive maximum key, so a child
                // whose fence sorts below the search key holds no key at or
                // above it and is left unread: a point lookup reads the path to
                // the key plus the pages it probes, not the whole index. The
                // probe is not cut short, so a miss in the child that covers
                // the key continues into later children while their fences
                // still reach it, and every child the probe descends into is
                // validated before it is used. ORP-1 fixes the child ranges as
                // nonoverlapping, so a fence that lies about its child is a
                // malformed node, and probing the children the key reaches is
                // what detects it.
                if key <= &fence {
                    // A fence is only a pruning hint after its child proves
                    // it: validate the child this search descends into, and
                    // reject a range that overlaps the previous child.
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
                    if let Some(found) = find_ordered_entry_with(
                        read_node,
                        child,
                        expected_domain,
                        Some(height - 1),
                        previous_fence.as_ref(),
                        key,
                    )? {
                        return Ok(Some(found));
                    }
                }
                previous_fence = Some(fence);
            }
            Ok(None)
        }
        other => Err(GraphError::WrongNodeKind {
            expected: NodeKind::OrderedLeaf,
            actual: other.kind(),
        }),
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

/// One child of a branch level under construction: the exact fence and count
/// its parent entry will name, plus the object its envelope was written as.
struct BranchCandidate {
    max_key: Vec<u8>,
    oid: NativeOid,
    row_count: u64,
}

/// Applies one maintenance pass in the canonical order: deletes, then rekeys,
/// then inserts. Every mutation is checked against the row set the pinned root
/// actually held, so an absent delete/rekey source or a colliding insert is
/// rejected rather than silently changing which rows exist.
fn apply_row_index_mutations(
    rows: &mut Vec<(crate::row_store::TypedKey, crate::row_store::RowValue)>,
    mutations: &[RowIndexMutation],
) -> Result<Vec<LogicalRowRekey>, GraphError> {
    let mut logical_rekeys = Vec::new();
    for mutation in mutations {
        let RowIndexMutation::Delete { key } = mutation else {
            continue;
        };
        let index = locate_row(rows, key)?.ok_or(GraphError::RowKeyAbsent)?;
        rows.remove(index);
    }
    for mutation in mutations {
        let RowIndexMutation::Rekey { from, to } = mutation else {
            continue;
        };
        let index = locate_row(rows, from)?.ok_or(GraphError::RowKeyAbsent)?;
        if locate_row(rows, to)?.is_some() {
            return Err(GraphError::DuplicateRowKey);
        }
        let (_, value) = rows.remove(index);
        let insertion = locate_insertion(rows, to);
        rows.insert(insertion, (to.clone(), value));
        logical_rekeys.push(LogicalRowRekey {
            old_address: from.clone(),
            new_address: to.clone(),
        });
    }
    for mutation in mutations {
        let RowIndexMutation::Insert { key, value } = mutation else {
            continue;
        };
        let insertion = locate_insertion(rows, key);
        if rows
            .get(insertion)
            .is_some_and(|(existing, _)| existing == key)
        {
            return Err(GraphError::DuplicateRowKey);
        }
        rows.insert(insertion, (key.clone(), value.clone()));
    }
    Ok(logical_rekeys)
}

fn locate_row(
    rows: &[(crate::row_store::TypedKey, crate::row_store::RowValue)],
    key: &crate::row_store::TypedKey,
) -> Result<Option<usize>, GraphError> {
    Ok(rows
        .binary_search_by(|(existing, _)| existing.cmp(key))
        .ok())
}

fn locate_insertion(
    rows: &[(crate::row_store::TypedKey, crate::row_store::RowValue)],
    key: &crate::row_store::TypedKey,
) -> usize {
    rows.binary_search_by(|(existing, _)| existing.cmp(key))
        .unwrap_or_else(|index| index)
}

/// Groups branch children into canonical ORP-1 branch pages: accumulate until
/// the next entry would exceed the 64 KiB node data bound, 256 entries or 256
/// distinct native refs; close a group at an anchor once it holds at least 16
/// entries, or at exactly 256; and emit the final short tail.
fn partition_branch_candidates<'a>(
    domain: &[u8],
    height: u8,
    candidates: &'a [BranchCandidate],
) -> Result<Vec<&'a [BranchCandidate]>, GraphError> {
    let mut groups = Vec::new();
    let mut start = 0usize;
    let mut size = 16usize + domain.len();
    let mut refs = 0usize;
    for (index, candidate) in candidates.iter().enumerate() {
        let entry_size = branch_entry_encoded_size(candidate)?;
        if entry_size + 16 + domain.len() > NODE_DATA_LIMIT {
            return Err(GraphError::NodeDataTooLarge(entry_size));
        }
        let held = index - start;
        let would_overflow = held != 0
            && (held == crate::row_store::MAX_PAGE_ENTRIES
                || refs + 1 > MAX_REFS
                || size + entry_size > NODE_DATA_LIMIT);
        if would_overflow {
            groups.push(&candidates[start..index]);
            start = index;
            size = 16 + domain.len();
            refs = 0;
        }
        size += entry_size;
        refs += 1;
        let held = index - start + 1;
        let anchor = ordered_token_anchor(height, &candidate.max_key)?;
        if held >= crate::row_store::MIN_PAGE_ENTRIES
            && (anchor || held == crate::row_store::MAX_PAGE_ENTRIES)
        {
            groups.push(&candidates[start..=index]);
            start = index + 1;
            size = 16 + domain.len();
            refs = 0;
        }
    }
    if start < candidates.len() {
        groups.push(&candidates[start..]);
    }
    Ok(groups)
}

/// Conservative encoded size of one `[inclusive_max_key, child_oid, count]`
/// entry: the entry array head is one byte, the child is a byte string header
/// plus the native width, and the count is a shortest definite head.
fn branch_entry_encoded_size(candidate: &BranchCandidate) -> Result<usize, GraphError> {
    let oid = candidate.oid.as_bytes().len();
    Ok(1 + 1 + candidate.max_key.len() + 1 + oid + cbor_unsigned_len(candidate.row_count))
}

const fn cbor_unsigned_len(value: u64) -> usize {
    if value < 24 {
        1
    } else if value <= u8::MAX as u64 {
        2
    } else if value <= u16::MAX as u64 {
        3
    } else if value <= u32::MAX as u64 {
        5
    } else {
        9
    }
}

/// The ORP-1 anchor test over an arbitrary canonical token. A leaf token is the
/// key's own canonical bytes, so this agrees with `row_store::boundary_anchor`;
/// a branch token is the child's inclusive maximum key.
fn ordered_token_anchor(height: u8, token: &[u8]) -> Result<bool, GraphError> {
    let mut hasher = Sha256::new();
    hasher.update(crate::row_store::ORP_DOMAIN);
    hasher.update(u32::from(height).to_be_bytes());
    hasher.update(token);
    let digest = hasher.finalize();
    Ok(digest[31] & ((1 << crate::row_store::ORP_ANCHOR_BITS) - 1) == 0)
}

/// Collects every node object of one ordered index (root, branches and leaves)
/// plus every stored-value payload object its entries reference, so a
/// maintenance pass can separate index churn from media reuse by object id.
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

fn collect_ordered_range_with(
    read_node: &mut impl FnMut(&NativeOid) -> Result<NodeData, GraphError>,
    node: NodeData,
    expected_domain: &[u8],
    expected_height: Option<u8>,
    lower_exclusive: Option<&crate::row_store::TypedKey>,
    lower_inclusive: Option<&crate::row_store::TypedKey>,
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
                let within_lower = lower_inclusive.is_none_or(|lower| fence >= *lower);
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
        let schema_bytes = descriptor.encode().expect("canonical fixture schema bytes");
        let schema_digest: [u8; 32] = Sha256::digest(&schema_bytes).into();
        let chunk = fixture_git_object(directory, "blob", &schema_bytes);
        let byte_root = fixture_git_node(
            directory,
            &NodeData::ByteIndex {
                height: 0,
                total_length: schema_bytes.len() as u64,
                entries: vec![ByteIndexEntry {
                    span_length: schema_bytes.len() as u64,
                    target: chunk,
                    chunk_sha256: Some(schema_digest),
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
        let init = Command::new("git")
            .current_dir(root)
            .args(["init", "--quiet", "--template="])
            .status()
            .expect("initialize capture repository");
        assert!(init.success());

        let algorithm = GitHashAlgorithm::Sha1;
        let database_id = [0x31; 16];
        let relation_id = [0x32; 16];
        // The schema descriptor binds relation identity and its complete field layout
        // (Orna 1.1.0 Section 29).
        let (schema_oid, schema_digest) =
            fixture_schema_node(root, relation_id, include_blob_field);
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
        let schema_digest: [u8; 32] = Sha256::digest(&schema_bytes).into();
        let schema_chunk = fixture_git_object(directory.path(), "blob", &schema_bytes);
        let schema_byte_root = fixture_git_node(
            directory.path(),
            &NodeData::ByteIndex {
                height: 0,
                total_length: schema_bytes.len() as u64,
                entries: vec![ByteIndexEntry {
                    span_length: schema_bytes.len() as u64,
                    target: schema_chunk,
                    chunk_sha256: Some(schema_digest),
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
        let value = RowValue::decode_canonical_fields(vec![0x80])
            .expect("canonical empty row");

        assert!(matches!(
            graph.maintain_relation_rows(
                &substituted,
                &[RowIndexMutation::Insert {
                    key: key.clone(),
                    value: value.clone(),
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
                &[RowIndexMutation::Insert { key, value }],
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
            value: RowValue::decode_canonical_fields(vec![0x80]).expect("canonical empty row"),
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
            value: RowValue::decode_canonical_fields(vec![0x80]).expect("canonical empty row"),
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

    #[test]
    fn publication_transaction_commit_supports_metadata_lookup_without_payload_read() {
        let (directory, mut graph) = capture_test_context();
        let seed_scope = graph.open_read_scope().expect("owner seed scope");
        let seed_value = RowValue::decode_canonical_fields(vec![0x80])
            .expect("empty canonical fixture row");
        let mutations: Vec<_> = (0..300)
            .map(|index| RowIndexMutation::Insert {
                key: TypedKey::Text(format!("row-{index:04}")),
                value: seed_value.clone(),
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
    #[test]
    fn bounded_range_reads_the_first_child_above_its_upper_bound_only() {
        let (_directory, mut graph) = capture_test_context();
        let seed_scope = graph.open_read_scope().expect("owner seed scope");
        let seed_value = RowValue::decode_canonical_fields(vec![0x80])
            .expect("empty canonical fixture row");
        let mutations: Vec<_> = (0..700)
            .map(|index| RowIndexMutation::Insert {
                key: TypedKey::Text(format!("row-{index:04}")),
                value: seed_value.clone(),
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
        drop(seed_scope);

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
        let blob_value = RowValue::decode_canonical_fields(encoded_fields)
            .expect("decode canonical captured Blob value");

        let source_version = graph.row_snapshot.version().clone();
        let source_snapshot_id = [0x39; 32];
        let row_key = TypedKey::Text("captured".to_owned());
        let source_store_root = graph
            .maintain_relation_rows(
                &source_version,
                &[RowIndexMutation::Insert {
                    key: row_key.clone(),
                    value: blob_value.clone(),
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
                    value: blob_value,
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
        let blob_value = RowValue::decode_canonical_fields(blob_row)
            .expect("canonical row with one Blob value");
        let empty_value = RowValue::decode_canonical_fields(vec![0x80])
            .expect("empty canonical fixture row");
        let mutations: Vec<_> = (0..300)
            .map(|index| RowIndexMutation::Insert {
                key: TypedKey::Text(format!("row-{index:04}")),
                value: if index == 150 {
                    blob_value.clone()
                } else {
                    empty_value.clone()
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

        let rewrite_scope = graph.open_read_scope().expect("rewrite scope");
        let rewrite = graph
            .maintain_row_index(
                &[RowIndexMutation::Insert {
                    key: TypedKey::Text("row-0300".to_owned()),
                    value: empty_value,
                }],
                &rewrite_scope,
            )
            .expect("rewrite the index while preserving the Blob row");
        assert_eq!(rewrite.payload_objects_reused().len(), 1);
        assert_eq!(rewrite.row_count(), 301);
        assert_eq!(rewrite_scope.payload_bytes_read(), 0);
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
        let owner_value = RowValue::inline(owner_fields, Vec::new())
            .expect("canonical owner row with a stored table reference");
        let target_value = RowValue::decode_canonical_fields(vec![0x80])
            .expect("canonical empty target row");
        let initial = [
            RowIndexMutation::Insert {
                key: old_key.clone(),
                value: target_value,
            },
            RowIndexMutation::Insert {
                key: owner_key.clone(),
                value: owner_value,
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
    InvalidDigestLength,
    InvalidEmptyBlob,
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
            return Err(GraphError::NonCanonicalData);
        }
        previous = Some(fence);
    }
    Ok(())
}

fn validate_canonical_value(bytes: &[u8]) -> Result<(), GraphError> {
    decode_canonical_cbor(bytes).map(|_| ())
}

fn canonical_value_bytes(value: &CborValue) -> Vec<u8> {
    let mut bytes = Vec::new();
    value.encode(&mut bytes);
    bytes
}

pub(crate) fn decode_canonical_cbor(bytes: &[u8]) -> Result<CborValue, GraphError> {
    decode_canonical_cbor_limited(bytes, NODE_DATA_LIMIT)
}

fn decode_canonical_content(bytes: &[u8]) -> Result<CborValue, GraphError> {
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

fn leaf_value_dependencies(
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
    output: &mut Vec<(
        crate::blob_store::BlobAnnotation,
        crate::blob_store::ContentIdentity,
    )>,
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
            validate_canonical_mime1_annotation(media_type, suffix)?;
            let mut digest_bytes = [0u8; 32];
            digest_bytes.copy_from_slice(digest);
            let identity = crate::blob_store::ContentIdentity::new(*length, digest_bytes)
                .map_err(|_| GraphError::InvalidLength(*length))?;
            let annotation = crate::blob_store::BlobAnnotation::new(media_type.clone(), suffix)
                .map_err(|_| GraphError::InvalidBlobAnnotation)?;
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
        CborValue::Tag(_, _) => Err(GraphError::NonCanonicalData),
        _ => Ok(()),
    }
}

fn rows_domain_for_version(version: &crate::row_store::RowMapVersion) -> Vec<u8> {
    rows_domain(version.relation_id(), version.schema().schema_digest())
}

fn rows_domain_for_parts(relation_id: &[u8; 16], schema_digest: &[u8; 32]) -> Vec<u8> {
    rows_domain(relation_id, schema_digest)
}

fn row_page_anchor(height: u8, key: &crate::row_store::TypedKey) -> Result<bool, GraphError> {
    let canonical = key
        .canonical_bytes()
        .map_err(|_| GraphError::NonCanonicalData)?;
    let mut hasher = Sha256::new();
    hasher.update(crate::row_store::ORP_DOMAIN);
    hasher.update(u32::from(height).to_be_bytes());
    hasher.update(canonical);
    let digest = hasher.finalize();
    Ok(digest[31] & ((1 << crate::row_store::ORP_ANCHOR_BITS) - 1) == 0)
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
        CborValue::Tag(_, _) => return Err(GraphError::NonCanonicalData),
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

fn validate_canonical_mime1_annotation(
    media_type: &str,
    suffix: Option<&str>,
) -> Result<(), GraphError> {
    let annotation =
        MediaAnnotation::new(media_type, suffix).map_err(|_| GraphError::InvalidBlobAnnotation)?;
    if annotation.media_type() != media_type || annotation.suffix() != suffix {
        return Err(GraphError::InvalidBlobAnnotation);
    }
    Ok(())
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

fn collect_row_dependencies(
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
        CborValue::Tag(60110 | 60112, _) => return Err(GraphError::NonCanonicalData),
        CborValue::Tag(_, _) => return Err(GraphError::NonCanonicalData),
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
fn direct_blob_reference_count(value: &CborValue) -> Result<usize, GraphError> {
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
        CborValue::Tag(_, _) => Err(GraphError::NonCanonicalData),
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
            1 if length <= i64::MAX as u64 => Ok(CborValue::Negative(length)),
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
                if !matches!(length, 37 | 60013 | 60021 | 60110..=60113) {
                    return Err(GraphError::NonCanonicalData);
                }
                Ok(CborValue::Tag(length, Box::new(self.value()?)))
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
