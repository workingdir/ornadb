//! OGB-2 Blob/content primitives for the format-3 native graph.
//!
//! These types carry immutable content identity, chunk spans, native byte
//! index references, and streamed digest state.  They do not hydrate media
//! when metadata is inspected, and they do not provide a second object store
//! or a legacy loose/compact/Parquet writer.

use std::{fmt, io::Read};

use sha2::{Digest, Sha256};

use crate::native_graph::{
    GraphError, NativeObjectId, NativeObjectKind, MAX_GRAPH_HEIGHT, MAX_REFS,
};

pub const MAX_BLOB_LENGTH: u64 = i64::MAX as u64;
pub const GEAR_MINIMUM: usize = 65_536;
pub const GEAR_MASK: u64 = 262_143;
pub const GEAR_MAXIMUM: usize = 1_048_576;
pub const GEAR_SEED: &[u8] = b"orna.gear.v1\0";

/// Raw content identity.  MIME and suffix annotations are intentionally not
/// part of this commitment.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ContentIdentity {
    length: u64,
    sha256: [u8; 32],
}

impl ContentIdentity {
    pub fn new(length: u64, sha256: [u8; 32]) -> Result<Self, BlobStoreError> {
        if length > MAX_BLOB_LENGTH {
            return Err(BlobStoreError::LengthExceeded(length));
        }
        Ok(Self { length, sha256 })
    }

    pub const fn length(self) -> u64 {
        self.length
    }

    pub const fn sha256(self) -> [u8; 32] {
        self.sha256
    }
}

/// Canonical declared metadata carried by each referencing value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlobAnnotation {
    media_type: String,
    suffix: Option<String>,
}

impl BlobAnnotation {
    pub fn new(
        media_type: impl Into<String>,
        suffix: Option<impl Into<String>>,
    ) -> Result<Self, BlobStoreError> {
        let media_type = media_type.into();
        if !valid_media_type(&media_type) {
            return Err(BlobStoreError::InvalidMediaType);
        }
        let suffix = suffix.map(Into::into);
        if suffix
            .as_deref()
            .is_some_and(|suffix| !valid_suffix(suffix))
        {
            return Err(BlobStoreError::InvalidSuffix);
        }
        Ok(Self { media_type, suffix })
    }

    pub fn media_type(&self) -> &str {
        &self.media_type
    }

    pub fn suffix(&self) -> Option<&str> {
        self.suffix.as_deref()
    }
}

/// The one format-3 Blob descriptor.  A nonempty descriptor always uses a
/// native byte graph root; an empty descriptor is the single canonical empty
/// form with no root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlobDescriptor {
    identity: ContentIdentity,
    byte_root: Option<NativeObjectId>,
}

impl BlobDescriptor {
    pub fn empty() -> Self {
        Self {
            identity: ContentIdentity {
                length: 0,
                sha256: sha256_bytes(&[]),
            },
            byte_root: None,
        }
    }

    pub fn new(
        identity: ContentIdentity,
        byte_root: Option<NativeObjectId>,
    ) -> Result<Self, BlobStoreError> {
        if (identity.length() == 0) != byte_root.is_none() {
            return Err(BlobStoreError::InvalidEmptyDescriptor);
        }
        if identity.length() == 0 && identity.sha256() != sha256_bytes(&[]) {
            return Err(BlobStoreError::InvalidEmptyDescriptor);
        }
        Ok(Self {
            identity,
            byte_root,
        })
    }

    /// Verifies the complete raw payload against this descriptor.  Native Git
    /// object IDs are checked separately and never substituted for this hash.
    pub fn verify_full_bytes(&self, bytes: &[u8]) -> Result<(), BlobStoreError> {
        if bytes.len() as u64 != self.identity.length()
            || sha256_bytes(bytes) != self.identity.sha256()
        {
            return Err(BlobStoreError::ContentIdentityMismatch);
        }
        Ok(())
    }

    pub const fn identity(&self) -> ContentIdentity {
        self.identity
    }

    pub fn byte_root(&self) -> Option<&NativeObjectId> {
        self.byte_root.as_ref()
    }

    pub fn metadata(&self) -> BlobMetadata {
        BlobMetadata {
            identity: self.identity,
            has_payload_graph: self.byte_root.is_some(),
        }
    }
}

/// Metadata-only projection: no chunk or media payload is present here.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlobMetadata {
    identity: ContentIdentity,
    has_payload_graph: bool,
}

impl BlobMetadata {
    pub const fn identity(self) -> ContentIdentity {
        self.identity
    }

