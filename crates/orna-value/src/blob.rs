//! Content identity, Blob representations and the OVB-2/ROV-3/SOV-3 Blob codecs.

use crate::Result;
use crate::cbor::{Reader, head};
use crate::error::Error;
use crate::format3::{
    ContextValue, Format3Context, Format3ContextInner, ValueFormat, oid_matches_algorithm,
    validate_context_raw,
};
use crate::identity::bytes32;
use crate::media::{MediaAnnotation, MimeRegistry};
use crate::raw::{Raw, array, int_u64, tag};
use crate::tags::{OVB2_BLOB_TAG, ROV3_BLOB_TAG, SOV3_BLOB_TAG};
use num_bigint::BigInt;
use sha2::{Digest as _, Sha256};
use std::cmp::Ordering;
use std::fmt;
use std::io::{Read as IoRead, Write as IoWrite};
use std::sync::Arc;

pub(crate) const MAX_BLOB_LENGTH: u64 = 1_u64 << 63;

pub(crate) const MAX_BLOB_READ: u64 = 8 * 1024 * 1024;

/// The raw content commitment shared by every Blob representation.
///
/// This is deliberately not a Git object ID: native object layout, chunking,
/// compression and repository hash algorithm do not enter content identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ContentIdentity {
    length: u64,
    sha256: [u8; 32],
}

impl ContentIdentity {
    pub fn new(length: u64, sha256: [u8; 32]) -> Result<Self> {
        if length >= MAX_BLOB_LENGTH {
            return Err(Error::Limit);
        }
        Ok(Self { length, sha256 })
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut digest = Sha256::new();
        digest.update(bytes);
        let mut sha256 = [0_u8; 32];
        sha256.copy_from_slice(&digest.finalize());
        // A slice cannot have a length outside the admitted in-memory range.
        Self {
            length: bytes.len() as u64,
            sha256,
        }
    }

    pub const fn length(self) -> u64 {
        self.length
    }

    pub const fn sha256(self) -> [u8; 32] {
        self.sha256
    }
}

/// A native Git object ID carried by a ROV-3 descriptor reference.
///
/// The value layer validates width but intentionally does not interpret the
/// referenced tree or invent a graph layout. Graph ownership stays with the
/// storage layer.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum NativeOid {
    Sha1([u8; 20]),
    Sha256([u8; 32]),
}

impl NativeOid {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        match bytes.len() {
            20 => {
                let mut oid = [0_u8; 20];
                oid.copy_from_slice(bytes);
                Ok(Self::Sha1(oid))
            }
            32 => {
                let mut oid = [0_u8; 32];
                oid.copy_from_slice(bytes);
                Ok(Self::Sha256(oid))
            }
            _ => Err(Error::InvalidOid),
        }
    }

    /// Digest width in bytes. A `NativeOid` always holds a full digest, so the
    /// width is never zero and there is no meaningful emptiness predicate.
    #[allow(clippy::len_without_is_empty)]
    pub const fn len(&self) -> usize {
        match self {
            Self::Sha1(_) => 20,
            Self::Sha256(_) => 32,
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Sha1(oid) => oid,
            Self::Sha256(oid) => oid,
        }
    }
}

/// A lazy descriptor reference. The resolver is optional so a row can retain
/// a verified reference and expose metadata while its content is offline.
#[derive(Clone)]
pub struct ContentReference {
    identity: ContentIdentity,
    descriptor_oid: NativeOid,
    context: Arc<Format3ContextInner>,
}

pub type BlobDescriptor = ContentReference;

pub type LazyBlobReference = ContentReference;

impl fmt::Debug for ContentReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContentReference")
            .field("identity", &self.identity)
            .field("descriptor_oid", &self.descriptor_oid)
            .field("context", &self.context.coordinates)
            .finish()
    }
}

impl PartialEq for ContentReference {
    fn eq(&self, other: &Self) -> bool {
        self.identity == other.identity
            && self.descriptor_oid == other.descriptor_oid
            && Arc::ptr_eq(&self.context, &other.context)
    }
}

impl Eq for ContentReference {}

impl ContentReference {
    pub(crate) fn from_context(
        identity: ContentIdentity,
        descriptor_oid: NativeOid,
        context: Arc<Format3ContextInner>,
    ) -> Result<Self> {
        if !oid_matches_algorithm(&descriptor_oid, context.git_oid_algorithm) {
            return Err(Error::InvalidOid);
        }
        Ok(Self {
            identity,
            descriptor_oid,
            context,
        })
    }

    pub const fn identity(&self) -> ContentIdentity {
        self.identity
    }

    pub fn descriptor_oid(&self) -> &NativeOid {
        &self.descriptor_oid
    }

    pub fn has_resolver(&self) -> bool {
        true
    }

    pub fn context(&self) -> Format3Context {
        Format3Context {
            inner: Arc::clone(&self.context),
        }
    }
}

/// The narrow lazy-read seam owned by the graph/storage layer.
///
/// A resolver validates the exact referenced descriptor against both its OID
/// and `ContentIdentity`, then verifies the native path and every intersecting
/// chunk's object ID, length and raw SHA-256 before returning only the requested
/// range (plus separately bounded read-ahead). A partial range is not proof of
/// the full Blob digest; a full-range request must verify that commitment. The
/// resolver never rebinds a value to another snapshot or HEAD and is retained
/// only inside an owner-issued [`Format3Context`].
pub trait BlobResolver: Send + Sync {
    fn read_range(&self, reference: &ContentReference, offset: u64, length: u64)
    -> Result<Vec<u8>>;
}

