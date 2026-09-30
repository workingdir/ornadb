//! Explicit compact-overlay consolidation planning.
//!
//! Consolidation is never part of ordinary publication. A caller requests a
//! preview and must expose the retained-history estimate before it executes;
//! an adapter writes a fresh data-only manifest only after generation-CAS and
//! logical-row verification.

use std::collections::BTreeMap;

use orna_foundation_v1::CanonicalValue;
use orna_repository_v1::{CompactManifest, CompactSegmentRole, Uuid};
use sha2::{Digest, Sha256};

use crate::{CompactBaseState, CompactOvbProfile};

/// Initial overlay threshold where compaction is considered operationally
/// material. Chapter 23 defines the rule but not a numeric trigger; four
/// overlay files plus at least 25% overlay bytes is a conservative v1 choice.
pub const MIN_OVERLAY_SEGMENTS_FOR_CONSOLIDATION: usize = 4;
/// Minimum share of retained compact bytes occupied by replacement/deletion
/// overlays before consolidation is offered.
pub const MIN_OVERLAY_BYTES_SHARE_PERCENT: u64 = 25;

/// A complete row materialized into the new consolidated data generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConsolidatedRow {
    key: Vec<u8>,
    value: Vec<u8>,
}

impl ConsolidatedRow {
    pub fn key(&self) -> &[u8] {
        &self.key
    }

    pub fn value(&self) -> &[u8] {
        &self.value
    }
}

/// Preview and input for an explicit compaction consolidation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompactConsolidationPlan {
    table: Uuid,
    schema: [u8; 32],
    generation: u64,
    input_segments: Vec<Uuid>,
    input_compressed_bytes: u64,
    overlay_segments: usize,
    overlay_compressed_bytes: u64,
    estimated_output_bytes: u64,
    expected_rows: Vec<ConsolidatedRow>,
    logical_digest: [u8; 32],
}

impl CompactConsolidationPlan {
    pub fn table(&self) -> Uuid {
        self.table
    }

    pub const fn schema(&self) -> [u8; 32] {
        self.schema
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub fn input_segments(&self) -> &[Uuid] {
        &self.input_segments
    }

    pub const fn input_compressed_bytes(&self) -> u64 {
        self.input_compressed_bytes
    }

    pub const fn overlay_segments(&self) -> usize {
        self.overlay_segments
    }

    pub const fn overlay_compressed_bytes(&self) -> u64 {
        self.overlay_compressed_bytes
    }

    pub const fn estimated_output_bytes(&self) -> u64 {
        self.estimated_output_bytes
    }

    /// Estimate of additional immutable bytes retained after publication.
    /// Existing inputs remain reachable in Git history, so the estimate is
    /// the new output size rather than the difference from input size.
    pub const fn estimated_extra_retained_history_bytes(&self) -> u64 {
        self.estimated_output_bytes
    }

    pub fn rows(&self) -> &[ConsolidatedRow] {
        &self.expected_rows
    }

    pub const fn logical_digest(&self) -> [u8; 32] {
        self.logical_digest
    }

    /// Rejects duplicate, malformed, missing, or changed output rows. A
    /// successful result is the proof that the logical semantic diff is empty.
    pub fn verify_candidate(
        &self,
        profile: &CompactOvbProfile,
        output_rows: impl IntoIterator<Item = (Vec<u8>, Vec<u8>)>,
    ) -> Result<ConsolidationVerification, CompactConsolidationError> {
        if profile.table_id() != *self.table.as_bytes() {
            return Err(CompactConsolidationError::WrongTable);
        }
        if profile.schema_fingerprint() != self.schema {
            return Err(CompactConsolidationError::WrongSchema);
        }
        let mut actual = BTreeMap::new();
        for (key_bytes, value_bytes) in output_rows {
            profile
                .decode_key(&key_bytes)
                .map_err(|_| CompactConsolidationError::InvalidOutputRow)?;
            let key = CanonicalValue::decode(&key_bytes)
                .map_err(|_| CompactConsolidationError::InvalidOutputRow)?;
            let value = CanonicalValue::decode(&value_bytes)
                .map_err(|_| CompactConsolidationError::InvalidOutputRow)?;
            if key.encode().map_err(|_| CompactConsolidationError::InvalidOutputRow)? != key_bytes
                || value
                    .encode()
                    .map_err(|_| CompactConsolidationError::InvalidOutputRow)?
                    != value_bytes
            {
                return Err(CompactConsolidationError::InvalidOutputRow);
            }
            let identity = profile
                .decode_key(&key_bytes)
                .map_err(|_| CompactConsolidationError::InvalidOutputRow)?;
            if actual.insert(identity, (key_bytes, value_bytes)).is_some() {
                return Err(CompactConsolidationError::DuplicateOutputKey);
            }
        }
        let expected = self
            .expected_rows
            .iter()
            .map(|row| {
                let key = profile
                    .decode_key(&row.key)
                    .map_err(|_| CompactConsolidationError::InvalidOutputRow)?;
                Ok((key, (row.key.clone(), row.value.clone())))
            })
            .collect::<Result<BTreeMap<_, _>, CompactConsolidationError>>()?;
        if actual != expected {
            return Err(CompactConsolidationError::SemanticChange);
        }
        Ok(ConsolidationVerification {
            rows: actual.len(),
            semantic_diff_entries: 0,
            logical_digest: self.logical_digest,
        })
    }
}

/// Successful empty-semantic-diff proof for a consolidation candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConsolidationVerification {
    rows: usize,
    semantic_diff_entries: usize,
    logical_digest: [u8; 32],
}

