//! Storage placement and storage-only rewrite planning.
//!
//! The plans in this module are the storage policy boundary. They are pure
//! values: repository/runtime adapters still provide the expected snapshot
//! identity, generation-CAS, durable journal, and crash recovery.

use std::collections::{BTreeMap, BTreeSet};

use orna_foundation_v1::CanonicalValue;
use sha2::{Digest, Sha256};

use crate::{
    CompactKeyIdentity, CompactOvbProfile, HybridBaseState, LoosePath, LooseRow,
};

const MIB: usize = 1024 * 1024;
/// Automatic placement stays editable through this resulting table size.
pub const AUTOMATIC_EDITABLE_MAX_ROWS: usize = 10_000;
/// Automatic placement switches new programmatic rows to compact above 8 MiB.
pub const AUTOMATIC_EDITABLE_MAX_PUBLICATION_BYTES: usize = 8 * MIB;
/// Loose row payloads use the existing 8 MiB materialization bound.
pub const MAX_EDITABLE_ROW_BYTES: usize = 8 * MIB;
/// Bound on a single planned rewrite. The reference requires resource bounds
/// but does not choose values; this initial limit caps adapter staging memory.
pub const MAX_STORAGE_REWRITE_BYTES: usize = 256 * MIB;
/// Bound on a single planned rewrite. Adapters may split larger rewrites into
/// separately reviewed operations without changing their logical semantics.
pub const MAX_STORAGE_REWRITE_ROWS: usize = 100_000;

/// Operational preference for future programmatic inserts.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum StoragePreference {
    #[default]
    Automatic,
    Editable,
    Compact,
}

/// Committed operational placement metadata for one table.
///
/// Persist this value beside the table manifest. Replacing it changes future
/// insert placement only; it does not rewrite rows or alter logical identity.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StoragePlacementPolicy {
    preference: StoragePreference,
}

impl StoragePlacementPolicy {
    pub const fn new(preference: StoragePreference) -> Self {
        Self { preference }
    }

    pub const fn preference(self) -> StoragePreference {
        self.preference
    }

    pub const fn set_preference(self, preference: StoragePreference) -> Self {
        Self { preference }
    }
}

/// Physical profile visible in a committed table snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StorageProfile {
    Empty,
    Editable,
    Compact,
    Hybrid,
}

/// Destination for an explicit full-table rewrite.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StorageRewriteTarget {
    Editable,
    Compact,
}

/// Current physical authority for a logical key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhysicalPlacement {
    Editable,
    Compact,
}

/// A pending table mutation considered by the placement policy.
///
/// `canonical_body_bytes` is the exact canonical row body size input used by
/// the publication threshold. `editable_path` is present only when the key
/// has a representable portable loose-row destination.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlacementCandidate {
    pub encoded_key: Vec<u8>,
    pub canonical_body_bytes: Option<usize>,
    pub existing: Option<PhysicalPlacement>,
    pub editable_path: Option<LoosePath>,
}

impl PlacementCandidate {
    pub fn insert(
        encoded_key: Vec<u8>,
        canonical_body_bytes: usize,
        editable_path: Option<LoosePath>,
    ) -> Self {
        Self {
            encoded_key,
            canonical_body_bytes: Some(canonical_body_bytes),
            existing: None,
            editable_path,
        }
    }

    pub fn update(
        encoded_key: Vec<u8>,
        canonical_body_bytes: usize,
        existing: PhysicalPlacement,
        editable_path: Option<LoosePath>,
    ) -> Self {
        Self {
            encoded_key,
            canonical_body_bytes: Some(canonical_body_bytes),
            existing: Some(existing),
            editable_path,
        }
    }

    pub fn delete(
        encoded_key: Vec<u8>,
        existing: PhysicalPlacement,
        editable_path: Option<LoosePath>,
    ) -> Self {
        Self {
            encoded_key,
            canonical_body_bytes: None,
            existing: Some(existing),
            editable_path,
        }
    }
}

/// Logical operation recorded for one placement decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlacementAction {
    Insert,
    Update,
    Delete,
}

/// One schema-validated physical route for a pending mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlacementDecision {
    key: CompactKeyIdentity,
    action: PlacementAction,
    placement: PhysicalPlacement,
    editable_path: Option<LoosePath>,
    canonical_body_bytes: usize,
}

