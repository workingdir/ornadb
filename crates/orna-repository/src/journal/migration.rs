//! Extracted from `lib.rs` along its seams; contents are moved verbatim.

use super::*;

/// Complete bounded predecessor inventory supplied with an explicit
/// format-1/2 to format-3 publication. An empty inventory is explicit evidence
/// that the migration has no persistent stream checkpoints to carry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MigrationContinuityRecord {
    predecessors: Vec<StreamCheckpointIdentityPredecessor>,
}

/// A legacy checkpoint key's format-3 successor: consumer identity, canonical
/// source bytes and optional canonical partition bytes.
type LegacySuccessor<'a> = ([u8; 32], &'a [u8], Option<&'a [u8]>);

impl MigrationContinuityRecord {
    pub fn new(
        mut predecessors: Vec<StreamCheckpointIdentityPredecessor>,
    ) -> Result<Self, RepositoryError> {
        if predecessors.len() > MAX_MIGRATION_IDENTITY_RECORDS {
            return Err(RepositoryError::InvalidFormatMigration);
        }
        predecessors.sort();
        let mut encoded_bytes = 5usize; // record version and entry count
        let mut legacy_keys = HashSet::new();
        let mut format3_keys = HashSet::new();
        let mut consumer_forward = BTreeMap::new();
        let mut consumer_reverse = BTreeMap::new();
        for predecessor in &predecessors {
            validate_migration_identity_component(&predecessor.legacy_source_identity, false)?;
            validate_migration_identity_component(
                predecessor
                    .legacy_partition_identity
                    .as_deref()
                    .unwrap_or_default(),
                predecessor.legacy_partition_identity.is_none(),
            )?;
            validate_migration_identity_component(&predecessor.format3_source_identity, false)?;
            validate_migration_identity_component(
                predecessor
                    .format3_partition_identity
                    .as_deref()
                    .unwrap_or_default(),
                predecessor.format3_partition_identity.is_none(),
            )?;
            encoded_bytes = encoded_bytes
                .checked_add(80)
                .and_then(|length| length.checked_add(predecessor.legacy_source_identity.len()))
                .and_then(|length| {
                    length.checked_add(
                        predecessor
                            .legacy_partition_identity
                            .as_ref()
                            .map_or(0, Vec::len),
                    )
                })
                .and_then(|length| length.checked_add(predecessor.format3_source_identity.len()))
                .and_then(|length| {
                    length.checked_add(
                        predecessor
                            .format3_partition_identity
                            .as_ref()
                            .map_or(0, Vec::len),
                    )
                })
                .ok_or(RepositoryError::InvalidFormatMigration)?;
            if encoded_bytes > MAX_JOURNAL_BYTES {
                return Err(RepositoryError::InvalidFormatMigration);
            }
            if !legacy_keys.insert(predecessor.legacy_key())
                || !format3_keys.insert(predecessor.format3_key())
            {
                return Err(RepositoryError::InvalidFormatMigration);
            }
            match consumer_forward.get(&predecessor.legacy_consumer_identity) {
                Some(mapped) if mapped != &predecessor.format3_consumer_identity => {
                    return Err(RepositoryError::InvalidFormatMigration);
                }
                Some(_) => {}
                None => {
                    consumer_forward.insert(
                        predecessor.legacy_consumer_identity,
                        predecessor.format3_consumer_identity,
                    );
                }
            }
            match consumer_reverse.get(&predecessor.format3_consumer_identity) {
                Some(mapped) if mapped != &predecessor.legacy_consumer_identity => {
                    return Err(RepositoryError::InvalidFormatMigration);
                }
                Some(_) => {}
                None => {
                    consumer_reverse.insert(
                        predecessor.format3_consumer_identity,
                        predecessor.legacy_consumer_identity,
                    );
                }
            }
        }
        Ok(Self { predecessors })
    }

    pub fn predecessors(&self) -> &[StreamCheckpointIdentityPredecessor] {
        &self.predecessors
    }