    pub const fn has_payload_graph(self) -> bool {
        self.has_payload_graph
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlobReference {
    descriptor: BlobDescriptor,
    annotation: BlobAnnotation,
}

impl BlobReference {
    pub fn new(descriptor: BlobDescriptor, annotation: BlobAnnotation) -> Self {
        Self {
            descriptor,
            annotation,
        }
    }

    pub fn descriptor(&self) -> &BlobDescriptor {
        &self.descriptor
    }

    pub fn annotation(&self) -> &BlobAnnotation {
        &self.annotation
    }
}

/// A content-defined chunk span and its independent raw chunk digest.  The
/// native Git OID is supplied only when an owner has materialized the chunk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChunkSpan {
    offset: u64,
    length: u64,
    sha256: [u8; 32],
}

impl ChunkSpan {
    pub fn new(offset: u64, length: u64, sha256: [u8; 32]) -> Result<Self, BlobStoreError> {
        if length == 0 || offset.checked_add(length).is_none() {
            return Err(BlobStoreError::InvalidChunkSpan);
        }
        Ok(Self {
            offset,
            length,
            sha256,
        })
    }

    pub const fn offset(&self) -> u64 {
        self.offset
    }

    pub const fn length(&self) -> u64 {
        self.length
    }

    pub const fn sha256(&self) -> [u8; 32] {
        self.sha256
    }

    pub fn verify_bytes(&self, bytes: &[u8]) -> Result<(), BlobStoreError> {
        if bytes.len() as u64 != self.length || sha256_bytes(bytes) != self.sha256 {
            return Err(BlobStoreError::ChunkDigestMismatch);
        }
        Ok(())
    }
}

/// One byte-index entry.  Leaf entries point to raw Git blobs and carry a
/// chunk hash; higher-level entries point to byte-index trees and carry no
/// chunk hash.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ByteIndexEntry {
    span_length: u64,
    target: NativeObjectId,
    chunk_sha256: Option<[u8; 32]>,
}

impl ByteIndexEntry {
    pub fn leaf(span: &ChunkSpan, target: NativeObjectId) -> Result<Self, BlobStoreError> {
        Ok(Self {
            span_length: span.length(),
            target,
            chunk_sha256: Some(span.sha256()),
        })
    }

    pub fn branch(span_length: u64, target: NativeObjectId) -> Result<Self, BlobStoreError> {
        if span_length == 0 || span_length > MAX_BLOB_LENGTH {
            return Err(BlobStoreError::InvalidChunkSpan);
        }
        Ok(Self {
            span_length,
            target,
            chunk_sha256: None,
        })
    }

    pub const fn span_length(&self) -> u64 {
        self.span_length
    }

    pub fn target(&self) -> &NativeObjectId {
        &self.target
    }

    pub const fn chunk_sha256(&self) -> Option<[u8; 32]> {
        self.chunk_sha256
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ByteIndexNode {
    height: u8,
    total_length: u64,
    entries: Vec<ByteIndexEntry>,
}

impl ByteIndexNode {
    pub fn leaf(entries: Vec<ByteIndexEntry>, total_length: u64) -> Result<Self, BlobStoreError> {
        Self::new(0, entries, total_length)
    }

    pub fn branch(
        height: u8,
        entries: Vec<ByteIndexEntry>,
        total_length: u64,
    ) -> Result<Self, BlobStoreError> {
        if height == 0 {
            return Err(BlobStoreError::InvalidHeight);
        }
        Self::new(height, entries, total_length)
    }

    fn new(
        height: u8,
        entries: Vec<ByteIndexEntry>,
        total_length: u64,
    ) -> Result<Self, BlobStoreError> {
        if height > MAX_GRAPH_HEIGHT || entries.len() > MAX_REFS {
            return Err(BlobStoreError::BoundsExceeded);
        }
        if total_length > MAX_BLOB_LENGTH {
            return Err(BlobStoreError::LengthExceeded(total_length));
        }
        if entries.is_empty() {
            return Err(BlobStoreError::InvalidByteIndex);
        }
        let leaf = height == 0;
        let algorithm = entries.first().map(|entry| entry.target.algorithm());
        let mut sum = 0u64;
        for entry in &entries {
            if entry.span_length == 0
                || entry.chunk_sha256.is_some() != leaf
                || Some(entry.target.algorithm()) != algorithm
            {
                return Err(BlobStoreError::InvalidByteIndex);
            }
            sum = sum
                .checked_add(entry.span_length)
                .ok_or(BlobStoreError::InvalidByteIndex)?;
        }
        if sum != total_length || (total_length == 0 && !entries.is_empty()) {
            return Err(BlobStoreError::InvalidByteIndex);
        }
        Ok(Self {
            height,
            total_length,
            entries,
        })
    }

    pub const fn height(&self) -> u8 {
        self.height
    }

    pub const fn total_length(&self) -> u64 {
        self.total_length
    }

    pub fn entries(&self) -> &[ByteIndexEntry] {
        &self.entries
    }
}

/// The deterministic OGB-2 Gear chunker.  It returns spans and hashes only;
/// callers can stream those spans into native Git objects without retaining a
/// second media store.
pub struct GearChunker {
    table: [u64; 256],
}

impl Default for GearChunker {
    fn default() -> Self {
        Self::new()
    }
}

impl GearChunker {
    pub fn new() -> Self {
        let mut table = [0u64; 256];
        for (byte, slot) in table.iter_mut().enumerate() {
            let mut hasher = Sha256::new();
            hasher.update(GEAR_SEED);
            hasher.update([byte as u8]);
            let digest = hasher.finalize();
            *slot = u64::from_be_bytes(digest[..8].try_into().expect("SHA-256 prefix"));
        }
        Self { table }
    }

    pub fn spans(&self, bytes: &[u8]) -> Result<Vec<ChunkSpan>, BlobStoreError> {
        if bytes.len() as u64 > MAX_BLOB_LENGTH {
            return Err(BlobStoreError::LengthExceeded(bytes.len() as u64));
        }
        if bytes.is_empty() {
            return Ok(Vec::new());
        }
        let mut output = Vec::new();
        let mut start = 0usize;
        let mut accumulator = 0u64;
        for (index, byte) in bytes.iter().copied().enumerate() {
            accumulator = accumulator
                .wrapping_shl(1)
                .wrapping_add(self.table[byte as usize]);
            let length = index + 1 - start;
            if length >= GEAR_MINIMUM && (accumulator & GEAR_MASK == 0 || length == GEAR_MAXIMUM) {
                output.push(chunk_span(bytes, start, index + 1)?);
                start = index + 1;
                accumulator = 0;
            }
        }
        if start < bytes.len() {
            output.push(chunk_span(bytes, start, bytes.len())?);
        }
        Ok(output)
    }
}

/// Streaming raw-content digest state.  The state never includes annotations
/// or native Git object headers.
pub struct ContentDigest {
    hasher: Sha256,
    length: u64,
}

impl Default for ContentDigest {
    fn default() -> Self {
        Self::new()
    }
}

impl ContentDigest {
    pub fn new() -> Self {
        Self {
            hasher: Sha256::new(),
            length: 0,
        }
    }

    pub fn update(&mut self, bytes: &[u8]) -> Result<(), BlobStoreError> {
        let length = self
            .length
            .checked_add(bytes.len() as u64)
            .ok_or(BlobStoreError::LengthExceeded(u64::MAX))?;
        if length > MAX_BLOB_LENGTH {
            return Err(BlobStoreError::LengthExceeded(length));
        }
        self.hasher.update(bytes);
        self.length = length;
        Ok(())
    }

    pub fn finish(self) -> ContentIdentity {
        ContentIdentity {
            length: self.length,
            sha256: self.hasher.finalize().into(),
        }
    }
}

pub fn digest_reader(reader: &mut impl Read) -> Result<ContentIdentity, BlobStoreError> {
    let mut digest = ContentDigest::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|_| BlobStoreError::ReadFailed)?;
        if read == 0 {
            return Ok(digest.finish());
        }
        digest.update(&buffer[..read])?;
    }
}