impl PlacementDecision {
    pub fn key(&self) -> &CompactKeyIdentity {
        &self.key
    }

    pub const fn action(&self) -> PlacementAction {
        self.action
    }

    pub const fn placement(&self) -> PhysicalPlacement {
        self.placement
    }

    pub fn editable_path(&self) -> Option<&LoosePath> {
        self.editable_path.as_ref()
    }

    pub const fn canonical_body_bytes(&self) -> usize {
        self.canonical_body_bytes
    }
}

/// Why the automatic policy selected a physical representation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlacementReason {
    ExplicitPreference,
    AutomaticEditable,
    AutomaticTableAlreadyCompact,
    AutomaticRowLimit,
    AutomaticPublicationBytes,
    AutomaticUnrepresentablePath,
}

/// Atomic output of placement planning. No runtime mutation is consumed here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlacementPlan {
    preference: StoragePreference,
    new_row_placement: PhysicalPlacement,
    resulting_row_count: usize,
    reason: PlacementReason,
    decisions: Vec<PlacementDecision>,
}

impl PlacementPlan {
    pub const fn preference(&self) -> StoragePreference {
        self.preference
    }

    pub const fn new_row_placement(&self) -> PhysicalPlacement {
        self.new_row_placement
    }

    pub const fn resulting_row_count(&self) -> usize {
        self.resulting_row_count
    }

    pub const fn reason(&self) -> PlacementReason {
        self.reason
    }

    pub fn decisions(&self) -> &[PlacementDecision] {
        &self.decisions
    }
}

