//! The format-3 value profiles, their pinned context and profile validation.

use crate::blob::{
    Blob, BlobResolver, ContentIdentity, ContentReference, MAX_BLOB_LENGTH, MAX_BLOB_READ,
    NativeOid, decode_blob_raw, raw_suffix,
};
use crate::cbor::{Reader, encode_raw, head, write_raw};
use crate::error::Error;
use crate::identity::bytes32;
use crate::media::MimeRegistry;
use crate::raw::{Raw, Value, array, int_u64};
use crate::snapshot::{GitHash, Snapshot, snapshot_database};
use crate::tags::{OVB2_BLOB_TAG, ROV3_BLOB_TAG, ROV3_OVERFLOW_TAG, SOV3_BLOB_TAG};
use crate::validation::{validate_raw, validate_tag};
use crate::{MAX_DEPTH, Result};
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering as AtomicOrdering;

/// The explicit value profiles used by the final format-3 writer.
///
/// [`Value`] and [`OvbCodec`] remain the frozen OVB-1 compatibility surface.
/// New tags are accepted only through [`ContextValue`] with the matching
/// profile; a caller cannot accidentally serialize an ROV-3 or SOV-3 value
/// through the old wire codec.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ValueFormat {
    Ovb1,
    Ovb2,
    Rov3,
    Sov3,
}

pub type ValueContext = ValueFormat;

pub type ValueProfile = ValueFormat;

impl ValueFormat {
    pub const OVB1: Self = Self::Ovb1;
    pub const OVB2: Self = Self::Ovb2;
    pub const ROV3: Self = Self::Rov3;
    pub const SOV3: Self = Self::Sov3;

    pub const fn name(self) -> &'static str {
        match self {
            Self::Ovb1 => "OVB-1",
            Self::Ovb2 => "OVB-2",
            Self::Rov3 => "ROV-3",
            Self::Sov3 => "SOV-3",
        }
    }
}

pub const OVB2_VERSION: &str = "OVB-2";

pub const ROV3_VERSION: &str = "ROV-3";

pub const SOV3_VERSION: &str = "SOV-3";

/// The single published format-3 coordinate set. It is intentionally closed:
/// graph/storage implementations cannot silently mix a different writer
/// profile into a value context.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct Format3Coordinates {
    graph: &'static str,
    rows: &'static str,
    blob_storage: &'static str,
    stored_values: &'static str,
    semantic_values: &'static str,
    portable_values: &'static str,
    media_policy: &'static str,
}

impl Format3Coordinates {
    pub const fn published() -> Self {
        Self {
            graph: "OGS-1",
            rows: "ORP-1",
            blob_storage: "OGB-2",
            stored_values: "ROV-3",
            semantic_values: "SOV-3",
            portable_values: "OVB-2",
            media_policy: "MIME-1",
        }
    }

    pub const fn graph(self) -> &'static str {
        self.graph
    }

    pub const fn rows(self) -> &'static str {
        self.rows
    }

    pub const fn blob_storage(self) -> &'static str {
        self.blob_storage
    }

    pub const fn stored_values(self) -> &'static str {
        self.stored_values
    }

    pub const fn semantic_values(self) -> &'static str {
        self.semantic_values
    }

    pub const fn portable_values(self) -> &'static str {
        self.portable_values
    }

    pub const fn media_policy(self) -> &'static str {
        self.media_policy
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[repr(u8)]
pub enum AvailabilityState {
    Available = 0,
    Offline = 1,
    Incomplete = 2,
    Expired = 3,
}

impl AvailabilityState {
    fn from_byte(value: u8) -> Self {
        match value {
            0 => Self::Available,
            1 => Self::Offline,
            2 => Self::Incomplete,
            _ => Self::Expired,
        }
    }
}

pub type ContentAvailability = AvailabilityState;

struct OwnerState {
    cancelled: AtomicBool,
    expired: AtomicBool,
}

/// Owner/runtime lifetime shared by every lazy value resolved through a
/// format-3 context. It is a capability marker, not a serializable handle.
#[derive(Clone)]
pub struct OwnerLifetime {
    id: [u8; 16],
    state: Arc<OwnerState>,
}

impl fmt::Debug for OwnerLifetime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OwnerLifetime")
            .field("id", &self.id)
            .field("cancelled", &self.is_cancelled())
            .field("expired", &self.is_expired())
            .finish()
    }
}