    /// The format-3 successor identity of one legacy checkpoint key: the
    /// consumer identity, the canonical source bytes and the optional
    /// canonical partition bytes.
    ///
    /// A resumed publisher looks up the identity the migration recorded
    /// instead of re-deriving it: re-deriving is exactly the reset this
    /// coordinate exists to prevent. The returned source and partition are the
    /// successor's canonical typed identity bytes, and `None` means the
    /// successor has no partition, which stays distinct from an encoded empty
    /// partition. A legacy key the migration never carried has no successor,
    /// so a caller cannot invent one.
    pub fn successor_for(
        &self,
        legacy_consumer_identity: [u8; 32],
        legacy_source_identity: &[u8],
        legacy_partition_identity: Option<&[u8]>,
    ) -> Option<LegacySuccessor<'_>> {
        self.predecessors
            .iter()
            .find(|predecessor| {
                predecessor.legacy_consumer_identity == legacy_consumer_identity
                    && predecessor.legacy_source_identity == legacy_source_identity
                    && predecessor.legacy_partition_identity.as_deref() == legacy_partition_identity
            })
            .map(|predecessor| {
                (
                    predecessor.format3_consumer_identity,
                    predecessor.format3_source_identity.as_slice(),
                    predecessor.format3_partition_identity.as_deref(),
                )
            })
    }

    pub(crate) fn encode(&self, bytes: &mut Vec<u8>) -> Result<(), RepositoryError> {
        bytes.push(1); // continuity-record version
        put_u32(bytes, self.predecessors.len())?;
        for predecessor in &self.predecessors {
            bytes.extend_from_slice(&predecessor.legacy_consumer_identity);
            put_migration_identity_component(bytes, &predecessor.legacy_source_identity)?;
            put_optional_bytes(bytes, predecessor.legacy_partition_identity.as_deref())?;
            bytes.extend_from_slice(&predecessor.format3_consumer_identity);
            put_migration_identity_component(bytes, &predecessor.format3_source_identity)?;
            put_optional_bytes(bytes, predecessor.format3_partition_identity.as_deref())?;
            if bytes.len() > MAX_JOURNAL_BYTES {
                return Err(RepositoryError::InvalidPublicationJournal);
            }
        }
        Ok(())
    }

    pub(crate) fn decode(bytes: &[u8], cursor: &mut usize) -> Result<Self, RepositoryError> {
        if take_byte(bytes, cursor)? != 1 {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        let count = usize::try_from(take_u32(bytes, cursor)?)
            .map_err(|_| RepositoryError::InvalidPublicationJournal)?;
        if count > MAX_MIGRATION_IDENTITY_RECORDS {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        let mut predecessors = Vec::with_capacity(count);
        for _ in 0..count {
            let legacy_consumer_identity = take_fixed_array::<32>(bytes, cursor)?;
            let legacy_source_identity = take_migration_identity_component(bytes, cursor)?;
            let legacy_partition_identity =
                take_optional_migration_identity_component(bytes, cursor)?;
            let format3_consumer_identity = take_fixed_array::<32>(bytes, cursor)?;
            let format3_source_identity = take_migration_identity_component(bytes, cursor)?;
            let format3_partition_identity =
                take_optional_migration_identity_component(bytes, cursor)?;
            let predecessor = StreamCheckpointIdentityPredecessor::new(
                legacy_consumer_identity,
                legacy_source_identity,
                legacy_partition_identity,
                format3_consumer_identity,
                format3_source_identity,
                format3_partition_identity,
            )
            .map_err(|_| RepositoryError::InvalidPublicationJournal)?;
            if predecessors
                .last()
                .is_some_and(|previous| previous >= &predecessor)
            {
                return Err(RepositoryError::InvalidPublicationJournal);
            }
            predecessors.push(predecessor);
        }
        Self::new(predecessors).map_err(|_| RepositoryError::InvalidPublicationJournal)
    }
}

/// The reported semantic annotation coordinate of one format-1/2 to format-3
/// migration.
///
/// Legacy rows carry no MIME-1 annotation. Format 3 always annotates a Blob,
/// so the migration assigns one and must say so: the raw bytes are preserved,
/// but the annotated value identity of `entry_count` rows is not. UPGRADE-004
/// fixes the assigned media type and forbids consulting payload bytes, so this
/// record is a count of newly defaulted rows together with an explicit
/// confirmation that no payload was read. Both are required for publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MigrationAnnotationDefaults {
    entry_count: u32,
    payload_inspected: bool,
}

impl MigrationAnnotationDefaults {
    /// Reports `entry_count` legacy rows that take the fixed default
    /// annotation during an explicit migration.
    pub fn new(entry_count: u32) -> Result<Self, RepositoryError> {
        if usize::try_from(entry_count).map_err(|_| RepositoryError::InvalidFormatMigration)?
            > MAX_MIGRATION_ANNOTATION_DEFAULTS
        {
            return Err(RepositoryError::InvalidFormatMigration);
        }
        Ok(Self {
            entry_count,
            payload_inspected: false,
        })
    }

    /// The number of legacy rows whose annotated value identity changes.
    pub const fn entry_count(self) -> u32 {
        self.entry_count
    }

    /// Whether the migration's defaults were derived from payload bytes.
    /// A conforming migration derives them from metadata alone and reports
    /// `false`; a `true` value is refused rather than reported.
    pub const fn payload_inspected(self) -> bool {
        self.payload_inspected
    }