#[derive(Clone)]
enum BlobContent {
    Inline(Arc<[u8]>),
    Reference(Arc<ContentReference>),
    Descriptor(NativeOid),
    Commitment,
}

/// An immutable annotated Blob with lazy content resolution.
///
/// Metadata, length, content identity, annotation changes and ROV-3/SOV-3
/// encodings do not materialize payload bytes. OVB-2 full-value encoding is
/// the explicit operation that may require hydration.
pub struct Blob {
    pub(crate) identity: ContentIdentity,
    annotation: MediaAnnotation,
    content: BlobContent,
    pub(crate) context: Option<Format3Context>,
}

pub type BlobValue = Blob;

pub type AnnotatedBlob = Blob;

impl Clone for Blob {
    fn clone(&self) -> Self {
        Self {
            identity: self.identity,
            annotation: self.annotation.clone(),
            content: self.content.clone(),
            context: self.context.clone(),
        }
    }
}

impl fmt::Debug for Blob {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Blob")
            .field("identity", &self.identity)
            .field("annotation", &self.annotation)
            .field("hydrated", &self.is_hydrated())
            .field("descriptor_oid", &self.descriptor_oid())
            .field("context_bound", &self.context.is_some())
            .finish()
    }
}

impl PartialEq for Blob {
    fn eq(&self, other: &Self) -> bool {
        self.value_eq(other).unwrap_or(false)
    }
}

/// Payload-free metadata for one Blob value: its annotation, committed content
/// identity and the descriptor the stored reference names. `hydrated` reports
/// whether the bytes are already local.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlobMetadata {
    media_type: String,
    suffix: Option<String>,
    length: u64,
    sha256: [u8; 32],
    descriptor_oid: Option<NativeOid>,
    hydrated: bool,
}

impl BlobMetadata {
    /// Builds the payload-free metadata a stored ROV-3 Blob reference
    /// carries.
    ///
    /// This is the constructor for readers that decode a committed row's own
    /// field tuple: the stored reference already holds the content identity,
    /// annotation and descriptor OID, so no context and no content read are
    /// needed. The bytes are not local, so `hydrated` is false.
    pub fn from_stored_reference(
        length: u64,
        sha256: [u8; 32],
        media_type: &str,
        suffix: Option<&str>,
        descriptor_oid: Option<NativeOid>,
    ) -> Self {
        Self {
            media_type: media_type.to_owned(),
            suffix: suffix.map(str::to_owned),
            length,
            sha256,
            descriptor_oid,
            hydrated: false,
        }
    }

    pub fn media_type(&self) -> &str {
        &self.media_type
    }

    pub fn suffix(&self) -> Option<&str> {
        self.suffix.as_deref()
    }

    pub const fn length(&self) -> u64 {
        self.length
    }

    pub const fn sha256(&self) -> [u8; 32] {
        self.sha256
    }

    /// The OGS-1 descriptor OID the stored reference names, when it names one.
    ///
    /// A stored ROV-3 Blob always does; an inline or commitment value does not,
    /// which is a different answer from a reference whose descriptor could not
    /// be decoded.
    pub fn descriptor_oid(&self) -> Option<&NativeOid> {
        self.descriptor_oid.as_ref()
    }

    pub const fn is_hydrated(&self) -> bool {
        self.hydrated
    }
}