impl OwnerLifetime {
    pub fn new(id: [u8; 16]) -> Self {
        Self {
            id,
            state: Arc::new(OwnerState {
                cancelled: AtomicBool::new(false),
                expired: AtomicBool::new(false),
            }),
        }
    }

    pub const fn id(&self) -> [u8; 16] {
        self.id
    }

    pub fn cancel(&self) {
        self.state.cancelled.store(true, AtomicOrdering::Release);
    }

    pub fn expire(&self) {
        self.state.expired.store(true, AtomicOrdering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(AtomicOrdering::Acquire)
    }

    pub fn is_expired(&self) -> bool {
        self.state.expired.load(AtomicOrdering::Acquire)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Format3Quota {
    max_read_bytes: u64,
    max_capture_bytes: u64,
}

impl Format3Quota {
    pub fn new(max_read_bytes: u64, max_capture_bytes: u64) -> Result<Self> {
        if max_read_bytes == 0
            || max_read_bytes > MAX_BLOB_READ
            || max_capture_bytes >= MAX_BLOB_LENGTH
        {
            return Err(Error::InvalidContext);
        }
        Ok(Self {
            max_read_bytes,
            max_capture_bytes,
        })
    }

    pub const fn max_read_bytes(self) -> u64 {
        self.max_read_bytes
    }

    pub const fn max_capture_bytes(self) -> u64 {
        self.max_capture_bytes
    }
}

pub(crate) struct Format3ContextInner {
    pub(crate) coordinates: Format3Coordinates,
    pub(crate) git_oid_algorithm: GitHash,
    database_uuid: [u8; 16],
    pin: Snapshot,
    schema_root: NativeOid,
    store_root: NativeOid,
    availability: AtomicU8,
    owner: OwnerLifetime,
    quota: Format3Quota,
    mime_registry: MimeRegistry,
    resolver: Arc<dyn BlobResolver>,
}

/// Shared, capability-bound context for all format-3 values.
///
/// The fields are private on purpose. A raw Git OID, content digest, or
/// portable tag cannot manufacture authority to read bytes. References are
/// minted only by this context, which retains database/pin/schema/store roots,
/// availability, owner lifetime, cancellation, quota and the MIME registry.
#[derive(Clone)]
pub struct Format3Context {
    pub(crate) inner: Arc<Format3ContextInner>,
}

impl fmt::Debug for Format3Context {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Format3Context")
            .field("coordinates", &self.inner.coordinates)
            .field("git_oid_algorithm", &self.inner.git_oid_algorithm)
            .field("database_uuid", &self.inner.database_uuid)
            .field("pin", &self.inner.pin)
            .field("schema_root", &self.inner.schema_root)
            .field("store_root", &self.inner.store_root)
            .field("availability", &self.availability())
            .field("owner", &self.inner.owner)
            .field("quota", &self.inner.quota)
            .field("resolver", &true)
            .finish()
    }
}

impl Format3Context {
    /// Constructs a format-3 context from metadata already verified by its owning
    /// repository/runtime. The caller must bind the pin and native roots to the
    /// same persisted snapshot and provide a resolver that enforces that pin;
    /// untrusted OIDs or user-selected roots must not be used here.
    /// The nine values are one persisted format-3 record, and every caller reads
    /// them from the same stored metadata, so they stay positional here rather
    /// than moving into a wrapper struct that only this constructor would use.
    #[allow(clippy::too_many_arguments)]
    pub fn from_persisted(
        database_uuid: [u8; 16],
        pin: Snapshot,
        git_oid_algorithm: GitHash,
        schema_root: NativeOid,
        store_root: NativeOid,
        availability: AvailabilityState,
        owner: OwnerLifetime,
        quota: Format3Quota,
        resolver: Arc<dyn BlobResolver>,
    ) -> Result<Self> {
        validate_format3_metadata(
            database_uuid,
            &pin,
            git_oid_algorithm,
            &schema_root,
            &store_root,
        )?;
        Ok(Self {
            inner: Arc::new(Format3ContextInner {
                coordinates: Format3Coordinates::published(),
                git_oid_algorithm,
                database_uuid,
                pin,
                schema_root,
                store_root,
                availability: AtomicU8::new(availability as u8),
                owner,
                quota,
                mime_registry: MimeRegistry::mime1(),
                resolver,
            }),
        })
    }

    pub fn coordinates(&self) -> Format3Coordinates {
        self.inner.coordinates
    }

    pub fn git_oid_algorithm(&self) -> GitHash {
        self.inner.git_oid_algorithm
    }

    pub fn database_uuid(&self) -> [u8; 16] {
        self.inner.database_uuid
    }

    pub fn pin(&self) -> &Snapshot {
        &self.inner.pin
    }

    pub fn schema_root(&self) -> &NativeOid {
        &self.inner.schema_root
    }

    pub fn store_root(&self) -> &NativeOid {
        &self.inner.store_root
    }

    pub fn availability(&self) -> AvailabilityState {
        AvailabilityState::from_byte(self.inner.availability.load(AtomicOrdering::Acquire))
    }

    pub fn owner(&self) -> OwnerLifetime {
        self.inner.owner.clone()
    }

    pub fn quota(&self) -> Format3Quota {
        self.inner.quota
    }

    pub fn mime_registry(&self) -> MimeRegistry {
        self.inner.mime_registry
    }

    pub fn cancel(&self) {
        self.inner.owner.cancel();
    }

    pub fn expire(&self) {
        self.inner.owner.expire();
        self.inner
            .availability
            .store(AvailabilityState::Expired as u8, AtomicOrdering::Release);
    }

    pub fn reference(
        &self,
        identity: ContentIdentity,
        descriptor_oid: NativeOid,
    ) -> Result<ContentReference> {
        ContentReference::from_context(identity, descriptor_oid, Arc::clone(&self.inner))
    }
}

fn validate_format3_metadata(
    database_uuid: [u8; 16],
    pin: &Snapshot,
    git_oid_algorithm: GitHash,
    schema_root: &NativeOid,
    store_root: &NativeOid,
) -> Result<()> {
    validate_snapshot_pin(pin)?;
    if snapshot_database(pin) != database_uuid
        || !oid_matches_algorithm(schema_root, git_oid_algorithm)
        || !oid_matches_algorithm(store_root, git_oid_algorithm)
    {
        return Err(Error::InvalidContext);
    }
    if let Snapshot::Commit { algorithm, oid, .. } = pin
        && (*algorithm != git_oid_algorithm || oid.len() != git_oid_algorithm.oid_width())
    {
        return Err(Error::InvalidContext);
    }
    Ok(())
}

fn validate_snapshot_pin(snapshot: &Snapshot) -> Result<()> {
    match snapshot {
        Snapshot::Cwd {
            database,
            runtime,
            generation,
            ..
        } => {
            let expected = Snapshot::cwd(*database, *runtime, generation.clone())
                .map_err(|_| Error::InvalidContext)?;
            if &expected != snapshot {
                return Err(Error::InvalidContext);
            }
        }
        Snapshot::Commit { algorithm, oid, .. } if oid.len() != algorithm.oid_width() => {
            return Err(Error::InvalidContext);
        }
        Snapshot::Commit { .. } => {}
    }
    Ok(())
}

impl Format3ContextInner {
    pub(crate) fn ensure_live(&self) -> Result<()> {
        if self.owner.is_expired() || self.availability() == AvailabilityState::Expired {
            return Err(Error::OwnerExpired);
        }
        if self.owner.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if self.availability() != AvailabilityState::Available {
            return Err(Error::ContentUnavailable);
        }
        Ok(())
    }

    fn availability(&self) -> AvailabilityState {
        AvailabilityState::from_byte(self.availability.load(AtomicOrdering::Acquire))
    }

    pub(crate) fn read_range(
        &self,
        reference: &ContentReference,
        offset: u64,
        length: u64,
    ) -> Result<Vec<u8>> {
        self.ensure_live()?;
        if length > self.quota.max_read_bytes {
            return Err(Error::QuotaExceeded);
        }
        self.resolver.read_range(reference, offset, length)
    }
}

pub(crate) fn oid_matches_algorithm(oid: &NativeOid, algorithm: GitHash) -> bool {
    oid.len() == algorithm.oid_width()
}

/// A validated value paired with its explicit value profile.
#[derive(Clone)]
pub struct ContextValue {
    pub(crate) format: ValueFormat,
    raw: Raw,
    context: Option<Format3Context>,
}

impl PartialEq for ContextValue {
    fn eq(&self, other: &Self) -> bool {
        self.format == other.format && self.raw == other.raw
    }
}

impl Eq for ContextValue {}

impl fmt::Debug for ContextValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContextValue")
            .field("format", &self.format)
            .field("raw", &self.raw)
            .field("context_bound", &self.context.is_some())
            .finish()
    }
}