    /// Derives the reported transition from the legacy rows the migration
    /// actually decoded.
    ///
    /// A row recorded without an annotation takes the fixed UPGRADE-004
    /// default and is counted as a changed annotated value identity. A row
    /// whose recorded legacy annotation is not already a representable
    /// canonical MIME-1 value fails the migration: ORNA-UPGRADE-010 forbids
    /// silently downcasting an unsupported annotated value to bytes. No
    /// payload byte is read here; the inventory is decoded metadata.
    pub fn from_legacy_inventory(
        legacy_rows: &[LegacyAnnotationCandidate],
    ) -> Result<Self, RepositoryError> {
        let mut entry_count = 0u32;
        for row in legacy_rows {
            match row.annotation_media_type() {
                Some(media_type) => {
                    orna_value::MediaAnnotation::new(media_type, row.suffix())
                        .map_err(|_| RepositoryError::InvalidFormatMigration)?;
                }
                None => entry_count = entry_count.saturating_add(1),
            }
        }
        Self::new(entry_count)
    }

    pub(crate) fn encode(self, bytes: &mut Vec<u8>) -> Result<(), RepositoryError> {
        bytes.push(1); // annotation-defaults record version
        put_u32(
            bytes,
            usize::try_from(self.entry_count)
                .map_err(|_| RepositoryError::InvalidFormatMigration)?,
        )?;
        bytes.push(u8::from(self.payload_inspected));
        if bytes.len() > MAX_JOURNAL_BYTES {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        Ok(())
    }

    pub(crate) fn decode(bytes: &[u8], cursor: &mut usize) -> Result<Self, RepositoryError> {
        if take_byte(bytes, cursor)? != 1 {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        let entry_count = take_u32(bytes, cursor)?;
        let payload_inspected = match take_byte(bytes, cursor)? {
            0 => false,
            1 => true,
            _ => return Err(RepositoryError::InvalidPublicationJournal),
        };
        let defaults =
            Self::new(entry_count).map_err(|_| RepositoryError::InvalidPublicationJournal)?;
        if payload_inspected != defaults.payload_inspected {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        Ok(defaults)
    }
}

/// One legacy authoritative row's recorded annotation, as decoded from its
/// original format-1/2 value profile. This is metadata only: payload bytes are
/// never read to build it.
///
/// A legacy row that already carries a representable annotation keeps it and
/// is not part of the transition; a row recorded without one takes the fixed
/// UPGRADE-004 default during the migration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacyAnnotationCandidate {
    key: Vec<u8>,
    annotation_media_type: Option<String>,
    suffix: Option<String>,
}

impl LegacyAnnotationCandidate {
    pub fn new(
        key: Vec<u8>,
        annotation_media_type: Option<String>,
        suffix: Option<String>,
    ) -> Self {
        Self {
            key,
            annotation_media_type,
            suffix,
        }
    }

    pub fn key(&self) -> &[u8] {
        &self.key
    }

    pub fn annotation_media_type(&self) -> Option<&str> {
        self.annotation_media_type.as_deref()
    }

    pub fn suffix(&self) -> Option<&str> {
        self.suffix.as_deref()
    }
}

pub(crate) fn validate_migration_identity_component(
    value: &[u8],
    nullable: bool,
) -> Result<(), RepositoryError> {
    if value.len() > MAX_MIGRATION_IDENTITY_COMPONENT_BYTES || (!nullable && value.is_empty()) {
        return Err(RepositoryError::InvalidFormatMigration);
    }
    Ok(())
}

fn put_migration_identity_component(
    bytes: &mut Vec<u8>,
    value: &[u8],
) -> Result<(), RepositoryError> {
    validate_migration_identity_component(value, false)?;
    put_optional_bytes(bytes, Some(value))
}

fn take_migration_identity_component(
    bytes: &[u8],
    cursor: &mut usize,
) -> Result<Vec<u8>, RepositoryError> {
    let value = take_optional_migration_identity_component(bytes, cursor)?
        .ok_or(RepositoryError::InvalidPublicationJournal)?;
    validate_migration_identity_component(&value, false)
        .map_err(|_| RepositoryError::InvalidPublicationJournal)?;
    Ok(value)
}

fn take_optional_migration_identity_component(
    bytes: &[u8],
    cursor: &mut usize,
) -> Result<Option<Vec<u8>>, RepositoryError> {
    let length = take_u32(bytes, cursor)?;
    if length == u32::MAX {
        return Ok(None);
    }
    let length = usize::try_from(length).map_err(|_| RepositoryError::InvalidPublicationJournal)?;
    if length == 0 || length > MAX_MIGRATION_IDENTITY_COMPONENT_BYTES {
        return Err(RepositoryError::InvalidPublicationJournal);
    }
    let end = cursor
        .checked_add(length)
        .ok_or(RepositoryError::InvalidPublicationJournal)?;
    let value = bytes
        .get(*cursor..end)
        .ok_or(RepositoryError::InvalidPublicationJournal)?
        .to_vec();
    *cursor = end;
    Ok(Some(value))
}