impl ConsolidationVerification {
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

/// Builds a reviewable consolidation preview from one verified manifest and
/// its fully folded logical base. Calling this function is the explicit
/// maintenance request; it refuses routine data-only rewrites. The preview
/// emits every live row in the folded base, with no age-based rolling window.
/// A deletion tombstone may disappear from the new complete snapshot once its
/// effect is folded, while the older immutable manifests and rows stay in
/// ancestor Git commits. History rewriting is a separate destructive action.
pub fn plan_compact_consolidation(
    profile: &CompactOvbProfile,
    manifest: &CompactManifest,
    base: &CompactBaseState,
    estimated_output_bytes: u64,
) -> Result<CompactConsolidationPlan, CompactConsolidationError> {
    if manifest.table().as_bytes() != &profile.table_id()
        || base.table_id() != profile.table_id()
    {
        return Err(CompactConsolidationError::WrongTable);
    }
    if manifest.schema() != profile.schema_fingerprint()
        || base.schema_fingerprint() != profile.schema_fingerprint()
    {
        return Err(CompactConsolidationError::WrongSchema);
    }
    if manifest.next_generation() != base.next_generation() {
        return Err(CompactConsolidationError::StaleGeneration);
    }
    if estimated_output_bytes == 0 && base.rows().any(|row| row.value().is_some()) {
        return Err(CompactConsolidationError::MissingOutputEstimate);
    }

    let mut input_compressed_bytes = 0u64;
    let mut overlay_compressed_bytes = 0u64;
    let mut overlay_segments = 0usize;
    let mut input_segments = Vec::with_capacity(manifest.entries().len());
    for entry in manifest.entries() {
        input_compressed_bytes = input_compressed_bytes
            .checked_add(entry.compressed_bytes())
            .ok_or(CompactConsolidationError::SizeOverflow)?;
        if matches!(entry.role(), CompactSegmentRole::Replacement | CompactSegmentRole::Deletion) {
            overlay_segments = overlay_segments
                .checked_add(1)
                .ok_or(CompactConsolidationError::SizeOverflow)?;
            overlay_compressed_bytes = overlay_compressed_bytes
                .checked_add(entry.compressed_bytes())
                .ok_or(CompactConsolidationError::SizeOverflow)?;
        }
        input_segments.push(entry.segment_id());
    }
    // The numeric trigger is a pragmatic first cut because the reference
    // leaves “materially harm performance” qualitative. It is deliberately
    // conservative and remains independently observable in the preview.
    if !overlay_pressure_is_material(
        overlay_segments,
        input_compressed_bytes,
        overlay_compressed_bytes,
    ) {
        return Err(CompactConsolidationError::NotMateriallyOverlaid);
    }

    let mut expected_rows = Vec::new();
    for row in base.rows() {
        let Some(value) = row.value() else {
            // This tombstone is represented by absence in the new complete
            // snapshot. The input manifest and its prior snapshot are retained.
            continue;
        };
        let key = row
            .key()
            .encode()
            .map_err(|_| CompactConsolidationError::InvalidBase)?;
        let value = value
            .encode()
            .map_err(|_| CompactConsolidationError::InvalidBase)?;
        expected_rows.push(ConsolidatedRow { key, value });
    }
    let logical_digest = digest_logical_rows(&expected_rows)?;
    Ok(CompactConsolidationPlan {
        table: manifest.table(),
        schema: manifest.schema(),
        generation: manifest.next_generation(),
        input_segments,
        input_compressed_bytes,
        overlay_segments,
        overlay_compressed_bytes,
        estimated_output_bytes,
        expected_rows,
        logical_digest,
    })
}

fn overlay_pressure_is_material(
    overlay_segments: usize,
    input_compressed_bytes: u64,
    overlay_compressed_bytes: u64,
) -> bool {
    overlay_segments >= MIN_OVERLAY_SEGMENTS_FOR_CONSOLIDATION
        && input_compressed_bytes > 0
        && u128::from(overlay_compressed_bytes) * 100
            >= u128::from(input_compressed_bytes) * u128::from(MIN_OVERLAY_BYTES_SHARE_PERCENT)
}

fn digest_logical_rows(rows: &[ConsolidatedRow]) -> Result<[u8; 32], CompactConsolidationError> {
    let mut hasher = Sha256::new();
    hasher.update(b"orna.storage.logical-rows.v1\0");
    for row in rows {
        hasher.update(
            u64::try_from(row.key.len())
                .map_err(|_| CompactConsolidationError::SizeOverflow)?
                .to_be_bytes(),
        );
        hasher.update(&row.key);
        hasher.update(
            u64::try_from(row.value.len())
                .map_err(|_| CompactConsolidationError::SizeOverflow)?
                .to_be_bytes(),
        );
        hasher.update(&row.value);
    }
    Ok(hasher.finalize().into())
}

/// Reasons an explicit consolidation preview was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompactConsolidationError {
    WrongTable,
    WrongSchema,
    StaleGeneration,
    NotMateriallyOverlaid,
    MissingOutputEstimate,
    InvalidBase,
    InvalidOutputRow,
    DuplicateOutputKey,
    SemanticChange,
    SizeOverflow,
}

