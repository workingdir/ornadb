//! Compact manifest scan planning for the table/query planner.
//!
//! This storage hook prunes immutable files using schema-typed primary-key
//! bounds and builds a stable-identity projection. Row-group/page statistics
//! remain a reader responsibility; missing or untrusted statistics must
//! widen the scan. This deterministic hook is not the chapter-23 production
//! benchmark/fault profile and does not establish the production claim.

use std::cmp::Ordering;

use orna_repository_v1::{CompactManifest, Uuid};

use crate::{CompactKeyIdentity, CompactOvbProfile};

const MANIFEST_SHARD_ENTRIES: usize = 256;

/// Optional inclusive/exclusive primary-key bounds for a compact scan.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CompactKeyRange {
    lower: Option<(CompactKeyIdentity, bool)>,
    upper: Option<(CompactKeyIdentity, bool)>,
}

impl CompactKeyRange {
    pub fn unbounded() -> Self {
        Self::default()
    }

    pub fn new(
        profile: &CompactOvbProfile,
        lower: Option<(Vec<u8>, bool)>,
        upper: Option<(Vec<u8>, bool)>,
    ) -> Result<Self, CompactScanPlanError> {
        let lower = lower
            .map(|(key, inclusive)| {
                profile
                    .decode_key(&key)
                    .map(|key| (key, inclusive))
                    .map_err(|_| CompactScanPlanError::InvalidKeyBound)
            })
            .transpose()?;
        let upper = upper
            .map(|(key, inclusive)| {
                profile
                    .decode_key(&key)
                    .map(|key| (key, inclusive))
                    .map_err(|_| CompactScanPlanError::InvalidKeyBound)
            })
            .transpose()?;
        if let (Some((lower_key, lower_inclusive)), Some((upper_key, upper_inclusive))) =
            (&lower, &upper)
        {
            match lower_key.cmp(upper_key) {
                Ordering::Greater => return Err(CompactScanPlanError::ReversedRange),
                Ordering::Equal if !lower_inclusive || !upper_inclusive => {
                    return Err(CompactScanPlanError::EmptyRange)
                }
                _ => {}
            }
        }
        Ok(Self { lower, upper })
    }

    pub fn lower(&self) -> Option<(&CompactKeyIdentity, bool)> {
        self.lower.as_ref().map(|(key, inclusive)| (key, *inclusive))
    }

    pub fn upper(&self) -> Option<(&CompactKeyIdentity, bool)> {
        self.upper.as_ref().map(|(key, inclusive)| (key, *inclusive))
    }
}

/// A compact scan plan consumed by a logical table planner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompactScanPlan {
    table: Uuid,
    schema: [u8; 32],
    next_generation: u64,
    selected_shards: Vec<u64>,
    pruned_shards: Vec<u64>,
    selected_segments: Vec<Uuid>,
    pruned_segments: Vec<Uuid>,
    projected_field_ids: Vec<[u8; 16]>,
    range: CompactKeyRange,
}

impl CompactScanPlan {
    pub fn table(&self) -> Uuid {
        self.table
    }

    pub const fn next_generation(&self) -> u64 {
        self.next_generation
    }

    pub const fn schema(&self) -> [u8; 32] {
        self.schema
    }

    pub fn selected_shards(&self) -> &[u64] {
        &self.selected_shards
    }

    pub fn pruned_shards(&self) -> &[u64] {
        &self.pruned_shards
    }

    pub fn selected_segments(&self) -> &[Uuid] {
        &self.selected_segments
    }

    pub fn pruned_segments(&self) -> &[Uuid] {
        &self.pruned_segments
    }

    /// Requested stored fields plus every primary-key field, in stable ID order.
    pub fn projected_field_ids(&self) -> &[[u8; 16]] {
        &self.projected_field_ids
    }

    pub fn range(&self) -> &CompactKeyRange {
        &self.range
    }

    /// Tests verified row-group or page primary-key bounds against this scan.
    /// Missing, malformed, or reversed physical statistics conservatively
    /// retain the scope. Readers can use this same hook at either level.
    pub fn may_match_primary_key_scope(
        &self,
        profile: &CompactOvbProfile,
        min_key: Option<&[u8]>,
        max_key: Option<&[u8]>,
    ) -> bool {
        if profile.table_id() != *self.table.as_bytes()
            || profile.schema_fingerprint() != self.schema
        {
            return true;
        }
        let (Some(min_key), Some(max_key)) = (min_key, max_key) else {
            return true;
        };
        let (Ok(min), Ok(max)) = (profile.decode_key(min_key), profile.decode_key(max_key)) else {
            return true;
        };
        if min > max {
            return true;
        }
        key_bounds_overlap(&min, &max, &self.range)
    }
}