/// Chooses placement for an already validated programmatic mutation batch.
///
/// Existing rows always keep their physical representation. The table layer
/// passes an exact current row count and whether compact data is committed;
/// direct valid loose-file edits do not use this automatic policy.
pub fn plan_storage_placement(
    profile: &CompactOvbProfile,
    preference: StoragePreference,
    current_row_count: usize,
    compact_data_exists: bool,
    candidates: impl IntoIterator<Item = PlacementCandidate>,
) -> Result<PlacementPlan, StoragePlacementError> {
    let candidates: Vec<_> = candidates.into_iter().collect();
    if candidates.is_empty() {
        return Err(StoragePlacementError::EmptyBatch);
    }

    let mut keyed = BTreeMap::new();
    let mut all_body_bytes = 0usize;
    let mut insert_count = 0usize;
    let mut delete_count = 0usize;
    let mut all_insert_paths = true;
    for candidate in candidates {
        let key = profile
            .decode_key(&candidate.encoded_key)
            .map_err(|_| StoragePlacementError::InvalidKey)?;
        let action = match (candidate.existing, candidate.canonical_body_bytes) {
            (None, Some(body_bytes)) if body_bytes > 0 => {
                insert_count = insert_count
                    .checked_add(1)
                    .ok_or(StoragePlacementError::RowCountOverflow)?;
                all_insert_paths &= candidate.editable_path.is_some();
                PlacementAction::Insert
            }
            (Some(_), Some(body_bytes)) if body_bytes > 0 => PlacementAction::Update,
            (Some(_), None) => {
                delete_count = delete_count
                    .checked_add(1)
                    .ok_or(StoragePlacementError::RowCountOverflow)?;
                PlacementAction::Delete
            }
            (None, None) => return Err(StoragePlacementError::DeleteMissingRow),
            (_, Some(_)) => return Err(StoragePlacementError::EmptyRow),
        };
        let body_bytes = candidate.canonical_body_bytes.unwrap_or(0);
        all_body_bytes = all_body_bytes
            .checked_add(body_bytes)
            .ok_or(StoragePlacementError::SizeOverflow)?;
        if keyed
            .insert(
                key,
                (candidate, action, body_bytes),
            )
            .is_some()
        {
            return Err(StoragePlacementError::DuplicateKey);
        }
    }

    let resulting_row_count = current_row_count
        .checked_add(insert_count)
        .and_then(|rows| rows.checked_sub(delete_count))
        .ok_or(StoragePlacementError::RowCountOverflow)?;

    let (mut new_row_placement, mut reason) = match preference {
        StoragePreference::Editable => (PhysicalPlacement::Editable, PlacementReason::ExplicitPreference),
        StoragePreference::Compact => (PhysicalPlacement::Compact, PlacementReason::ExplicitPreference),
        StoragePreference::Automatic if compact_data_exists => (
            PhysicalPlacement::Compact,
            PlacementReason::AutomaticTableAlreadyCompact,
        ),
        StoragePreference::Automatic
            if resulting_row_count > AUTOMATIC_EDITABLE_MAX_ROWS =>
        {
            (PhysicalPlacement::Compact, PlacementReason::AutomaticRowLimit)
        }
        StoragePreference::Automatic
            if all_body_bytes > AUTOMATIC_EDITABLE_MAX_PUBLICATION_BYTES =>
        {
            (PhysicalPlacement::Compact, PlacementReason::AutomaticPublicationBytes)
        }
        StoragePreference::Automatic if !all_insert_paths => (
            PhysicalPlacement::Compact,
            PlacementReason::AutomaticUnrepresentablePath,
        ),
        StoragePreference::Automatic => (
            PhysicalPlacement::Editable,
            PlacementReason::AutomaticEditable,
        ),
    };

    let mut decisions = Vec::with_capacity(keyed.len());
    for (key, (candidate, action, body_bytes)) in keyed {
        let placement = match candidate.existing {
            Some(PhysicalPlacement::Editable) => PhysicalPlacement::Editable,
            Some(PhysicalPlacement::Compact) => PhysicalPlacement::Compact,
            None => new_row_placement,
        };
        let editable_path = if placement == PhysicalPlacement::Editable {
            if let Some(path) = candidate.editable_path {
                if body_bytes > MAX_EDITABLE_ROW_BYTES {
                    return Err(StoragePlacementError::EditableRowTooLarge);
                }
                Some(path)
            } else {
                return Err(StoragePlacementError::UnrepresentableEditableKey);
            }
        } else {
            None
        };
        decisions.push(PlacementDecision {
            key,
            action,
            placement,
            editable_path,
            canonical_body_bytes: body_bytes,
        });
    }

    let requested_paths: Vec<_> = decisions
        .iter()
        .filter_map(|decision| decision.editable_path.as_ref())
        .collect();
    if placement_paths_collide(&requested_paths) {
        if preference == StoragePreference::Automatic
            && new_row_placement == PhysicalPlacement::Editable
        {
            let existing_paths: Vec<_> = decisions
                .iter()
                .filter(|decision| decision.action != PlacementAction::Insert)
                .filter_map(|decision| decision.editable_path.as_ref())
                .collect();
            if placement_paths_collide(&existing_paths) {
                return Err(StoragePlacementError::PathCollision);
            }

            // The reference requires every path to be valid for automatic
            // editable placement but does not spell out batch path aliases.
            // Treat a collectively colliding insert set as an automatic
            // compact fallback; existing editable rows retain their placement.
            new_row_placement = PhysicalPlacement::Compact;
            reason = PlacementReason::AutomaticUnrepresentablePath;
            for decision in &mut decisions {
                if decision.action == PlacementAction::Insert {
                    decision.placement = PhysicalPlacement::Compact;
                    decision.editable_path = None;
                }
            }
        } else {
            return Err(StoragePlacementError::PathCollision);
        }
    }

    let remaining_paths: Vec<_> = decisions
        .iter()
        .filter_map(|decision| decision.editable_path.as_ref())
        .collect();
    if placement_paths_collide(&remaining_paths) {
        return Err(StoragePlacementError::PathCollision);
    }

    Ok(PlacementPlan {
        preference,
        new_row_placement,
        resulting_row_count,
        reason,
        decisions,
    })
}

fn placement_paths_collide(paths: &[&LoosePath]) -> bool {
    let mut exact_paths = BTreeSet::new();
    if paths.iter().any(|path| !exact_paths.insert(*path)) {
        return true;
    }
    crate::validate_portable_paths(paths.iter().copied()).is_err()
}

/// One canonical row included in a full placement rewrite.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageRewriteRow {
    key: Vec<u8>,
    canonical_value: Vec<u8>,
    source: PhysicalPlacement,
    destination: PhysicalPlacement,
    row_hash: [u8; 32],
    editable_path: Option<LoosePath>,
    editable_bytes: Option<Vec<u8>>,
}