impl ContextValue {
    pub fn new(format: ValueFormat, raw: Raw) -> Result<Self> {
        if matches!(format, ValueFormat::Rov3 | ValueFormat::Sov3) {
            return Err(Error::InvalidContext);
        }
        Self::new_with_context(format, raw, None)
    }

    pub fn from_raw(raw: Raw, format: ValueFormat) -> Result<Self> {
        Self::new(format, raw)
    }

    pub fn from_value(value: Value) -> Self {
        Self {
            format: ValueFormat::Ovb1,
            raw: value.0,
            context: None,
        }
    }

    pub fn from_blob(blob: &Blob, format: ValueFormat) -> Result<Self> {
        let context = if matches!(format, ValueFormat::Rov3 | ValueFormat::Sov3) {
            Some(blob.context.clone().ok_or(Error::InvalidContext)?)
        } else {
            None
        };
        Self::new_with_context(format, blob.to_raw(format)?, context)
    }

    pub fn new_in_context(context: &Format3Context, format: ValueFormat, raw: Raw) -> Result<Self> {
        Self::new_with_context(format, raw, Some(context.clone()))
    }

    fn new_with_context(
        format: ValueFormat,
        raw: Raw,
        context: Option<Format3Context>,
    ) -> Result<Self> {
        if matches!(format, ValueFormat::Rov3 | ValueFormat::Sov3) && context.is_none() {
            return Err(Error::InvalidContext);
        }
        let oid_algorithm = context.as_ref().map(Format3Context::git_oid_algorithm);
        // The annotation profile travels with the same immutable context as
        // the object-hash algorithm; codecs must not select a host MIME table.
        let mime_registry = context
            .as_ref()
            .map(Format3Context::mime_registry)
            .unwrap_or_else(MimeRegistry::mime1);
        if format == ValueFormat::Ovb1 {
            Value::new(raw.clone())?;
        } else {
            validate_context_raw(&raw, 0, format, oid_algorithm, mime_registry)?;
        }
        Ok(Self {
            format,
            raw,
            context,
        })
    }