impl Blob {
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self {
            identity: ContentIdentity::from_bytes(&bytes),
            annotation: MediaAnnotation::default(),
            content: BlobContent::Inline(Arc::from(bytes.into_boxed_slice())),
            context: None,
        }
    }

    pub fn new(bytes: Vec<u8>) -> Self {
        Self::from_bytes(bytes)
    }

    pub fn from_bytes_with_annotation(
        bytes: Vec<u8>,
        media_type: &str,
        suffix: Option<&str>,
    ) -> Result<Self> {
        let annotation = MimeRegistry::mime1().annotation(media_type, suffix)?;
        let identity = ContentIdentity::from_bytes(&bytes);
        Ok(Self {
            identity,
            annotation,
            content: BlobContent::Inline(Arc::from(bytes.into_boxed_slice())),
            context: None,
        })
    }

    pub fn from_reference(reference: ContentReference) -> Result<Self> {
        let identity = reference.identity;
        let context = reference.context();
        Ok(Self {
            identity,
            annotation: MediaAnnotation::default(),
            content: BlobContent::Reference(Arc::new(reference)),
            context: Some(context),
        })
    }

    pub fn from_reference_with_annotation(
        reference: ContentReference,
        media_type: &str,
        suffix: Option<&str>,
    ) -> Result<Self> {
        let identity = reference.identity;
        let context = reference.context();
        let annotation = context.mime_registry().annotation(media_type, suffix)?;
        Ok(Self {
            identity,
            annotation,
            content: BlobContent::Reference(Arc::new(reference)),
            context: Some(context),
        })
    }

    fn from_descriptor_in_context(
        identity: ContentIdentity,
        context: &Format3Context,
        descriptor_oid: NativeOid,
        media_type: &str,
        suffix: Option<&str>,
    ) -> Result<Self> {
        if !oid_matches_algorithm(&descriptor_oid, context.git_oid_algorithm()) {
            return Err(Error::InvalidOid);
        }
        Ok(Self {
            identity,
            annotation: context.mime_registry().annotation(media_type, suffix)?,
            content: BlobContent::Descriptor(descriptor_oid),
            context: Some(context.clone()),
        })
    }

    pub fn from_semantic_commitment(
        identity: ContentIdentity,
        media_type: &str,
        suffix: Option<&str>,
    ) -> Result<Self> {
        Ok(Self {
            identity,
            annotation: MimeRegistry::mime1().annotation(media_type, suffix)?,
            content: BlobContent::Commitment,
            context: None,
        })
    }

    pub fn from_semantic_commitment_in_context(
        identity: ContentIdentity,
        context: &Format3Context,
        media_type: &str,
        suffix: Option<&str>,
    ) -> Result<Self> {
        Ok(Self {
            identity,
            annotation: context.mime_registry().annotation(media_type, suffix)?,
            content: BlobContent::Commitment,
            context: Some(context.clone()),
        })
    }

    pub const fn length(&self) -> u64 {
        self.identity.length()
    }

    pub const fn content_identity(&self) -> ContentIdentity {
        self.identity
    }

    pub const fn is_hydrated(&self) -> bool {
        matches!(&self.content, BlobContent::Inline(_))
    }

    pub fn media_type(&self) -> &str {
        self.annotation.media_type()
    }

    pub fn suffix(&self) -> Option<&str> {
        self.annotation.suffix()
    }

    pub fn annotation(&self) -> &MediaAnnotation {
        &self.annotation
    }

    /// Projects this Blob's descriptor metadata without reading its content.
    /// Querying rows through this projection never hydrates a payload, so it
    /// is safe for listings over large media.
    pub fn metadata(&self) -> BlobMetadata {
        BlobMetadata {
            media_type: self.media_type().to_owned(),
            suffix: self.suffix().map(str::to_owned),
            length: self.length(),
            sha256: self.content_identity().sha256(),
            descriptor_oid: self.descriptor_oid().cloned(),
            hydrated: self.is_hydrated(),
        }
    }

    pub fn descriptor_oid(&self) -> Option<&NativeOid> {
        match &self.content {
            BlobContent::Reference(reference) => Some(reference.descriptor_oid()),
            BlobContent::Descriptor(descriptor_oid) => Some(descriptor_oid),
            BlobContent::Inline(_) | BlobContent::Commitment => None,
        }
    }

    pub fn content_reference(&self) -> Option<ContentReference> {
        match &self.content {
            BlobContent::Reference(reference) => Some(reference.as_ref().clone()),
            BlobContent::Inline(_) | BlobContent::Descriptor(_) | BlobContent::Commitment => None,
        }
    }

    /// Return a new value with the same lazy content and a new annotation.
    pub fn annotate(&self, media_type: &str, suffix: Option<&str>) -> Result<Self> {
        let mime_registry = self
            .context
            .as_ref()
            .map(Format3Context::mime_registry)
            .unwrap_or_else(MimeRegistry::mime1);
        Ok(Self {
            identity: self.identity,
            annotation: mime_registry.canonical_annotation(media_type, suffix)?,
            content: self.content.clone(),
            context: self.context.clone(),
        })
    }

    fn read_chunk_limit(&self) -> u64 {
        self.context
            .as_ref()
            .map(|context| context.quota().max_read_bytes())
            .unwrap_or(MAX_BLOB_READ)
            .min(MAX_BLOB_READ)
    }

    /// Reads one bounded byte range. Lazy references rely on their resolver's
    /// descriptor/path and intersecting-chunk verification; the full raw SHA-256
    /// is checked here only when this request covers the complete Blob.
    pub fn read(&self, offset: u64, length: u64) -> Result<Vec<u8>> {
        if length > MAX_BLOB_READ || offset > self.length() {
            return Err(Error::InvalidRange);
        }
        let expected = length.min(self.length() - offset);
        if expected == 0 {
            if let BlobContent::Reference(reference) = &self.content {
                let bytes = read_reference_range(reference, offset, 0)?;
                if self.length() == 0 && ContentIdentity::from_bytes(&bytes) != self.identity {
                    return Err(Error::ContentDigestMismatch);
                }
                return Ok(bytes);
            }
            if self.length() == 0 && ContentIdentity::from_bytes(&[]) != self.identity {
                return Err(Error::ContentDigestMismatch);
            }
            return Ok(Vec::new());
        }
        match &self.content {
            BlobContent::Inline(bytes) => {
                let start = usize::try_from(offset).map_err(|_| Error::Limit)?;
                let end = start.checked_add(expected as usize).ok_or(Error::Limit)?;
                Ok(bytes
                    .get(start..end)
                    .ok_or(Error::ContentDigestMismatch)?
                    .to_vec())
            }
            BlobContent::Reference(reference) => {
                let bytes = read_reference_range(reference, offset, expected)?;
                if offset == 0
                    && expected == self.length()
                    && ContentIdentity::from_bytes(&bytes) != self.identity
                {
                    return Err(Error::ContentDigestMismatch);
                }
                Ok(bytes)
            }
            BlobContent::Descriptor(_) => Err(Error::ContentUnavailable),
            BlobContent::Commitment => Err(Error::ContentUnavailable),
        }
    }

    pub fn read_to_end(&self) -> Result<Vec<u8>> {
        let capacity = usize::try_from(self.length()).map_err(|_| Error::Limit)?;
        let mut result = Vec::new();
        result
            .try_reserve_exact(capacity)
            .map_err(|_| Error::Limit)?;
        let mut offset = 0_u64;
        let chunk_limit = self.read_chunk_limit();
        let mut digest = Sha256::new();
        if self.length() == 0
            && let BlobContent::Reference(reference) = &self.content
        {
            let bytes = read_reference_range(reference, 0, 0)?;
            result.extend_from_slice(&bytes);
        }
        while offset < self.length() {
            let size = (self.length() - offset).min(chunk_limit);
            let bytes = match &self.content {
                BlobContent::Inline(content) => {
                    let start = usize::try_from(offset).map_err(|_| Error::Limit)?;
                    let length = usize::try_from(size).map_err(|_| Error::Limit)?;
                    let end = start.checked_add(length).ok_or(Error::Limit)?;
                    content
                        .get(start..end)
                        .ok_or(Error::ContentDigestMismatch)?
                        .to_vec()
                }
                BlobContent::Reference(reference) => read_reference_range(reference, offset, size)?,
                BlobContent::Descriptor(_) | BlobContent::Commitment => {
                    return Err(Error::ContentUnavailable);
                }
            };
            digest.update(&bytes);
            result.extend_from_slice(&bytes);
            offset = offset.checked_add(size).ok_or(Error::Limit)?;
        }
        if digest.finalize()[..] != self.identity.sha256 {
            return Err(Error::ContentDigestMismatch);
        }
        Ok(result)
    }

    /// Verifies the complete raw content against the identity in bounded
    /// chunks. Unlike `read_to_end`, no buffer of the full length is kept: each
    /// chunk is hashed and discarded, so memory is bounded by the read limit.
    pub fn verify(&self) -> Result<()> {
        let chunk_limit = self.read_chunk_limit();
        let mut digest = Sha256::new();
        if self.length() == 0
            && let BlobContent::Reference(reference) = &self.content
        {
            read_reference_range(reference, 0, 0)?;
        }
        let mut offset = 0_u64;
        while offset < self.length() {
            let size = (self.length() - offset).min(chunk_limit);
            match &self.content {
                BlobContent::Inline(content) => {
                    let start = usize::try_from(offset).map_err(|_| Error::Limit)?;
                    let length = usize::try_from(size).map_err(|_| Error::Limit)?;
                    let end = start.checked_add(length).ok_or(Error::Limit)?;
                    digest.update(
                        content
                            .get(start..end)
                            .ok_or(Error::ContentDigestMismatch)?,
                    );
                }
                BlobContent::Reference(reference) => {
                    let bytes = read_reference_range(reference, offset, size)?;
                    digest.update(&bytes);
                }
                BlobContent::Descriptor(_) | BlobContent::Commitment => {
                    return Err(Error::ContentUnavailable);
                }
            }
            offset = offset.checked_add(size).ok_or(Error::Limit)?;
        }
        if digest.finalize()[..] != self.identity.sha256 {
            return Err(Error::ContentDigestMismatch);
        }
        Ok(())
    }

    /// Two descriptor-backed blobs in one admitted context with equal layout OID
    /// and equal identity are treated as equal content without streaming
    /// (ORNA-BLOB-005). A layout OID identifies an encoding, not logical content,
    /// so this is a trust assumption on the admitted context's resolver: the
    /// shortcut skips the digest check, and the identity equality is the only
    /// binding between the two references.
    fn shares_descriptor_witness(&self, other: &Self) -> bool {
        match (&self.content, &other.content, &self.context, &other.context) {
            (
                BlobContent::Descriptor(left),
                BlobContent::Descriptor(right),
                Some(left_context),
                Some(right_context),
            ) => left == right && Arc::ptr_eq(&left_context.inner, &right_context.inner),
            _ => false,
        }
    }

    /// Compare exact bytes while ignoring annotation metadata.
    pub fn same_content(&self, other: &Self) -> Result<bool> {
        self.same_content_checked(other, || Ok(()))
    }

    /// Like [`Blob::same_content`], but calls `checkpoint` before each streamed
    /// chunk so callers can enforce cancellation and budgets (ORNA-BLOB-015).
    pub fn same_content_checked(
        &self,
        other: &Self,
        mut checkpoint: impl FnMut() -> Result<()>,
    ) -> Result<bool> {
        if self.identity != other.identity {
            return Ok(false);
        }
        if self.length() == 0 {
            let empty = ContentIdentity::from_bytes(&[]);
            return if self.identity == empty {
                Ok(true)
            } else {
                Err(Error::ContentDigestMismatch)
            };
        }
        if same_content_source(&self.content, &other.content)
            || self.shares_descriptor_witness(other)
        {
            return Ok(true);
        }
        let mut left_digest = Sha256::new();
        let mut right_digest = Sha256::new();
        let mut differs = false;
        let mut offset = 0_u64;
        let chunk_limit = self.read_chunk_limit().min(other.read_chunk_limit());
        while offset < self.length() {
            checkpoint()?;
            let size = (self.length() - offset).min(chunk_limit);
            let left = read_content_range(self, offset, size)?;
            let right = read_content_range(other, offset, size)?;
            if left != right {
                // Keep streaming: a single tampered side must be reported as
                // corruption, not as a collision, so classification waits for
                // both full digests (see `exact_content_outcome`).
                differs = true;
            }
            left_digest.update(&left);
            right_digest.update(&right);
            offset = offset.checked_add(size).ok_or(Error::Limit)?;
        }
        let left_verified = left_digest.finalize()[..] == self.identity.sha256;
        let right_verified = right_digest.finalize()[..] == self.identity.sha256;
        Self::exact_content_outcome(differs, left_verified, right_verified)
    }

    /// Classify a full streamed comparison. A digest failure on either side is
    /// corruption and wins over byte differences; only two verified digests with
    /// differing bytes prove a collision (ORNA-BLOB-005).
    pub(crate) fn exact_content_outcome(
        differs: bool,
        left_verified: bool,
        right_verified: bool,
    ) -> Result<bool> {
        if !left_verified || !right_verified {
            return Err(Error::ContentDigestMismatch);
        }
        if differs {
            return Err(Error::ContentCollision);
        }
        Ok(true)
    }

    /// Compare annotations and exact content, returning unavailable when two
    /// independent lazy commitments cannot be proved equal without bytes.
    pub fn value_eq(&self, other: &Self) -> Result<bool> {
        if self.annotation != other.annotation {
            return Ok(false);
        }
        self.same_content(other)
    }

    pub fn to_context_value(&self, format: ValueFormat) -> Result<ContextValue> {
        ContextValue::from_blob(self, format)
    }

    /// Streams this annotated Blob as OVB-2, rejecting an undersized output
    /// budget before resolving any content. The sink may contain a partial
    /// value if a later read or write fails; callers must discard it on error.
    pub fn encode_ovb2_to_writer<W: IoWrite>(
        &self,
        writer: &mut W,
        max_encoded_bytes: u64,
    ) -> Result<u64> {
        let (prefix, suffix, encoded_len) = self.ovb2_parts()?;
        self.preflight_ovb2(encoded_len, max_encoded_bytes)?;
        self.write_ovb2_preflighted(writer, &prefix, &suffix, encoded_len)
    }

    fn ovb2_parts(&self) -> Result<(Vec<u8>, Vec<u8>, u64)> {
        ovb2_parts_for(self.length(), &self.annotation)
    }

    fn preflight_ovb2(&self, encoded_len: u64, max_encoded_bytes: u64) -> Result<()> {
        if encoded_len > max_encoded_bytes {
            return Err(Error::QuotaExceeded);
        }
        if matches!(
            &self.content,
            BlobContent::Descriptor(_) | BlobContent::Commitment
        ) && self.length() != 0
        {
            return Err(Error::ContentUnavailable);
        }
        if let BlobContent::Reference(reference) = &self.content {
            reference.context.ensure_live()?;
        }
        Ok(())
    }

    fn write_ovb2_preflighted<W: IoWrite>(
        &self,
        writer: &mut W,
        prefix: &[u8],
        suffix: &[u8],
        encoded_len: u64,
    ) -> Result<u64> {
        writer.write_all(prefix).map_err(|_| Error::SinkFailed)?;
        let mut digest = Sha256::new();
        let mut offset = 0_u64;
        let chunk_limit = self.read_chunk_limit();
        while offset < self.length() {
            let size = (self.length() - offset).min(chunk_limit);
            match &self.content {
                BlobContent::Inline(content) => {
                    let start = usize::try_from(offset).map_err(|_| Error::Limit)?;
                    let length = usize::try_from(size).map_err(|_| Error::Limit)?;
                    let end = start.checked_add(length).ok_or(Error::Limit)?;
                    let bytes = content
                        .get(start..end)
                        .ok_or(Error::ContentDigestMismatch)?;
                    writer.write_all(bytes).map_err(|_| Error::SinkFailed)?;
                    digest.update(bytes);
                }
                BlobContent::Reference(_) => {
                    let BlobContent::Reference(reference) = &self.content else {
                        return Err(Error::ContentUnavailable);
                    };
                    let bytes = read_reference_range(reference, offset, size)?;
                    writer.write_all(&bytes).map_err(|_| Error::SinkFailed)?;
                    digest.update(&bytes);
                }
                BlobContent::Descriptor(_) | BlobContent::Commitment => {
                    return Err(Error::ContentUnavailable);
                }
            }
            offset = offset.checked_add(size).ok_or(Error::Limit)?;
        }
        if digest.finalize()[..] != self.identity.sha256 {
            return Err(Error::ContentDigestMismatch);
        }
        writer.write_all(suffix).map_err(|_| Error::SinkFailed)?;
        Ok(encoded_len)
    }
}