impl StorageRewriteRow {
    pub fn key(&self) -> &[u8] {
        &self.key
    }

    pub fn canonical_value(&self) -> &[u8] {
        &self.canonical_value
    }

    pub const fn source(&self) -> PhysicalPlacement {
        self.source
    }

    pub const fn destination(&self) -> PhysicalPlacement {
        self.destination
    }

    pub const fn row_hash(&self) -> [u8; 32] {
        self.row_hash
    }

    pub fn editable_path(&self) -> Option<&LoosePath> {
        self.editable_path.as_ref()
    }

    pub fn editable_bytes(&self) -> Option<&[u8]> {
        self.editable_bytes.as_deref()
    }
}

/// Generation-bound, storage-only rewrite preview.
///
/// The adapter must publish the complete target behind one generation barrier
/// and use compare-and-swap against the observed base HEAD. `verify_candidate`
/// is required before that barrier becomes visible. The plan only replaces the
/// current placement snapshot; it never prunes ancestor commits or row objects.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageRewritePlan {
    table_id: [u8; 16],
    schema_fingerprint: [u8; 32],
    from: StorageProfile,
    to: StorageRewriteTarget,
    previous_generation: u64,
    generation: u64,
    rows: Vec<StorageRewriteRow>,
    logical_digest: [u8; 32],
    estimated_output_bytes: u64,
}

impl StorageRewritePlan {
    pub const fn from(&self) -> StorageProfile {
        self.from
    }

    pub const fn to(&self) -> StorageRewriteTarget {
        self.to
    }

    pub const fn previous_generation(&self) -> u64 {
        self.previous_generation
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub fn rows(&self) -> &[StorageRewriteRow] {
        &self.rows
    }

    pub const fn logical_digest(&self) -> [u8; 32] {
        self.logical_digest
    }

    pub const fn estimated_output_bytes(&self) -> u64 {
        self.estimated_output_bytes
    }

    /// Verifies exact canonical key/value equality before publication.
    pub fn verify_candidate(
        &self,
        profile: &CompactOvbProfile,
        rows: impl IntoIterator<Item = (Vec<u8>, Vec<u8>)>,
    ) -> Result<StorageRewriteVerification, StorageRewriteError> {
        self.verify_rows(profile, rows, StorageRewriteError::CandidateMismatch)
    }

    /// Rechecks the source table immediately before a rewrite crosses its
    /// generation barrier. Comparing the full logical row set closes the
    /// editable-only case where storage generations can otherwise repeat;
    /// repository adapters must still compare-and-swap against the captured
    /// Git HEAD so unrelated snapshot changes also conflict.
    pub fn verify_source_snapshot(
        &self,
        profile: &CompactOvbProfile,
        observed_generation: u64,
        rows: impl IntoIterator<Item = (Vec<u8>, Vec<u8>)>,
    ) -> Result<(), StorageRewriteError> {
        if observed_generation != self.previous_generation {
            return Err(StorageRewriteError::StaleInput);
        }
        self.verify_rows(profile, rows, StorageRewriteError::StaleInput)
            .map(|_| ())
    }

    fn verify_rows(
        &self,
        profile: &CompactOvbProfile,
        rows: impl IntoIterator<Item = (Vec<u8>, Vec<u8>)>,
        mismatch_error: StorageRewriteError,
    ) -> Result<StorageRewriteVerification, StorageRewriteError> {
        if profile.table_id() != self.table_id
            || profile.schema_fingerprint() != self.schema_fingerprint
        {
            return Err(StorageRewriteError::WrongProfile);
        }
        let mut actual = BTreeMap::new();
        for (key_bytes, value_bytes) in rows {
            profile
                .decode_key(&key_bytes)
                .map_err(|_| StorageRewriteError::InvalidCanonicalRow)?;
            let key = CanonicalValue::decode(&key_bytes)
                .map_err(|_| StorageRewriteError::InvalidCanonicalRow)?;
            let value = CanonicalValue::decode(&value_bytes)
                .map_err(|_| StorageRewriteError::InvalidCanonicalRow)?;
            if key.encode().map_err(|_| StorageRewriteError::InvalidCanonicalRow)? != key_bytes
                || value
                    .encode()
                    .map_err(|_| StorageRewriteError::InvalidCanonicalRow)?
                    != value_bytes
            {
                return Err(StorageRewriteError::InvalidCanonicalRow);
            }
            let identity = profile
                .decode_key(&key_bytes)
                .map_err(|_| StorageRewriteError::InvalidCanonicalRow)?;
            if actual.insert(identity, (key_bytes, value_bytes)).is_some() {
                return Err(StorageRewriteError::DuplicateKey);
            }
        }
        let expected = self
            .rows
            .iter()
            .map(|row| {
                let key = profile
                    .decode_key(&row.key)
                    .map_err(|_| StorageRewriteError::InvalidCanonicalRow)?;
                Ok((key, (row.key.clone(), row.canonical_value.clone())))
            })
            .collect::<Result<BTreeMap<_, _>, StorageRewriteError>>()?;
        if actual != expected {
            return Err(mismatch_error);
        }
        Ok(StorageRewriteVerification {
            rows: actual.len(),
            semantic_diff_entries: 0,
            logical_digest: self.logical_digest,
        })
    }
}

/// Successful proof result for a rewrite candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StorageRewriteVerification {
    rows: usize,
    semantic_diff_entries: usize,
    logical_digest: [u8; 32],
}