    pub const fn format(&self) -> ValueFormat {
        self.format
    }

    pub fn raw(&self) -> &Raw {
        &self.raw
    }

    pub fn context(&self) -> Option<&Format3Context> {
        self.context.as_ref()
    }

    pub fn into_raw(self) -> Raw {
        self.raw
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        let oid_algorithm = self.context.as_ref().map(Format3Context::git_oid_algorithm);
        let mime_registry = self
            .context
            .as_ref()
            .map(Format3Context::mime_registry)
            .unwrap_or_else(MimeRegistry::mime1);
        encode_context_raw(&self.raw, self.format, oid_algorithm, mime_registry)
    }

    pub fn decode(bytes: &[u8], format: ValueFormat) -> Result<Self> {
        if format == ValueFormat::Ovb1 {
            return Ok(Self::from_value(Value::decode(bytes)?));
        }
        if matches!(format, ValueFormat::Rov3 | ValueFormat::Sov3) {
            return Err(Error::InvalidContext);
        }
        Self::decode_bytes(bytes, format, None)
    }

    fn decode_bytes(
        bytes: &[u8],
        format: ValueFormat,
        context: Option<Format3Context>,
    ) -> Result<Self> {
        let mut reader = Reader::new(bytes);
        let raw = reader.raw(0)?;
        if reader.at != bytes.len() {
            return Err(Error::TrailingBytes);
        }
        let value = Self::new_with_context(format, raw, context)?;
        if value.encode()? != bytes {
            return Err(Error::NonCanonical);
        }
        Ok(value)
    }

    pub fn decode_in_context(
        bytes: &[u8],
        format: ValueFormat,
        context: &Format3Context,
    ) -> Result<Self> {
        Self::decode_bytes(bytes, format, Some(context.clone()))
    }

    pub fn blob(&self) -> Result<Blob> {
        decode_blob_raw(&self.raw, self.format, self.context.as_ref())
    }