fn ovb2_parts_for(length: u64, annotation: &MediaAnnotation) -> Result<(Vec<u8>, Vec<u8>, u64)> {
    if length >= MAX_BLOB_LENGTH {
        return Err(Error::Limit);
    }
    let mut prefix = Vec::with_capacity(16);
    head(&mut prefix, 6, OVB2_BLOB_TAG);
    head(&mut prefix, 4, 3);
    head(&mut prefix, 2, length);

    let media_type = annotation.media_type().as_bytes();
    let mut suffix = Vec::new();
    head(
        &mut suffix,
        3,
        u64::try_from(media_type.len()).map_err(|_| Error::Limit)?,
    );
    suffix.extend_from_slice(media_type);
    match annotation.suffix() {
        Some(value) => {
            head(
                &mut suffix,
                3,
                u64::try_from(value.len()).map_err(|_| Error::Limit)?,
            );
            suffix.extend_from_slice(value.as_bytes());
        }
        None => suffix.push(0xf6),
    }

    let prefix_len = u64::try_from(prefix.len()).map_err(|_| Error::Limit)?;
    let suffix_len = u64::try_from(suffix.len()).map_err(|_| Error::Limit)?;
    let encoded_len = prefix_len
        .checked_add(length)
        .and_then(|length| length.checked_add(suffix_len))
        .ok_or(Error::Limit)?;
    Ok((prefix, suffix, encoded_len))
}