/// Selects only manifest entries whose typed primary-key bounds may overlap
/// the query. Projection is expressed in stable field IDs rather than names.
pub fn plan_compact_scan(
    profile: &CompactOvbProfile,
    manifest: &CompactManifest,
    range: CompactKeyRange,
    required_field_ids: impl IntoIterator<Item = [u8; 16]>,
) -> Result<CompactScanPlan, CompactScanPlanError> {
    if manifest.table().as_bytes() != &profile.table_id() {
        return Err(CompactScanPlanError::WrongTable);
    }
    if manifest.schema() != profile.schema_fingerprint() {
        return Err(CompactScanPlanError::WrongSchema);
    }
    let mut projected = required_field_ids.into_iter().collect::<std::collections::BTreeSet<_>>();
    projected.extend(profile.key_field_ids());

    let mut decoded_bounds = Vec::with_capacity(manifest.entries().len());
    for entry in manifest.entries() {
        let min = profile
            .decode_key(entry.min_key())
            .map_err(|_| CompactScanPlanError::InvalidManifestBounds)?;
        let max = profile
            .decode_key(entry.max_key())
            .map_err(|_| CompactScanPlanError::InvalidManifestBounds)?;
        if min > max {
            return Err(CompactScanPlanError::InvalidManifestBounds);
        }
        decoded_bounds.push((entry, min, max));
    }

    let mut selected_shards = Vec::new();
    let mut pruned_shards = Vec::new();
    let mut selected_segments = Vec::new();
    let mut pruned_segments = Vec::new();
    for (shard_number, shard_entries) in decoded_bounds.chunks(MANIFEST_SHARD_ENTRIES).enumerate() {
        let Some((_, first_min, first_max)) = shard_entries.first() else {
            continue;
        };
        let (shard_min, shard_max) = shard_entries.iter().skip(1).fold(
            (first_min.clone(), first_max.clone()),
            |(min, max), (_, entry_min, entry_max)| {
                (
                    if entry_min < &min { (*entry_min).clone() } else { min },
                    if entry_max > &max { (*entry_max).clone() } else { max },
                )
            },
        );
        let shard_may_match = key_bounds_overlap(&shard_min, &shard_max, &range);
        let shard_number = u64::try_from(shard_number)
            .map_err(|_| CompactScanPlanError::InvalidManifestBounds)?;
        if shard_may_match {
            selected_shards.push(shard_number);
        } else {
            pruned_shards.push(shard_number);
        }
        for (entry, min, max) in shard_entries {
            let overlaps = key_bounds_overlap(min, max, &range);
            if overlaps {
                selected_segments.push(entry.segment_id());
            } else {
                pruned_segments.push(entry.segment_id());
            }
        }
    }
    selected_segments.sort_by_key(|id| id.as_bytes().to_owned());
    pruned_segments.sort_by_key(|id| id.as_bytes().to_owned());

    Ok(CompactScanPlan {
        table: manifest.table(),
        schema: manifest.schema(),
        next_generation: manifest.next_generation(),
        selected_shards,
        pruned_shards,
        selected_segments,
        pruned_segments,
        projected_field_ids: projected.into_iter().collect(),
        range,
    })
}

fn key_bounds_overlap(
    min: &CompactKeyIdentity,
    max: &CompactKeyIdentity,
    range: &CompactKeyRange,
) -> bool {
    let before_lower = range.lower.as_ref().is_some_and(|(lower, inclusive)| {
        match max.cmp(lower) {
            Ordering::Less => true,
            Ordering::Equal => !inclusive,
            Ordering::Greater => false,
        }
    });
    let after_upper = range.upper.as_ref().is_some_and(|(upper, inclusive)| {
        match min.cmp(upper) {
            Ordering::Greater => true,
            Ordering::Equal => !inclusive,
            Ordering::Less => false,
        }
    });
    !before_lower && !after_upper
}

/// A fail-closed compact query plan error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompactScanPlanError {
    InvalidKeyBound,
    ReversedRange,
    EmptyRange,
    WrongTable,
    WrongSchema,
    InvalidManifestBounds,
}

impl std::fmt::Display for CompactScanPlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidKeyBound => "compact scan has a key bound invalid for the schema",
            Self::ReversedRange => "compact scan key range is reversed",
            Self::EmptyRange => "compact scan key range is empty",
            Self::WrongTable => "compact scan manifest belongs to another table",
            Self::WrongSchema => "compact scan manifest belongs to another schema",
            Self::InvalidManifestBounds => "compact scan manifest has invalid key bounds",
        })
    }
}

impl std::error::Error for CompactScanPlanError {}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_foundation_v1::{CanonicalValue, OvbRaw, SchemaDescriptor};

    const KEY_FIELD: Uuid =
        Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0001);

    fn profile() -> CompactOvbProfile {
        let table = Uuid::from_u128(1);
        CompactOvbProfile::new(
            SchemaDescriptor::new(OvbRaw::Map(vec![
                (OvbRaw::Int(0.into()), OvbRaw::Int(1.into())),
                (
                    OvbRaw::Int(1.into()),
                    OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(table.as_bytes().to_vec()))),
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

    fn key(profile: &CompactOvbProfile, value: i64) -> CompactKeyIdentity {
        let bytes = CanonicalValue::new(OvbRaw::Int(value.into()))
            .unwrap()
            .encode()
            .unwrap();
        profile.decode_key(&bytes).unwrap()
    }

    #[test]
    fn manifest_bound_pruning_obeys_inclusive_typed_key_ranges() {
        let profile = profile();
        let range = CompactKeyRange {
            lower: Some((key(&profile, 2), true)),
            upper: Some((key(&profile, 5), false)),
        };
        assert!(!key_bounds_overlap(
            &key(&profile, 0),
            &key(&profile, 1),
            &range
        ));
        assert!(key_bounds_overlap(
            &key(&profile, 2),
            &key(&profile, 3),
            &range
        ));
        assert!(!key_bounds_overlap(
            &key(&profile, 5),
            &key(&profile, 8),
            &range
        ));
    }
}