    /// Return this annotated Blob with canonical MIME-1 annotations while
    /// preserving its encoded content commitment and resolution context.
    ///
    /// This metadata-only rewrite does not hydrate OVB-2 bytes or recompute
    /// the `(length, SHA-256)` identity carried by ROV-3/SOV-3.
    pub fn with_blob_annotation(&self, media_type: &str, suffix: Option<&str>) -> Result<Self> {
        let mime_registry = self
            .context
            .as_ref()
            .map(Format3Context::mime_registry)
            .unwrap_or_else(MimeRegistry::mime1);
        let annotation = mime_registry.canonical_annotation(media_type, suffix)?;
        let (tag_number, media_index, suffix_index, expected_len) = match self.format {
            ValueFormat::Ovb2 => (OVB2_BLOB_TAG, 1, 2, 3),
            ValueFormat::Rov3 => (ROV3_BLOB_TAG, 2, 3, 5),
            ValueFormat::Sov3 => (SOV3_BLOB_TAG, 2, 3, 4),
            ValueFormat::Ovb1 => return Err(Error::InvalidProfile),
        };
        let mut raw = self.raw.clone();
        let Raw::Tag(actual_tag, payload) = &mut raw else {
            return Err(Error::InvalidValue);
        };
        if *actual_tag != tag_number {
            return Err(Error::InvalidValue);
        }
        let Raw::Array(fields) = payload.as_mut() else {
            return Err(Error::InvalidTag);
        };
        if fields.len() != expected_len {
            return Err(Error::InvalidTag);
        }
        fields[media_index] = Raw::Text(annotation.media_type().to_owned());
        fields[suffix_index] = annotation
            .suffix()
            .map(|value| Raw::Text(value.to_owned()))
            .unwrap_or(Raw::Null);
        Self::new_with_context(self.format, raw, self.context.clone())
    }
}

pub fn encode_with_profile(value: &ContextValue) -> Result<Vec<u8>> {
    value.encode()
}

pub fn decode_with_profile(bytes: &[u8], format: ValueFormat) -> Result<ContextValue> {
    ContextValue::decode(bytes, format)
}

pub(crate) fn validate_context_raw(
    value: &Raw,
    depth: usize,
    format: ValueFormat,
    oid_algorithm: Option<GitHash>,
    mime_registry: MimeRegistry,
) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(Error::Limit);
    }
    match value {
        Raw::Array(values) => {
            for value in values {
                validate_context_raw(value, depth + 1, format, oid_algorithm, mime_registry)?;
            }
        }
        Raw::Map(entries) => {
            let mut previous = None;
            for (key, value) in entries {
                validate_context_raw(key, depth + 1, format, oid_algorithm, mime_registry)?;
                validate_context_raw(value, depth + 1, format, oid_algorithm, mime_registry)?;
                let encoded = encode_context_raw(key, format, oid_algorithm, mime_registry)?;
                if previous
                    .as_ref()
                    .is_some_and(|prior: &Vec<u8>| encoded <= *prior)
                {
                    return Err(Error::DuplicateOrUnorderedMapKey);
                }
                previous = Some(encoded);
            }
        }
        Raw::Tag(number, payload)
            if matches!(
                *number,
                OVB2_BLOB_TAG | ROV3_BLOB_TAG | SOV3_BLOB_TAG | ROV3_OVERFLOW_TAG
            ) =>
        {
            validate_context_tag(*number, payload, format, oid_algorithm, mime_registry)?;
        }
        Raw::Tag(number, payload) => {
            if *number == 0 {
                return Err(Error::ProtectedValue);
            }
            validate_tag(*number, payload)?;
            validate_context_raw(payload, depth + 1, format, oid_algorithm, mime_registry)?;
        }
        _ => validate_raw(value, depth)?,
    }
    Ok(())
}

