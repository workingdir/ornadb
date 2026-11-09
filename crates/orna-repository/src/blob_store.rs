//! OGB-2 Blob/content primitives for the format-3 native graph.
//!
//! These types carry immutable content identity, chunk spans, and streamed
//! digest state.  They do not hydrate media when metadata is inspected, and
//! they do not provide a second object store or a legacy
//! loose/compact/Parquet writer.

use std::{cell::Cell, fmt};

use sha2::{Digest, Sha256};

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
        if length == 0 || length > GEAR_MAXIMUM as u64 || offset.checked_add(length).is_none() {
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

thread_local! {
    /// Raw-content bytes digested on this thread by [`ContentDigest::update`].
    /// The schema identity digest is excluded: it hashes schema text, not payload.
    static RAW_CONTENT_BYTES_DIGESTED: Cell<u64> = const { Cell::new(0) };
}

/// Raw-content bytes this thread has digested so far: the rehash cost that
/// ORNA-VFS-010 requires to be measured. Callers measure a delta around the
/// operation they observe. The counter is thread-local, so a caller observes
/// only digests performed on its own thread.
pub fn raw_content_bytes_digested() -> u64 {
    RAW_CONTENT_BYTES_DIGESTED.with(Cell::get)
}

/// Streaming raw-content digest state.  The state never includes annotations
/// or native Git object headers; a domain-separated digest starts from its
/// domain prefix and still counts only the content in its length.
pub struct ContentDigest {
    hasher: Sha256,
    length: u64,
    counted: bool,
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
            counted: true,
        }
    }

    /// A digest of `domain`, NUL and the streamed content: a format-3 domain
    /// digest such as the `orna.schema.v3` schema identity (store-format-3
    /// section 5). The length counts the content only.
    pub(crate) fn domain_separated(domain: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(domain.as_bytes());
        hasher.update([0]);
        Self {
            hasher,
            length: 0,
            counted: false,
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
        if self.counted {
            RAW_CONTENT_BYTES_DIGESTED.with(|digested| {
                digested.set(digested.get().saturating_add(bytes.len() as u64));
            });
        }
        Ok(())
    }

    pub fn finish(self) -> ContentIdentity {
        ContentIdentity {
            length: self.length,
            sha256: self.hasher.finalize().into(),
        }
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BlobStoreError {
    LengthExceeded(u64),
    InvalidChunkSpan,
}

impl fmt::Display for BlobStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LengthExceeded(length) => write!(f, "Blob length {length} exceeds 2^63-1"),
            Self::InvalidChunkSpan => f.write_str("invalid chunk span"),
        }
    }
}

impl std::error::Error for BlobStoreError {}

#[cfg(test)]
mod tests {
    use super::{BlobStoreError, ChunkSpan, GEAR_MAXIMUM};

    #[test]
    fn chunk_spans_enforce_the_gear_maximum() {
        let digest = [0; 32];
        assert!(ChunkSpan::new(0, GEAR_MAXIMUM as u64, digest).is_ok());
        assert!(matches!(
            ChunkSpan::new(0, GEAR_MAXIMUM as u64 + 1, digest),
            Err(BlobStoreError::InvalidChunkSpan)
        ));
    }
}