/// Streams exact-length raw bytes as an annotated OVB-2 Blob without building
/// the payload in memory. The complete encoded size is checked before reading
/// or writing. The sink may contain a partial value on I/O failure and must be
/// discarded unless this function succeeds.
pub fn encode_ovb2_reader_to_writer<R: IoRead, W: IoWrite>(
    reader: &mut R,
    length: u64,
    annotation: &MediaAnnotation,
    writer: &mut W,
    max_encoded_bytes: u64,
) -> Result<u64> {
    let (prefix, suffix, encoded_len) = ovb2_parts_for(length, annotation)?;
    if encoded_len > max_encoded_bytes {
        return Err(Error::QuotaExceeded);
    }

    writer.write_all(&prefix).map_err(|_| Error::SinkFailed)?;
    let mut remaining = length;
    let mut buffer = [0_u8; 64 * 1024];
    while remaining != 0 {
        let chunk_length =
            usize::try_from(remaining.min(buffer.len() as u64)).map_err(|_| Error::Limit)?;
        let mut filled = 0;
        while filled < chunk_length {
            match reader.read(&mut buffer[filled..chunk_length]) {
                Ok(0) => return Err(Error::ContentUnavailable),
                Ok(count) => filled += count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(Error::ContentUnavailable),
            }
        }
        writer
            .write_all(&buffer[..chunk_length])
            .map_err(|_| Error::SinkFailed)?;
        remaining -= chunk_length as u64;
    }

    let mut extra = [0_u8; 1];
    loop {
        match reader.read(&mut extra) {
            Ok(0) => break,
            Ok(_) => return Err(Error::InvalidRange),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(Error::ContentUnavailable),
        }
    }
    writer.write_all(&suffix).map_err(|_| Error::SinkFailed)?;
    Ok(encoded_len)
}