impl StorageRewriteVerification {
    pub const fn rows(self) -> usize {
        self.rows
    }

    pub const fn semantic_diff_entries(self) -> usize {
        self.semantic_diff_entries
    }

    pub const fn logical_digest(self) -> [u8; 32] {
        self.logical_digest
    }
}

/// Creates a full-table rewrite plan from an exact hybrid logical snapshot.
///
/// Editable encoding/decoding and key-to-path mapping are supplied by the table
/// layer, which owns canonical `.orna` row syntax and namespace. Each encoded
/// row is decoded again before the plan is returned. The plan remains storage-
/// only and does not advance a repository ref.
pub fn plan_storage_rewrite(
    profile: &CompactOvbProfile,
    base: &HybridBaseState,
    target: StorageRewriteTarget,
    mut path_for_key: impl FnMut(&CompactKeyIdentity) -> Result<LoosePath, StorageRewriteError>,
    mut encode_editable_row: impl FnMut(
        &CompactKeyIdentity,
        &CanonicalValue,
    ) -> Result<Vec<u8>, StorageRewriteError>,
    mut decode_editable_row: impl FnMut(
        &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>), StorageRewriteError>,
) -> Result<StorageRewritePlan, StorageRewriteError> {
    let compact = base.compact();
    if compact.table_id() != profile.table_id()
        || compact.schema_fingerprint() != profile.schema_fingerprint()
    {
        return Err(StorageRewriteError::WrongProfile);
    }
    let mut rows: BTreeMap<CompactKeyIdentity, (CanonicalValue, PhysicalPlacement)> =
        BTreeMap::new();
    let mut source_bytes = 0usize;
    for row in compact.rows() {
        if let Some(value) = row.value() {
            let key_bytes = row
                .key()
                .encode()
                .map_err(|_| StorageRewriteError::InvalidCanonicalRow)?;
            let value_bytes = value
                .encode()
                .map_err(|_| StorageRewriteError::InvalidCanonicalRow)?;
            source_bytes = source_bytes
                .checked_add(key_bytes.len())
                .and_then(|bytes| bytes.checked_add(value_bytes.len()))
                .ok_or(StorageRewriteError::ResourceLimit)?;
            if source_bytes > MAX_STORAGE_REWRITE_BYTES {
                return Err(StorageRewriteError::ResourceLimit);
            }
            let key = profile
                .decode_key(&key_bytes)
                .map_err(|_| StorageRewriteError::InvalidCanonicalRow)?;
            if rows
                .insert(key, (value.clone(), PhysicalPlacement::Compact))
                .is_some()
            {
                return Err(StorageRewriteError::DuplicateKey);
            }
        }
    }
    for (key, value) in base.editable_rows() {
        let value_bytes = value
            .encode()
            .map_err(|_| StorageRewriteError::InvalidCanonicalRow)?;
        source_bytes = source_bytes
            .checked_add(key.encoded().len())
            .and_then(|bytes| bytes.checked_add(value_bytes.len()))
            .ok_or(StorageRewriteError::ResourceLimit)?;
        if source_bytes > MAX_STORAGE_REWRITE_BYTES {
            return Err(StorageRewriteError::ResourceLimit);
        }
        if rows
            .insert(
                key.clone(),
                (value.clone(), PhysicalPlacement::Editable),
            )
            .is_some()
        {
            return Err(StorageRewriteError::DuplicateKey);
        }
    }
    if rows.is_empty() {
        return Err(StorageRewriteError::EmptyTable);
    }
    if rows.len() > MAX_STORAGE_REWRITE_ROWS {
        return Err(StorageRewriteError::ResourceLimit);
    }
    let has_editable = rows
        .values()
        .any(|(_, place)| *place == PhysicalPlacement::Editable);
    let has_compact = rows
        .values()
        .any(|(_, place)| *place == PhysicalPlacement::Compact);
    let from = match (has_editable, has_compact) {
        (true, true) => StorageProfile::Hybrid,
        (true, false) => StorageProfile::Editable,
        (false, true) => StorageProfile::Compact,
        (false, false) => StorageProfile::Empty,
    };
    if matches!(
        (from, target),
        (StorageProfile::Editable, StorageRewriteTarget::Editable)
            | (StorageProfile::Compact, StorageRewriteTarget::Compact)
    ) {
        return Err(StorageRewriteError::AlreadyAtTarget);
    }

    let mut output = Vec::with_capacity(rows.len());
    let mut paths = BTreeSet::new();
    let mut total_output_bytes = 0usize;
    let mut staged_bytes = 0usize;
    for (key, (value, source)) in rows {
        let key_bytes = key.encoded().to_vec();
        let value_bytes = value
            .encode()
            .map_err(|_| StorageRewriteError::InvalidCanonicalRow)?;
        staged_bytes = staged_bytes
            .checked_add(key_bytes.len())
            .and_then(|bytes| bytes.checked_add(value_bytes.len()))
            .ok_or(StorageRewriteError::ResourceLimit)?;
        let mut hasher = Sha256::new();
        hasher.update(b"orna.storage.row.v1\0");
        hasher.update(u64::try_from(key_bytes.len()).map_err(|_| StorageRewriteError::ResourceLimit)?.to_be_bytes());
        hasher.update(&key_bytes);
        hasher.update(u64::try_from(value_bytes.len()).map_err(|_| StorageRewriteError::ResourceLimit)?.to_be_bytes());
        hasher.update(&value_bytes);
        let row_hash = hasher.finalize().into();
        let (destination, editable_path, editable_bytes) = match target {
            StorageRewriteTarget::Compact => {
                total_output_bytes = total_output_bytes
                    .checked_add(key_bytes.len())
                    .and_then(|bytes| bytes.checked_add(value_bytes.len()))
                    .ok_or(StorageRewriteError::ResourceLimit)?;
                (PhysicalPlacement::Compact, None, None)
            }
            StorageRewriteTarget::Editable => {
                let path = path_for_key(&key)?;
                if !paths.insert(path.clone()) {
                    return Err(StorageRewriteError::PathCollision);
                }
                let bytes = encode_editable_row(&key, &value)?;
                let row = LooseRow::new(bytes.clone()).map_err(|_| StorageRewriteError::InvalidEditableRow)?;
                let (decoded_key, decoded_value) = decode_editable_row(row.bytes())?;
                if decoded_key != key_bytes || decoded_value != value_bytes {
                    return Err(StorageRewriteError::CandidateMismatch);
                }
                total_output_bytes = total_output_bytes
                    .checked_add(row.bytes().len())
                    .ok_or(StorageRewriteError::ResourceLimit)?;
                staged_bytes = staged_bytes
                    .checked_add(row.bytes().len())
                    .ok_or(StorageRewriteError::ResourceLimit)?;
                (PhysicalPlacement::Editable, Some(path), Some(bytes))
            }
        };
        if total_output_bytes > MAX_STORAGE_REWRITE_BYTES
            || staged_bytes > MAX_STORAGE_REWRITE_BYTES
        {
            return Err(StorageRewriteError::ResourceLimit);
        }
        output.push(StorageRewriteRow {
            key: key_bytes,
            canonical_value: value_bytes,
            source,
            destination,
            row_hash,
            editable_path,
            editable_bytes,
        });
    }
    if let StorageRewriteTarget::Editable = target {
        crate::validate_portable_paths(output.iter().filter_map(|row| row.editable_path.as_ref()))
            .map_err(|_| StorageRewriteError::UnrepresentablePath)?;
    }
    let logical_digest = digest_rows(
        output
            .iter()
            .map(|row| (row.key.as_slice(), row.canonical_value.as_slice())),
    )?;
    let generation = compact.next_generation();
    if generation == 0 {
        return Err(StorageRewriteError::InvalidGeneration);
    }
    Ok(StorageRewritePlan {
        table_id: profile.table_id(),
        schema_fingerprint: profile.schema_fingerprint(),
        from,
        to: target,
        previous_generation: generation - 1,
        generation,
        rows: output,
        logical_digest,
        estimated_output_bytes: u64::try_from(total_output_bytes)
            .map_err(|_| StorageRewriteError::ResourceLimit)?,
    })
}