pub fn digest_bytes(bytes: &[u8]) -> ContentIdentity {
    let mut digest = ContentDigest::new();
    digest.update(bytes).expect("slice length is bounded");
    digest.finish()
}

fn chunk_span(bytes: &[u8], start: usize, end: usize) -> Result<ChunkSpan, BlobStoreError> {
    let length = end
        .checked_sub(start)
        .ok_or(BlobStoreError::InvalidChunkSpan)?;
    let digest = sha256_bytes(&bytes[start..end]);
    ChunkSpan::new(start as u64, length as u64, digest)
}

fn sha256_bytes(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn valid_media_type(value: &str) -> bool {
    !value.is_empty()
        && value.is_ascii()
        && value.contains('/')
        && !value.bytes().any(|byte| byte <= b' ' || byte == b'\x7f')
}

fn valid_suffix(value: &str) -> bool {
    !value.is_empty()
        && value.is_ascii()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-')
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BlobStoreError {
    LengthExceeded(u64),
    InvalidMediaType,
    InvalidSuffix,
    InvalidEmptyDescriptor,
    InvalidChunkSpan,
    InvalidHeight,
    BoundsExceeded,
    InvalidByteIndex,
    ContentIdentityMismatch,
    ChunkDigestMismatch,
    ReadFailed,
    Native(GraphError),
}

impl fmt::Display for BlobStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LengthExceeded(length) => write!(f, "Blob length {length} exceeds 2^63-1"),
            Self::InvalidMediaType => f.write_str("invalid canonical media type"),
            Self::InvalidSuffix => f.write_str("invalid safe suffix hint"),
            Self::InvalidEmptyDescriptor => f.write_str("empty descriptor must have no byte root"),
            Self::InvalidChunkSpan => f.write_str("invalid chunk span"),
            Self::InvalidHeight => f.write_str("invalid byte-index height"),
            Self::BoundsExceeded => {
                f.write_str("byte-index fanout or height exceeds format bounds")
            }
            Self::InvalidByteIndex => f.write_str("byte-index spans do not match total length"),
            Self::ContentIdentityMismatch => {
                f.write_str("full Blob bytes do not match descriptor length or SHA-256")
            }
            Self::ChunkDigestMismatch => {
                f.write_str("chunk bytes do not match span length or SHA-256")
            }
            Self::ReadFailed => f.write_str("content read failed"),
            Self::Native(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for BlobStoreError {}

impl From<GraphError> for BlobStoreError {
    fn from(error: GraphError) -> Self {
        Self::Native(error)
    }
}