/// Encodes an annotated Blob as OVB-2 into a Vec with a caller supplied byte
/// ceiling. The exact output size is checked before allocation or content
/// resolution; allocation is reserved fallibly before bytes are written.
pub fn encode_ovb2_bounded(blob: &Blob, max_encoded_bytes: u64) -> Result<Vec<u8>> {
    let (prefix, suffix, encoded_len) = blob.ovb2_parts()?;
    blob.preflight_ovb2(encoded_len, max_encoded_bytes)?;
    let capacity = usize::try_from(encoded_len).map_err(|_| Error::Limit)?;
    let mut encoded = Vec::new();
    encoded
        .try_reserve_exact(capacity)
        .map_err(|_| Error::Limit)?;
    blob.write_ovb2_preflighted(&mut encoded, &prefix, &suffix, encoded_len)?;
    Ok(encoded)
}

fn same_content_source(left: &BlobContent, right: &BlobContent) -> bool {
    match (left, right) {
        (BlobContent::Inline(left), BlobContent::Inline(right)) => Arc::ptr_eq(left, right),
        (BlobContent::Reference(left), BlobContent::Reference(right)) => left == right,
        (BlobContent::Commitment, BlobContent::Commitment) => false,
        _ => false,
    }
}

fn read_reference_range(reference: &ContentReference, offset: u64, length: u64) -> Result<Vec<u8>> {
    let bytes = reference.context.read_range(reference, offset, length)?;
    if bytes.len() as u64 != length {
        return Err(Error::ContentUnavailable);
    }
    Ok(bytes)
}

fn read_content_range(blob: &Blob, offset: u64, length: u64) -> Result<Vec<u8>> {
    let end = offset.checked_add(length).ok_or(Error::Limit)?;
    if end > blob.length() {
        return Err(Error::InvalidRange);
    }
    match &blob.content {
        BlobContent::Inline(bytes) => {
            let start = usize::try_from(offset).map_err(|_| Error::Limit)?;
            let length = usize::try_from(length).map_err(|_| Error::Limit)?;
            let end = start.checked_add(length).ok_or(Error::Limit)?;
            Ok(bytes
                .get(start..end)
                .ok_or(Error::ContentDigestMismatch)?
                .to_vec())
        }
        BlobContent::Reference(reference) => read_reference_range(reference, offset, length),
        BlobContent::Descriptor(_) | BlobContent::Commitment => Err(Error::ContentUnavailable),
    }
}

pub fn encode_ovb2(blob: &Blob) -> Result<Vec<u8>> {
    encode_ovb2_bounded(blob, u64::MAX)
}

pub fn decode_ovb2(bytes: &[u8]) -> Result<Blob> {
    ContextValue::decode(bytes, ValueFormat::Ovb2)?.blob()
}

/// Projects one stored ROV-3 Blob row to payload-free metadata. The descriptor
/// carries length, SHA-256 and annotation directly, so listings need neither a
/// format context nor a content read; `hydrated` is always false here.
pub fn decode_rov3_blob_metadata(bytes: &[u8]) -> Result<BlobMetadata> {
    let mut reader = Reader::new(bytes);
    let raw = reader.raw(0)?;
    if reader.at != bytes.len() {
        return Err(Error::TrailingBytes);
    }
    blob_metadata_from_raw(&raw)
}