impl std::fmt::Display for CompactConsolidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::WrongTable => "compact consolidation table identity mismatches",
            Self::WrongSchema => "compact consolidation schema identity mismatches",
            Self::StaleGeneration => "compact consolidation base generation is stale",
            Self::NotMateriallyOverlaid => "compact overlays do not justify consolidation",
            Self::MissingOutputEstimate => "compact consolidation output estimate is missing",
            Self::InvalidBase => "compact consolidation base contains a noncanonical row",
            Self::InvalidOutputRow => "compact consolidation output row is not canonical",
            Self::DuplicateOutputKey => "compact consolidation repeats an output key",
            Self::SemanticChange => "compact consolidation changes logical rows",
            Self::SizeOverflow => "compact consolidation byte estimate overflowed",
        })
    }
}

impl std::error::Error for CompactConsolidationError {}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_foundation_v1::OvbRaw;

    const TABLE: Uuid = Uuid::from_u128(1);
    const KEY_FIELD: Uuid =
        Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0001);

    fn profile() -> CompactOvbProfile {
        CompactOvbProfile::new(
            orna_foundation_v1::SchemaDescriptor::new(OvbRaw::Map(vec![
                (OvbRaw::Int(0.into()), OvbRaw::Int(1.into())),
                (
                    OvbRaw::Int(1.into()),
                    OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(TABLE.as_bytes().to_vec()))),
                ),
                (
                    OvbRaw::Int(2.into()),
                    OvbRaw::Array(vec![OvbRaw::Tag(
                        37,
                        Box::new(OvbRaw::Bytes(KEY_FIELD.as_bytes().to_vec())),
                    )]),
                ),
                (
                    OvbRaw::Int(3.into()),
                    OvbRaw::Array(vec![OvbRaw::Array(vec![
                        OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(KEY_FIELD.as_bytes().to_vec()))),
                        OvbRaw::Text(format!("f_{}", KEY_FIELD.simple())),
                        OvbRaw::Array(vec![OvbRaw::Int(0.into()), OvbRaw::Text("Int".into())]),
                        OvbRaw::Int(0.into()),
                        OvbRaw::Array(vec![OvbRaw::Int(0.into())]),
                    ])]),
                ),
                (OvbRaw::Int(4.into()), OvbRaw::Array(Vec::new())),
            ]))
            .unwrap(),
        )
        .unwrap()
    }

    fn fixture_row(value: i64) -> (Vec<u8>, Vec<u8>) {
        let key = CanonicalValue::new(OvbRaw::Int(value.into()))
            .unwrap()
            .encode()
            .unwrap();
        let row = CanonicalValue::new(OvbRaw::Tag(
            60009,
            Box::new(OvbRaw::Array(vec![
                OvbRaw::Null,
                OvbRaw::Array(vec![OvbRaw::Array(vec![
                    OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(KEY_FIELD.as_bytes().to_vec()))),
                    OvbRaw::Int(value.into()),
                ])]),
            ])),
        ))
        .unwrap()
        .encode()
        .unwrap();
        (key, row)
    }

    #[test]
    fn consolidation_requires_material_overlay_count_and_byte_share() {
        assert!(!overlay_pressure_is_material(3, 100, 50));
        assert!(!overlay_pressure_is_material(4, 100, 24));
        assert!(overlay_pressure_is_material(4, 100, 25));
        assert!(overlay_pressure_is_material(8, 400, 100));
    }

    #[test]
    fn consolidation_output_verifier_requires_exact_logical_rows() {
        let profile = profile();
        let (key, value) = fixture_row(17);
        let plan = CompactConsolidationPlan {
            table: TABLE,
            schema: profile.schema_fingerprint(),
            generation: 9,
            input_segments: Vec::new(),
            input_compressed_bytes: 0,
            overlay_segments: MIN_OVERLAY_SEGMENTS_FOR_CONSOLIDATION,
            overlay_compressed_bytes: 0,
            estimated_output_bytes: 20,
            logical_digest: digest_logical_rows(&[ConsolidatedRow {
                key: key.clone(),
                value: value.clone(),
            }])
            .unwrap(),
            expected_rows: vec![ConsolidatedRow {
                key: key.clone(),
                value: value.clone(),
            }],
        };

        assert_eq!(
            plan.verify_candidate(&profile, [(key.clone(), value.clone())])
                .unwrap()
                .semantic_diff_entries(),
            0
        );
        assert_eq!(
            plan.verify_candidate(&profile, [(key, fixture_row(18).1)]),
            Err(CompactConsolidationError::SemanticChange)
        );
    }
}