fn validate_context_tag(
    number: u64,
    payload: &Raw,
    format: ValueFormat,
    oid_algorithm: Option<GitHash>,
    mime_registry: MimeRegistry,
) -> Result<()> {
    let allowed = match number {
        OVB2_BLOB_TAG => format == ValueFormat::Ovb2,
        ROV3_BLOB_TAG | ROV3_OVERFLOW_TAG => format == ValueFormat::Rov3,
        SOV3_BLOB_TAG => format == ValueFormat::Sov3,
        _ => false,
    };
    if !allowed {
        return Err(Error::InvalidProfile);
    }
    match number {
        OVB2_BLOB_TAG => {
            let [Raw::Bytes(_), Raw::Text(media_type), suffix] = array(payload)?.as_slice() else {
                return Err(Error::InvalidTag);
            };
            validate_annotation_fields(media_type, suffix, mime_registry)
        }
        ROV3_BLOB_TAG => {
            let [length, sha, Raw::Text(media_type), suffix, Raw::Bytes(oid)] =
                array(payload)?.as_slice()
            else {
                return Err(Error::InvalidTag);
            };
            validate_length(length)?;
            bytes32(sha)?;
            validate_context_oid(oid, oid_algorithm)?;
            validate_annotation_fields(media_type, suffix, mime_registry)
        }
        SOV3_BLOB_TAG => {
            let [length, sha, Raw::Text(media_type), suffix] = array(payload)?.as_slice() else {
                return Err(Error::InvalidTag);
            };
            validate_length(length)?;
            bytes32(sha)?;
            validate_annotation_fields(media_type, suffix, mime_registry)
        }
        ROV3_OVERFLOW_TAG => {
            let [Raw::Bytes(oid)] = array(payload)?.as_slice() else {
                return Err(Error::InvalidTag);
            };
            validate_context_oid(oid, oid_algorithm)?;
            Ok(())
        }
        _ => Err(Error::InvalidTag),
    }
}

fn validate_context_oid(oid: &[u8], algorithm: Option<GitHash>) -> Result<()> {
    let oid = NativeOid::from_bytes(oid)?;
    let algorithm = algorithm.ok_or(Error::InvalidContext)?;
    if !oid_matches_algorithm(&oid, algorithm) {
        return Err(Error::InvalidOid);
    }
    Ok(())
}

fn validate_length(raw: &Raw) -> Result<u64> {
    let length = int_u64(raw)?;
    if length >= MAX_BLOB_LENGTH {
        return Err(Error::Limit);
    }
    Ok(length)
}

pub(crate) fn validate_annotation_fields(
    media_type: &str,
    suffix: &Raw,
    mime_registry: MimeRegistry,
) -> Result<()> {
    let suffix = raw_suffix(suffix)?;
    let annotation = mime_registry.annotation(media_type, suffix.as_deref())?;
    if annotation.media_type() != media_type || annotation.suffix() != suffix.as_deref() {
        return Err(Error::NonCanonical);
    }
    Ok(())
}

pub(crate) fn encode_context_raw(
    value: &Raw,
    format: ValueFormat,
    oid_algorithm: Option<GitHash>,
    mime_registry: MimeRegistry,
) -> Result<Vec<u8>> {
    if format == ValueFormat::Ovb1 {
        return encode_raw(value);
    }
    validate_context_raw(value, 0, format, oid_algorithm, mime_registry)?;
    let mut output = Vec::new();
    write_context_raw(value, format, oid_algorithm, mime_registry, &mut output)?;
    Ok(output)
}

fn write_context_raw(
    value: &Raw,
    format: ValueFormat,
    oid_algorithm: Option<GitHash>,
    mime_registry: MimeRegistry,
    output: &mut Vec<u8>,
) -> Result<()> {
    match value {
        Raw::Map(entries) => {
            let mut encoded = Vec::with_capacity(entries.len());
            for (key, value) in entries {
                encoded.push((
                    encode_context_raw(key, format, oid_algorithm, mime_registry)?,
                    key,
                    value,
                ));
            }
            encoded.sort_by(|left, right| left.0.cmp(&right.0));
            if encoded.windows(2).any(|pair| pair[0].0 == pair[1].0) {
                return Err(Error::DuplicateOrUnorderedMapKey);
            }
            head(output, 5, encoded.len() as u64);
            for (key, _, value) in encoded {
                output.extend(key);
                write_context_raw(value, format, oid_algorithm, mime_registry, output)?;
            }
            Ok(())
        }
        Raw::Array(values) => {
            head(output, 4, values.len() as u64);
            for value in values {
                write_context_raw(value, format, oid_algorithm, mime_registry, output)?;
            }
            Ok(())
        }
        Raw::Tag(number, value) => {
            head(output, 6, *number);
            write_context_raw(value, format, oid_algorithm, mime_registry, output)
        }
        _ => write_raw(value, output),
    }
}