/// Projects an already-decoded ROV-3 Blob value to payload-free metadata: the
/// same projection `decode_rov3_blob_metadata` applies to stored bytes. Callers
/// that hold a decoded field value use this rather than re-encoding it.
pub fn blob_metadata_from_raw(raw: &Raw) -> Result<BlobMetadata> {
    let Raw::Tag(ROV3_BLOB_TAG, payload) = raw else {
        return Err(Error::InvalidProfile);
    };
    let fields = array(payload)?;
    let [
        length,
        sha,
        Raw::Text(media_type),
        suffix,
        Raw::Bytes(descriptor),
    ] = fields.as_slice()
    else {
        return Err(Error::InvalidTag);
    };
    let identity = ContentIdentity::new(int_u64(length)?, bytes32(sha)?)?;
    let suffix = raw_suffix(suffix)?;
    let annotation = MimeRegistry::mime1().canonical_annotation(media_type, suffix.as_deref())?;
    Ok(BlobMetadata {
        media_type: annotation.media_type().to_owned(),
        suffix: annotation.suffix().map(str::to_owned),
        length: identity.length(),
        sha256: identity.sha256(),
        descriptor_oid: Some(NativeOid::from_bytes(descriptor)?),
        hydrated: false,
    })
}

/// Listing filter over payload-free Blob metadata. Unset fields match every
/// row. The media type is compared exactly against the canonical MIME-1 form
/// that `BlobMetadata::media_type` reports.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BlobMetadataFilter {
    media_type: Option<String>,
    excluded_media_types: Vec<String>,
    suffix: Option<String>,
    suffixes: Vec<String>,
    min_length: Option<u64>,
    max_length: Option<u64>,
}

impl BlobMetadataFilter {
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_media_type(mut self, media_type: impl Into<String>) -> Self {
        self.media_type = Some(media_type.into());
        self
    }

    #[must_use]
    pub fn without_media_type(mut self, media_type: impl Into<String>) -> Self {
        self.excluded_media_types.push(media_type.into());
        self
    }

    /// Selects rows whose annotation binds exactly this suffix hint. The
    /// suffix is part of the stored annotation coordinate, so it is answered
    /// from the row's own tuple like the media type and never from the
    /// payload's bytes.
    #[must_use]
    pub fn with_suffix(mut self, suffix: impl Into<String>) -> Self {
        self.suffix = Some(suffix.into());
        self
    }

    /// Selects rows whose annotation binds one of these suffix hints. An
    /// unset suffix on a row never matches a named hint, because "no suffix"
    /// is the absence of that coordinate rather than a suffix value.
    #[must_use]
    pub fn with_any_suffix(
        mut self,
        suffixes: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.suffixes = suffixes.into_iter().map(Into::into).collect();
        self
    }

    #[must_use]
    pub const fn with_min_length(mut self, bytes: u64) -> Self {
        self.min_length = Some(bytes);
        self
    }

    #[must_use]
    pub const fn with_max_length(mut self, bytes: u64) -> Self {
        self.max_length = Some(bytes);
        self
    }

    /// The coordinate names this predicate evaluates, in the order the plan
    /// reports them. Every named coordinate is bound by the stored ROV-3
    /// annotation, so evaluating the predicate reads no payload byte.
    pub const fn coordinates(&self) -> &'static [&'static str] {
        &["kind", "suffix", "length"]
    }

    pub fn matches(&self, metadata: &BlobMetadata) -> bool {
        self.media_type
            .as_deref()
            .is_none_or(|media_type| metadata.media_type() == media_type)
            && self
                .excluded_media_types
                .iter()
                .all(|media_type| metadata.media_type() != media_type)
            && self.suffix.as_deref().is_none_or(|suffix| {
                metadata
                    .suffix()
                    .is_some_and(|candidate| candidate == suffix)
            })
            && (self.suffixes.is_empty()
                || self.suffixes.iter().any(|suffix| {
                    metadata
                        .suffix()
                        .is_some_and(|candidate| candidate == suffix)
                }))
            && self.min_length.is_none_or(|min| metadata.length() >= min)
            && self.max_length.is_none_or(|max| metadata.length() <= max)
    }
}

/// Orders payload-free Blob metadata for listings over mixed media types:
/// media type first, then length, then digest, so equal keys stay stable.
pub fn compare_blob_metadata(left: &BlobMetadata, right: &BlobMetadata) -> Ordering {
    left.media_type()
        .cmp(right.media_type())
        .then_with(|| left.length().cmp(&right.length()))
        .then_with(|| left.sha256().cmp(&right.sha256()))
}

pub fn encode_rov3(blob: &Blob) -> Result<Vec<u8>> {
    blob.to_context_value(ValueFormat::Rov3)?.encode()
}

pub fn decode_rov3(bytes: &[u8]) -> Result<Blob> {
    ContextValue::decode(bytes, ValueFormat::Rov3)?.blob()
}

pub fn decode_rov3_in_context(bytes: &[u8], context: &Format3Context) -> Result<Blob> {
    ContextValue::decode_in_context(bytes, ValueFormat::Rov3, context)?.blob()
}

pub fn encode_sov3(blob: &Blob) -> Result<Vec<u8>> {
    blob.to_context_value(ValueFormat::Sov3)?.encode()
}

pub fn decode_sov3(bytes: &[u8]) -> Result<Blob> {
    ContextValue::decode(bytes, ValueFormat::Sov3)?.blob()
}