fn digest_rows<'a>(
    rows: impl IntoIterator<Item = (&'a [u8], &'a [u8])>,
) -> Result<[u8; 32], StorageRewriteError> {
    let mut hasher = Sha256::new();
    hasher.update(b"orna.storage.logical-rows.v1\0");
    for (key, value) in rows {
        hasher.update(u64::try_from(key.len()).map_err(|_| StorageRewriteError::ResourceLimit)?.to_be_bytes());
        hasher.update(key);
        hasher.update(u64::try_from(value.len()).map_err(|_| StorageRewriteError::ResourceLimit)?.to_be_bytes());
        hasher.update(value);
    }
    Ok(hasher.finalize().into())
}

/// Invalid placement input or a refused table placement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoragePlacementError {
    EmptyBatch,
    InvalidKey,
    DuplicateKey,
    PathCollision,
    DeleteMissingRow,
    EmptyRow,
    RowCountOverflow,
    SizeOverflow,
    UnrepresentableEditableKey,
    EditableRowTooLarge,
}

impl std::fmt::Display for StoragePlacementError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::EmptyBatch => "storage placement batch is empty",
            Self::InvalidKey => "storage placement key is invalid for the table profile",
            Self::DuplicateKey => "storage placement batch repeats a logical key",
            Self::PathCollision => "storage placement paths collide",
            Self::DeleteMissingRow => "storage placement cannot delete a missing row",
            Self::EmptyRow => "storage placement row body is empty",
            Self::RowCountOverflow => "storage placement row count overflowed",
            Self::SizeOverflow => "storage placement byte count overflowed",
            Self::UnrepresentableEditableKey => "editable placement cannot represent the row key",
            Self::EditableRowTooLarge => "editable row exceeds the loose-row size limit",
        })
    }
}

