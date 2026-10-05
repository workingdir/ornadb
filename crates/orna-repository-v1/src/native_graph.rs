//! Format-3 native Git graph validation.
//!
//! This module is deliberately a seam for the repository context owner.  The
//! public values describe native objects and decoded node data, but admission
//! is `pub(crate)` and requires an owner-issued [`ValidatedRepositoryContext`].
//! In particular, a raw OID or a caller-provided resolver is not a repository
//! authority.

use std::{collections::BTreeMap, fmt, ops::Range};

pub const REPOSITORY_FORMAT: u8 = 3;
pub const NODE_DATA_LIMIT: usize = 65_536;
pub const MAX_REFS: usize = 256;
pub const MAX_GRAPH_HEIGHT: u8 = 64;
pub const MAX_SIGNED_LENGTH: u64 = i64::MAX as u64;

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
#[derive(Clone, Debug)]
pub struct NativeGraphContext {
    identity: ContextIdentity,
    database_id: [u8; 16],
    owner_id: [u8; 16],
    snapshot_id: [u8; 32],
    algorithm: GitHashAlgorithm,
    store_root: NativeOid,
    schema_digest: [u8; 32],
}

impl NativeGraphContext {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn issue(
        identity: [u8; 32],
        database_id: [u8; 16],
        owner_id: [u8; 16],
        snapshot_id: [u8; 32],
        algorithm: GitHashAlgorithm,
        store_root: NativeOid,
        schema_digest: [u8; 32],
    ) -> Result<Self, GraphError> {
        if store_root.algorithm() != algorithm {
            return Err(GraphError::InvalidOidWidth {
                expected: algorithm.width(),
                actual: store_root.as_bytes().len(),
            });
        }
        Ok(Self {
            identity: ContextIdentity(identity),
            database_id,
            owner_id,
            snapshot_id,
            algorithm,
            store_root,
            schema_digest,
        })
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
}

/// Verified immutable descriptor evidence.  It is intentionally unconstructible
/// outside this module until a native Git closure verifier issues it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VerifiedBlobDescriptor {
    descriptor_oid: NativeOid,
    identity: crate::blob_store::ContentIdentity,
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
    durable_root: NativeOid,
    journal_terminal: bool,
}

impl Pub3ReleaseReceipt {
    fn issue_after_pub3(
        repository_id: [u8; 32],
        pin_id: [u8; 16],
        durable_root: NativeOid,
        journal_terminal: bool,
    ) -> Result<Self, GraphError> {
        if !journal_terminal {
            return Err(GraphError::PublicationNotTerminal);
        }
        Ok(Self {
            repository_id,
            pin_id,
            durable_root,
            journal_terminal,
        })
    }

    pub(crate) fn authorizes(&self, pin: &ProtectedContentPin) -> bool {
        self.journal_terminal
            && self.repository_id == pin.repository_id
            && self.pin_id == pin.pin_id
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
    pub key: Vec<u8>,
    pub value: NativeDependency,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderedBranchEntry {
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
            Self::OrderedLeaf { entries, .. } => {
                for entry in entries {
                    dependencies.push(entry.value.clone());
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
        if dependencies.len() > MAX_REFS {
            return Err(GraphError::FanoutExceeded(dependencies.len()));
        }
        Ok(dependencies)
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
                array(&mut output, 4);
                uint(&mut output, 1);
                uint(&mut output, 1);
                bytes(&mut output, domain);
                array(&mut output, entries.len() as u64);
                for entry in entries {
                    array(&mut output, 3);
                    bytes(&mut output, &entry.key);
                    bytes(&mut output, entry.value.oid.as_bytes());
                    uint(
                        &mut output,
                        match entry.value.kind {
                            NativeObjectKind::Tree => 0,
                            NativeObjectKind::Blob => 1,
                        },
                    );
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
                array(&mut output, 5);
                uint(&mut output, 1);
                uint(&mut output, 2);
                bytes(&mut output, domain);
                uint(&mut output, u64::from(*height));
                array(&mut output, entries.len() as u64);
                for entry in entries {
                    check_length(entry.row_count)?;
                    array(&mut output, 3);
                    bytes(&mut output, &entry.inclusive_max_key);
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

fn check_domain(domain: &[u8]) -> Result<(), GraphError> {
    if domain.is_empty() || domain.len() > 256 {
        Err(GraphError::InvalidDomain)
    } else {
        Ok(())
    }
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
enum CborValue {
    Unsigned(u64),
    Bytes(Vec<u8>),
    Array(Vec<CborValue>),
    Null,
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
        let length = self.argument(additional)?;
        match major {
            0 => Ok(CborValue::Unsigned(length)),
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
            7 if additional == 22 => Ok(CborValue::Null),
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
            let domain = bytes_field(fields.get(2))?.to_vec();
            check_domain(&domain)?;
            let entries =
                array_fields(fields.get(3).cloned().ok_or(GraphError::NonCanonicalData)?)?
                    .into_iter()
                    .map(|value| {
                        let fields = array_fields(value)?;
                        if fields.len() != 3 {
                            return Err(GraphError::NonCanonicalData);
                        }
                        let kind = match unsigned(fields.get(2))? {
                            0 => NativeObjectKind::Tree,
                            1 => NativeObjectKind::Blob,
                            _ => return Err(GraphError::NonCanonicalData),
                        };
                        Ok(OrderedLeafEntry {
                            key: bytes_field(fields.first())?.to_vec(),
                            value: NativeDependency::new(
                                oid(bytes_field(fields.get(1))?, algorithm)?,
                                kind,
                            ),
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
            Ok(NodeData::OrderedLeaf { domain, entries })
        }
        NodeKind::OrderedBranch => {
            let domain = bytes_field(fields.get(2))?.to_vec();
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
                            inclusive_max_key: bytes_field(fields.first())?.to_vec(),
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
