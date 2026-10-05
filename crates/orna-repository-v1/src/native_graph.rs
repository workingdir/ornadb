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
pub struct NativeGraphContext {
    repository: crate::Repository,
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
        self.walk_full_index(
            &byte_root,
            encoded_length,
            None,
            &scope,
            &mut objects,
            &mut digest,
        )?;
        let actual = digest.finish();
        if actual.length() != encoded_length || actual.sha256() != schema_digest {
            return Err(GraphError::ContentIdentityMismatch);
        }
        Ok(())
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
        let version = self.row_snapshot.version();
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
                    self.verify_value_overflow(&oid, scope, objects, &mut BTreeSet::new())?;
                crate::row_store::RowValue::overflow(verified.reference)
            }
            CborValue::Array(_) => {
                let mut overflow_oids = BTreeSet::new();
                collect_overflow_oids(&value, self.algorithm, &mut overflow_oids)?;
                for oid in overflow_oids {
                    self.verify_value_overflow(&oid, scope, objects, &mut BTreeSet::new())?;
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
            for dependency in actual_dependencies {
                let dependency_node = self.read_native_node(&dependency, scope, objects)?;
                match dependency_node {
                    NodeData::BlobDescriptor { .. } => {}
                    NodeData::ValueOverflow { .. } => {
                        self.verify_value_overflow(&dependency, scope, objects, active)?;
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
                Ok(CborValue::Tag(
                    60112,
                    Box::new(CborValue::Array(fields.into_iter().take(4).collect())),
                ))
            }
            CborValue::Tag(60113, payload) => {
                let oid = overflow_tag_oid(&payload, self.algorithm)?;
                Ok(self
                    .verify_value_overflow(&oid, scope, objects, active)?
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
                output
                    .try_reserve(chunk.len())
                    .map_err(|_| GraphError::ReadQuotaExceeded)?;
                output.extend_from_slice(&chunk);
                reservation.commit(entry.span_length);
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

    pub fn store_root(&self) -> &NativeOid {
        &self.store_root
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
        scope.authorize(self)?;
        let persisted_row = self.lookup_row(row.key(), scope)?;
        if row.version().database_id() != &self.database_id
            || row.version().store_root() != &self.store_root
            || row.version().schema().schema_digest() != &self.schema_digest
            || descriptor_oid.algorithm() != self.algorithm
            || persisted_row.as_ref() != Some(row)
        {
            return Err(GraphError::ContextMismatch);
        }
        let dependency_present = row.value().dependencies().iter().any(|dependency| {
            dependency.kind() == NativeObjectKind::Tree && dependency.oid() == descriptor_oid
        });
        if !dependency_present {
            return Err(GraphError::DescriptorNotInRow);
        }
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
        if !reference.matches_context(self)
            || range.start > range.end
            || range.end > reference.identity.length()
        {
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
        let pin_hex = hex_encode(&pin_id);
        let protected_ref = format!("refs/orna/pins/{pin_hex}");
        let provisional_ref = format!("refs/orna/pins/pending/{pin_hex}");
        if let Err(error) = self.create_protected_ref(&provisional_ref, &reference.descriptor_oid) {
            let _ = self.delete_protected_ref(&provisional_ref, &reference.descriptor_oid);
            return Err(error);
        }
        let objects = match self.verify_full_blob(reference, scope) {
            Ok(objects) => objects,
            Err(error) => {
                // If cleanup fails, keeping an orphaned private ref is safer
                // than exposing unverified bytes to concurrent collection.
                let _ = self.delete_protected_ref(&provisional_ref, &reference.descriptor_oid);
                return Err(error);
            }
        };
        if let Err(error) = self
            .sync_object_closure(&objects, scope)
            .and_then(|()| scope.check_cancelled())
        {
            let _ = self.delete_protected_ref(&provisional_ref, &reference.descriptor_oid);
            return Err(error);
        }
        if let Err(error) =
            self.promote_protected_ref(&provisional_ref, &protected_ref, &reference.descriptor_oid)
        {
            let _ = self.delete_protected_ref(&provisional_ref, &reference.descriptor_oid);
            return Err(error);
        }
        // If syncing the promoted ref fails, leave it rooted. Returning no pin
        // is safe; deleting a possibly durable root here could expose objects
        // to concurrent GC. The unissued ref is a recoverable orphan.
        self.sync_git_ref(&protected_ref)?;
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
        if let Some(root) = descriptor.byte_root.as_ref() {
            let height = self.walk_full_index(
                root,
                descriptor.identity.length(),
                None,
                scope,
                &mut objects,
                &mut digest,
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
            return Err(GraphError::UnknownObjectAvailability);
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
            return Err(if args.first() == Some(&"cat-file") {
                GraphError::UnknownObjectAvailability
            } else {
                GraphError::GitCommandFailed
            });
        }
        Ok(output.stdout)
    }

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
        self.git_output(&["update-ref", "-d", reference, &oid.to_hex()])?;
        self.sync_ref_directories(reference)
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

    fn sync_object_closure(
        &self,
        objects: &BTreeSet<NativeOid>,
        scope: &RepositoryReadScope,
    ) -> Result<(), GraphError> {
        let objects_output = self.git_output(&["rev-parse", "--git-path", "objects"])?;
        let objects_dir = resolve_git_path(self.repository.worktree(), &objects_output)?;
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
            scope.check_cancelled()?;
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
        sync_directory(&objects_dir)
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

fn sync_directory(path: &Path) -> Result<(), GraphError> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|_| GraphError::DurabilityFailed)
}

fn sync_all_pack_files(pack_dir: &Path) -> Result<(), GraphError> {
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

/// Proof that a complete content closure was verified, flushed and rooted by
/// a durable private Git ref.  It is non-cloneable and has no public
/// constructor.  Dropping it does not release that Git ref.
#[derive(Debug)]
pub struct ProtectedContentPin {
    repository_id: [u8; 32],
    database_id: [u8; 16],
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
            repository_id,
            database_id,
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
            pin_id: self.pin_id,
            descriptor_oid: self.descriptor_oid.clone(),
            identity: self.identity,
        }
    }

    pub(crate) fn protected_ref(&self) -> &str {
        &self.protected_ref
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
    let (schema_oid, primary_root, secondary_root, row_count) =
        decode_relation_value(&relation.value, algorithm)?;

    // The current profile does not define a complete secondary-domain tuple
    // beyond requiring ORP-1 pages. Do not silently admit an unrecognized
    // index domain against this row root.
    if secondary_root.is_some() {
        return Err(GraphError::InvalidDomain);
    }

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
    reader.verify_full_byte_index(&byte_root, encoded_length, schema_digest)?;

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

    fn verify_full_byte_index(
        &mut self,
        root: &NativeObjectId,
        expected_length: u64,
        expected_digest: [u8; 32],
    ) -> Result<(), GraphError> {
        let mut hasher = Sha256::new();
        let (length, _) = self.walk_byte_index(root, None, &mut hasher)?;
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
            } else {
                let (child_length, child_height) =
                    self.walk_byte_index(&entry.target, Some(height - 1), hasher)?;
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
            return Err(if args.first() == Some(&"cat-file") {
                GraphError::UnknownObjectAvailability
            } else {
                GraphError::GitCommandFailed
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
            let bounds = ordered_node_bounds_with(
                read_node,
                NodeData::OrderedBranch {
                    domain: domain.clone(),
                    height,
                    entries: entries.clone(),
                },
                expected_domain,
                expected_height,
            )?;
            if lower_exclusive.is_some_and(|lower| bounds.1 <= *lower) {
                return Err(GraphError::NonCanonicalData);
            }
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
                // A fence is only a pruning hint after its child proves it.
                // Validate every child visited before or at the search fence;
                // otherwise a lying middle fence can hide a matching key.
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
                if key <= &fence {
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
                let overlaps = lower_inclusive.is_none_or(|lower| child_bounds.2 >= *lower)
                    && upper_exclusive.is_none_or(|upper| child_bounds.1 < *upper);
                if overlaps {
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
                previous_fence = Some(fence);
                if output.len() >= limit {
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
        blob_reference.extend_from_slice(b"\x61x");
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
        malformed_oid_value.extend_from_slice(b"\x61x");
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
    ContextMismatch,
    InvalidReadScope,
    DescriptorNotInRow,
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
            Self::ContextMismatch => f.write_str("graph capability belongs to another context"),
            Self::InvalidReadScope => f.write_str("invalid owner-bound graph read scope"),
            Self::DescriptorNotInRow => {
                f.write_str("Blob descriptor is not a dependency of the admitted row")
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
            Self::UnknownObjectAvailability => f.write_str("native object availability is unknown"),
            Self::LegacyFormatReadOnly(format) => {
                write!(f, "repository format {format} is reader-only")
            }
            Self::UnsupportedWriterProfile => f.write_str("legacy writer profile is unsupported"),
        }
    }
}

impl std::error::Error for GraphError {}

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
}

fn rows_domain_for_version(version: &crate::row_store::RowMapVersion) -> Vec<u8> {
    rows_domain(version.relation_id(), version.schema().schema_digest())
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
        CborValue::Tag(60113, payload) => {
            output.insert(overflow_tag_oid(payload, algorithm)?);
        }
        CborValue::Tag(60111, _) => {}
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
                if !matches!(length, 60110..=60113) {
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
            let entries = decode_byte_entries(fields.get(4), height, algorithm)?;
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
    height: u8,
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