impl std::error::Error for StoragePlacementError {}

/// A storage-only rewrite exceeded a validation or resource boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StorageRewriteError {
    WrongProfile,
    EmptyTable,
    AlreadyAtTarget,
    DuplicateKey,
    PathCollision,
    UnrepresentablePath,
    InvalidEditableRow,
    InvalidCanonicalRow,
    CandidateMismatch,
    StaleInput,
    ResourceLimit,
    InvalidGeneration,
}

impl std::fmt::Display for StorageRewriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::WrongProfile => "storage rewrite base does not match the table profile",
            Self::EmptyTable => "storage rewrite requires at least one logical row",
            Self::AlreadyAtTarget => "storage rewrite target already matches the table placement",
            Self::DuplicateKey => "storage rewrite found duplicate logical keys",
            Self::PathCollision => "storage rewrite paths collide",
            Self::UnrepresentablePath => "storage rewrite cannot represent a key as a portable path",
            Self::InvalidEditableRow => "storage rewrite produced an invalid editable row",
            Self::InvalidCanonicalRow => "storage rewrite row is not canonical",
            Self::CandidateMismatch => "storage rewrite candidate changes logical rows",
            Self::StaleInput => "storage rewrite input snapshot changed after planning",
            Self::ResourceLimit => "storage rewrite exceeds its configured resource bound",
            Self::InvalidGeneration => "storage rewrite candidate generation is invalid",
        })
    }
}

impl std::error::Error for StorageRewriteError {}