pub fn decode_sov3_in_context(bytes: &[u8], context: &Format3Context) -> Result<Blob> {
    ContextValue::decode_in_context(bytes, ValueFormat::Sov3, context)?.blob()
}

impl Blob {
    pub(crate) fn to_raw(&self, format: ValueFormat) -> Result<Raw> {
        match format {
            ValueFormat::Ovb1 => Err(Error::InvalidProfile),
            ValueFormat::Ovb2 => {
                let bytes = self.read_to_end()?;
                Ok(tag(
                    OVB2_BLOB_TAG,
                    Raw::Array(vec![
                        Raw::Bytes(bytes),
                        Raw::Text(self.media_type().to_owned()),
                        self.suffix()
                            .map(|value| Raw::Text(value.to_owned()))
                            .unwrap_or(Raw::Null),
                    ]),
                ))
            }
            ValueFormat::Rov3 => {
                let descriptor_oid = self.descriptor_oid().ok_or(Error::MissingDescriptor)?;
                Ok(tag(
                    ROV3_BLOB_TAG,
                    Raw::Array(vec![
                        Raw::Int(BigInt::from(self.length())),
                        Raw::Bytes(self.identity.sha256().to_vec()),
                        Raw::Text(self.media_type().to_owned()),
                        self.suffix()
                            .map(|value| Raw::Text(value.to_owned()))
                            .unwrap_or(Raw::Null),
                        Raw::Bytes(descriptor_oid.as_bytes().to_vec()),
                    ]),
                ))
            }
            ValueFormat::Sov3 => {
                if self.context.is_none() {
                    return Err(Error::InvalidContext);
                }
                Ok(tag(
                    SOV3_BLOB_TAG,
                    Raw::Array(vec![
                        Raw::Int(BigInt::from(self.length())),
                        Raw::Bytes(self.identity.sha256().to_vec()),
                        Raw::Text(self.media_type().to_owned()),
                        self.suffix()
                            .map(|value| Raw::Text(value.to_owned()))
                            .unwrap_or(Raw::Null),
                    ]),
                ))
            }
        }
    }
}

pub(crate) fn decode_blob_raw(
    raw: &Raw,
    format: ValueFormat,
    context: Option<&Format3Context>,
) -> Result<Blob> {
    match (format, raw) {
        (ValueFormat::Ovb2, Raw::Tag(OVB2_BLOB_TAG, payload)) => {
            let fields = array(payload)?;
            let [Raw::Bytes(bytes), Raw::Text(media_type), suffix] = fields.as_slice() else {
                return Err(Error::InvalidTag);
            };
            let suffix = raw_suffix(suffix)?;
            Blob::from_bytes_with_annotation(bytes.clone(), media_type, suffix.as_deref())
        }
        (ValueFormat::Rov3, Raw::Tag(ROV3_BLOB_TAG, payload)) => {
            let fields = array(payload)?;
            let [
                length,
                sha,
                Raw::Text(media_type),
                suffix,
                Raw::Bytes(descriptor),
            ] = fields.as_slice()
            else {
                return Err(Error::InvalidTag);
            };
            let identity = ContentIdentity::new(int_u64(length)?, bytes32(sha)?)?;
            let context = context.ok_or(Error::InvalidContext)?;
            Blob::from_descriptor_in_context(
                identity,
                context,
                NativeOid::from_bytes(descriptor)?,
                media_type,
                raw_suffix(suffix)?.as_deref(),
            )
        }
        (ValueFormat::Sov3, Raw::Tag(SOV3_BLOB_TAG, payload)) => {
            let fields = array(payload)?;
            let [length, sha, Raw::Text(media_type), suffix] = fields.as_slice() else {
                return Err(Error::InvalidTag);
            };
            let context = context.ok_or(Error::InvalidContext)?;
            Blob::from_semantic_commitment_in_context(
                ContentIdentity::new(int_u64(length)?, bytes32(sha)?)?,
                context,
                media_type,
                raw_suffix(suffix)?.as_deref(),
            )
        }
        _ => Err(Error::InvalidValue),
    }
}

pub(crate) fn raw_suffix(raw: &Raw) -> Result<Option<String>> {
    match raw {
        Raw::Null => Ok(None),
        Raw::Text(value) => Ok(Some(value.clone())),
        _ => Err(Error::InvalidTag),
    }
}

/// Decides an inline OVB-2 Blob's annotation and content identity from its raw
/// wire value without copying the content. It runs the same checks that
/// building the Blob from that value runs, so it fails exactly where the Blob
/// would fail to build.
pub fn ovb2_blob_fingerprint(raw: &Raw) -> Result<(MediaAnnotation, ContentIdentity)> {
    // The checks `ContextValue::new(Ovb2, raw)` runs, then the decode
    // `ContextValue::blob` runs, without copying the content.
    validate_context_raw(raw, 0, ValueFormat::Ovb2, None, MimeRegistry::mime1())?;
    let Raw::Tag(OVB2_BLOB_TAG, payload) = raw else {
        return Err(Error::InvalidValue);
    };
    let [Raw::Bytes(bytes), Raw::Text(media_type), suffix] = array(payload)?.as_slice() else {
        return Err(Error::InvalidTag);
    };
    let suffix = raw_suffix(suffix)?;
    let annotation = MimeRegistry::mime1().annotation(media_type, suffix.as_deref())?;
    Ok((annotation, ContentIdentity::from_bytes(bytes)))
}
