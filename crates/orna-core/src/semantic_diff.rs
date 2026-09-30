//! Snapshot-level semantic differences for catalogue, keyed data, results,
//! dependencies, and compact-storage observations.
//!
//! Callers provide complete immutable observations for both sides. This layer
//! does not open a repository or infer missing row identity; keyed rows use
//! canonical OVB keys and caller-visible stable catalogue identities.

use crate::{
    FieldId, FunctionId, ParameterId, SchemaId, TypeId,
    catalogue::CatalogueSnapshot,
    catalogue_diff::{CatalogueSemanticDiff, catalogue_diff},
};
use orna_value_v1::Value as CanonicalValue;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
};

/// A semantic catalogue entity that may participate in a dependency edge.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SemanticEntityId {
    /// A declared schema.
    Schema(SchemaId),
    /// An object or value type.
    Type(TypeId),
    /// A function.
    Function(FunctionId),
    /// A field on an object or record value type.
    Field(FieldId),
    /// A function parameter.
    Parameter(ParameterId),
}

/// A directed semantic dependency from a dependent entity to the entity it uses.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DependencyEdge {
    dependent: SemanticEntityId,
    dependency: SemanticEntityId,
}

impl DependencyEdge {
    /// Creates one exact dependency observation.
    pub const fn new(
        dependent: SemanticEntityId,
        dependency: SemanticEntityId,
    ) -> Self {
        Self {
            dependent,
            dependency,
        }
    }

    /// Returns the entity whose behavior or contents depend on `dependency`.
    pub const fn dependent(self) -> SemanticEntityId {
        self.dependent
    }

    /// Returns the entity used by `dependent`.
    pub const fn dependency(self) -> SemanticEntityId {
        self.dependency
    }
}

/// One persisted row identified by its stable table identity and canonical key.
#[derive(Clone)]
pub struct KeyedRow {
    table: TypeId,
    key: Vec<u8>,
    value_digest: [u8; 32],
}

impl KeyedRow {
    /// Creates an observation from canonical key and row values.
    pub fn new(
        table: TypeId,
        key: &CanonicalValue,
        value: &CanonicalValue,
    ) -> Result<Self, SemanticSnapshotError> {
        Ok(Self {
            table,
            key: key.encode().map_err(SemanticSnapshotError::ValueEncoding)?,
            value_digest: canonical_digest(b"row-value", value)?,
        })
    }

    /// Returns the stable table identity.
    pub const fn table(&self) -> TypeId {
        self.table
    }

    /// Returns the canonical OVB encoding of this row's natural key.
    pub fn key_bytes(&self) -> &[u8] {
        &self.key
    }
}

impl fmt::Debug for KeyedRow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("KeyedRow")
            .field("table", &self.table)
            .field("key", &"<redacted>")
            .field("value_digest", &self.value_digest)
            .finish()
    }
}

/// An explicit semantic re-key intent for one stable table identity.
///
/// Re-key intent is supplied by the activation ledger. The diff never infers
/// identity continuity from a matching row body or a raw path rename.
#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
pub struct RowRekey {
    table: TypeId,
    old_key: Vec<u8>,
    new_key: Vec<u8>,
}

impl RowRekey {
    /// Creates a re-key intent from canonical logical key values.
    pub fn new(
        table: TypeId,
        old_key: &CanonicalValue,
        new_key: &CanonicalValue,
    ) -> Result<Self, SemanticSnapshotError> {
        let old_key = old_key
            .encode()
            .map_err(SemanticSnapshotError::ValueEncoding)?;
        let new_key = new_key
            .encode()
            .map_err(SemanticSnapshotError::ValueEncoding)?;
        if old_key == new_key {
            return Err(SemanticSnapshotError::IdentityRekey);
        }
        Ok(Self {
            table,
            old_key,
            new_key,
        })
    }

    /// Returns the stable table identity.
    pub const fn table(&self) -> TypeId {
        self.table
    }

    /// Returns the old canonical key bytes.
    pub fn old_key_bytes(&self) -> &[u8] {
        &self.old_key
    }

    /// Returns the new canonical key bytes.
    pub fn new_key_bytes(&self) -> &[u8] {
        &self.new_key
    }
}

impl fmt::Debug for RowRekey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RowRekey")
            .field("table", &self.table)
            .field("old_key", &"<redacted>")
            .field("new_key", &"<redacted>")
            .finish()
    }
}

/// One function result observation, keyed by function identity and arguments.
///
/// The specification asks for result changes where available but leaves result
/// correlation open; this API matches equal canonical arguments for one stable
/// function identity, then compares canonical result digests.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResultObservation {
    function: FunctionId,
    arguments_digest: [u8; 32],
    result_digest: [u8; 32],
}

impl ResultObservation {
    /// Creates a result observation from canonical arguments and output.
    pub fn new(
        function: FunctionId,
        arguments: &CanonicalValue,
        result: &CanonicalValue,
    ) -> Result<Self, SemanticSnapshotError> {
        Ok(Self {
            function,
            arguments_digest: canonical_digest(b"result-arguments", arguments)?,
            result_digest: canonical_digest(b"result-value", result)?,
        })
    }

    /// Returns the stable function identity.
    pub const fn function(&self) -> FunctionId {
        self.function
    }
}

/// The physical compact-storage facts known for one snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompactStorageObservation {
    /// The compact manifest generation, when the snapshot has one.
    pub generation: u64,
    /// A stable digest of physical representation metadata, when available.
    pub representation_digest: Option<[u8; 32]>,
}

/// An immutable semantic snapshot supplied by a repository or runtime adapter.
pub struct SemanticSnapshot {
    catalogue: CatalogueSnapshot,
    rows: BTreeMap<(TypeId, Vec<u8>), [u8; 32]>,
    results: BTreeMap<(FunctionId, [u8; 32]), [u8; 32]>,
    dependencies: BTreeSet<DependencyEdge>,
    compact_storage: Option<CompactStorageObservation>,
    // Re-key intents are an ordered activation log. Keeping source order is
    // necessary when a freed key is reused by a different row in that same
    // activation.
    rekeys: Vec<RowRekey>,
}

impl SemanticSnapshot {
    /// Creates a snapshot and rejects duplicate logical row or result identities.
    pub fn new(
        catalogue: CatalogueSnapshot,
        rows: impl IntoIterator<Item = KeyedRow>,
        results: impl IntoIterator<Item = ResultObservation>,
        dependencies: impl IntoIterator<Item = DependencyEdge>,
        compact_storage: Option<CompactStorageObservation>,
    ) -> Result<Self, SemanticSnapshotError> {
        let mut row_map = BTreeMap::new();
        for row in rows {
            let key = (row.table, row.key);
            if row_map.insert(key, row.value_digest).is_some() {
                return Err(SemanticSnapshotError::DuplicateRow);
            }
        }

        let mut result_map = BTreeMap::new();
        for result in results {
            let key = (result.function, result.arguments_digest);
            if result_map.insert(key, result.result_digest).is_some() {
                return Err(SemanticSnapshotError::DuplicateResult);
            }
        }

        // Dependency facts are set-valued; repeated observations do not create
        // additional semantic edges.
        let dependencies = dependencies.into_iter().collect();

        Ok(Self {
            catalogue,
            rows: row_map,
            results: result_map,
            dependencies,
            compact_storage,
            rekeys: Vec::new(),
        })
    }

    /// Attaches exact re-key intents retained by the candidate activation.
    pub fn with_rekeys(
        mut self,
        rekeys: impl IntoIterator<Item = RowRekey>,
    ) -> Result<Self, SemanticSnapshotError> {
        for rekey in rekeys {
            self.rekeys.push(rekey);
        }
        Ok(self)
    }
}

/// Whether a keyed row was added, removed, updated, or explicitly re-keyed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RowChangeKind {
    /// The key appears only in the candidate snapshot.
    Added,
    /// The key appears only in the base snapshot.
    Removed,
    /// The key appears in both snapshots with different canonical values.
    Updated,
    /// An explicit pending re-key moved the same row from `previous_key`.
    Rekeyed,
}

/// A row change retaining the stable table identity and canonical natural key.
#[derive(Clone, Eq, PartialEq)]
pub struct RowChange {
    table: TypeId,
    key: Vec<u8>,
    previous_key: Option<Vec<u8>>,
    kind: RowChangeKind,
}

impl RowChange {
    /// Returns the table identity.
    pub const fn table(&self) -> TypeId {
        self.table
    }

    /// Returns the canonical OVB encoding of the natural key.
    pub fn key_bytes(&self) -> &[u8] {
        &self.key
    }

    /// Returns the old canonical key for an explicit re-key change.
    pub fn previous_key_bytes(&self) -> Option<&[u8]> {
        self.previous_key.as_deref()
    }

    /// Returns the row change category.
    pub const fn kind(&self) -> RowChangeKind {
        self.kind
    }
}

impl fmt::Debug for RowChange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RowChange")
            .field("table", &self.table)
            .field("key", &"<redacted>")
            .field("previous_key", &self.previous_key.as_ref().map(|_| "<redacted>"))
            .field("kind", &self.kind)
            .finish()
    }
}

/// Whether a result observation was added, removed, or changed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResultChangeKind {
    /// The function and argument identity appear only in the candidate.
    Added,
    /// The function and argument identity appear only in the base.
    Removed,
    /// The function returned a different canonical result for the same arguments.
    Changed,
}

/// One changed function result keyed by function and a digest of its arguments.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResultChange {
    function: FunctionId,
    arguments_digest: [u8; 32],
    kind: ResultChangeKind,
}

impl ResultChange {
    /// Returns the function identity.
    pub const fn function(&self) -> FunctionId {
        self.function
    }

    /// Returns the stable digest used to match equal function arguments.
    pub const fn arguments_digest(&self) -> [u8; 32] {
        self.arguments_digest
    }

    /// Returns the result change category.
    pub const fn kind(&self) -> ResultChangeKind {
        self.kind
    }
}

/// Whether a dependency edge was added or removed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DependencyChangeKind {
    /// The edge appears only in the candidate snapshot.
    Added,
    /// The edge appears only in the base snapshot.
    Removed,
}

/// A dependency graph edge change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DependencyChange {
    edge: DependencyEdge,
    kind: DependencyChangeKind,
}

impl DependencyChange {
    /// Returns the changed edge.
    pub const fn edge(self) -> DependencyEdge {
        self.edge
    }

    /// Returns whether the edge was added or removed.
    pub const fn kind(self) -> DependencyChangeKind {
        self.kind
    }
}

/// Physical changes observed independently from catalogue or logical data changes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhysicalDiff {
    /// The before and after compact generations, when both were observed and differ.
    pub compact_generation: Option<(u64, u64)>,
    /// Whether representation fingerprints differ; `None` means they were not both available.
    pub representation_changed: Option<bool>,
    /// Whether any catalogue, row, result, or dependency change was also found.
    pub logical_changes_present: bool,
}

impl PhysicalDiff {
    /// Returns true when this snapshot pair differs only in physical metadata.
    pub const fn is_physical_only(self) -> bool {
        !self.logical_changes_present
    }
}

/// Complete semantic and physical report for a pair of snapshots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticDiffReport {
    catalogue: CatalogueSemanticDiff,
    rows: Vec<RowChange>,
    results: Vec<ResultChange>,
    dependencies: Vec<DependencyChange>,
    physical: Option<PhysicalDiff>,
}

impl SemanticDiffReport {
    /// Returns schema, type, field, function, and parameter changes.
    pub fn catalogue(&self) -> &CatalogueSemanticDiff {
        &self.catalogue
    }

    /// Returns keyed-row additions, removals, and updates in canonical key order.
    pub fn rows(&self) -> &[RowChange] {
        &self.rows
    }

    /// Returns result changes ordered by function and argument digest.
    pub fn results(&self) -> &[ResultChange] {
        &self.results
    }

    /// Returns dependency additions and removals in stable identity order.
    pub fn dependencies(&self) -> &[DependencyChange] {
        &self.dependencies
    }

    /// Returns physical metadata changes, separate from logical changes.
    pub const fn physical(&self) -> Option<PhysicalDiff> {
        self.physical
    }

    /// Returns true when catalogue, keyed-row, result, or dependency state changed.
    pub fn has_logical_changes(&self) -> bool {
        !self.catalogue.is_empty()
            || !self.rows.is_empty()
            || !self.results.is_empty()
            || !self.dependencies.is_empty()
    }

    /// Returns true when there are no logical or observed physical changes.
    pub fn is_empty(&self) -> bool {
        !self.has_logical_changes() && self.physical.is_none()
    }

    /// Returns true when observed physical metadata changed without logical changes.
    pub fn is_physical_only(&self) -> bool {
        self.physical.is_some_and(PhysicalDiff::is_physical_only)
    }
}

/// Computes a deterministic semantic report from two immutable snapshots.
pub fn semantic_snapshot_diff(
    base: &SemanticSnapshot,
    candidate: &SemanticSnapshot,
) -> SemanticDiffReport {
    let catalogue = catalogue_diff(&base.catalogue, &candidate.catalogue);
    let rows = diff_rows(&base.rows, &candidate.rows, &candidate.rekeys);
    let results = diff_results(&base.results, &candidate.results);
    let dependencies = diff_dependencies(&base.dependencies, &candidate.dependencies);
    let logical_changes_present = !catalogue.is_empty()
        || !rows.is_empty()
        || !results.is_empty()
        || !dependencies.is_empty();

    // Generations can advance during compaction without changing logical
    // contents, so compare this physical coordinate independently below.
    let compact_generation = match (base.compact_storage, candidate.compact_storage) {
        (Some(before), Some(after)) if before.generation != after.generation => {
            Some((before.generation, after.generation))
        }
        _ => None,
    };
    let representation_changed = match (base.compact_storage, candidate.compact_storage) {
        (Some(before), Some(after)) => before
            .representation_digest
            .zip(after.representation_digest)
            .map(|(before, after)| before != after),
        _ => None,
    };
    let physical = (compact_generation.is_some() || representation_changed == Some(true))
        .then_some(PhysicalDiff {
            compact_generation,
            representation_changed,
            logical_changes_present,
        });

    SemanticDiffReport {
        catalogue,
        rows,
        results,
        dependencies,
        physical,
    }
}

fn diff_rows(
    base: &BTreeMap<(TypeId, Vec<u8>), [u8; 32]>,
    candidate: &BTreeMap<(TypeId, Vec<u8>), [u8; 32]>,
    rekeys: &[RowRekey],
) -> Vec<RowChange> {
    // Carry each base row's stable identity through the ordered activation
    // log. A key can be reused only after its previous row has moved, so an
    // unordered edge graph would incorrectly collapse e.g. 2→3, then 1→2.
    let mut current_identity = base
        .keys()
        .cloned()
        .map(|identity| (identity.clone(), identity))
        .collect::<BTreeMap<_, _>>();
    for rekey in rekeys {
        let old = (rekey.table, rekey.old_key.clone());
        let new = (rekey.table, rekey.new_key.clone());
        if current_identity.contains_key(&new) {
            // A stale or invalid intent cannot displace another live row.
            // The ordinary keyed diff below remains the conservative result.
            continue;
        }
        if let Some(origin) = current_identity.remove(&old) {
            current_identity.insert(new, origin);
        }
    }

    let mut consumed = BTreeSet::new();
    let mut changes = Vec::new();
    for (current, origin) in current_identity {
        if origin != current && candidate.contains_key(&current) {
            // The activation ledger is explicit evidence of continuity, even
            // when an earlier move frees a key that another row then occupies.
            // A terminal delete has no candidate row and stays a removal.
            consumed.insert(origin.clone());
            consumed.insert(current.clone());
            changes.push(RowChange {
                table: current.0,
                key: current.1,
                previous_key: Some(origin.1),
                kind: RowChangeKind::Rekeyed,
            });
        }
    }
    let keys = base
        .keys()
        .chain(candidate.keys())
        .cloned()
        .collect::<BTreeSet<_>>();

    let ordinary = keys
        .into_iter()
        .filter_map(|(table, key)| {
            if consumed.contains(&(table, key.clone())) {
                return None;
            }
            let kind = match (base.get(&(table, key.clone())), candidate.get(&(table, key.clone()))) {
                (None, Some(_)) => Some(RowChangeKind::Added),
                (Some(_), None) => Some(RowChangeKind::Removed),
                (Some(before), Some(after)) if before != after => Some(RowChangeKind::Updated),
                _ => None,
            }?;
            Some(RowChange {
                table,
                key,
                previous_key: None,
                kind,
            })
        })
        .collect::<Vec<_>>();
    changes.extend(ordinary);
    changes.sort_by(|left, right| (left.table, &left.key).cmp(&(right.table, &right.key)));
    changes
}

fn diff_results(
    base: &BTreeMap<(FunctionId, [u8; 32]), [u8; 32]>,
    candidate: &BTreeMap<(FunctionId, [u8; 32]), [u8; 32]>,
) -> Vec<ResultChange> {
    let keys = base
        .keys()
        .chain(candidate.keys())
        .copied()
        .collect::<BTreeSet<_>>();

    keys.into_iter()
        .filter_map(|(function, arguments_digest)| {
            let key = (function, arguments_digest);
            let kind = match (base.get(&key), candidate.get(&key)) {
                (None, Some(_)) => Some(ResultChangeKind::Added),
                (Some(_), None) => Some(ResultChangeKind::Removed),
                (Some(before), Some(after)) if before != after => Some(ResultChangeKind::Changed),
                _ => None,
            }?;
            Some(ResultChange {
                function,
                arguments_digest,
                kind,
            })
        })
        .collect()
}

fn diff_dependencies(
    base: &BTreeSet<DependencyEdge>,
    candidate: &BTreeSet<DependencyEdge>,
) -> Vec<DependencyChange> {
    base.difference(candidate)
        .copied()
        .map(|edge| DependencyChange {
            edge,
            kind: DependencyChangeKind::Removed,
        })
        .chain(
            candidate
                .difference(base)
                .copied()
                .map(|edge| DependencyChange {
                    edge,
                    kind: DependencyChangeKind::Added,
                }),
        )
        .collect()
}

fn canonical_digest(
    domain: &'static [u8],
    value: &CanonicalValue,
) -> Result<[u8; 32], SemanticSnapshotError> {
    let encoded = value
        .encode()
        .map_err(SemanticSnapshotError::ValueEncoding)?;
    let mut hasher = Sha256::new();
    hasher.update(b"orna-semantic-diff-v1\0");
    hasher.update(domain);
    hasher.update(encoded);
    Ok(hasher.finalize().into())
}

/// An invalid or ambiguous observation rejected while building a semantic snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticSnapshotError {
    /// A canonical OVB value could not be encoded.
    ValueEncoding(orna_value_v1::Error),
    /// A table and natural key appeared more than once.
    DuplicateRow,
    /// A function and canonical argument identity appeared more than once.
    DuplicateResult,
    /// A re-key intent mapped a row to its existing key.
    IdentityRekey,
    /// The same re-key intent appeared more than once.
    DuplicateRekey,
}

impl fmt::Display for SemanticSnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ValueEncoding(_) => formatter.write_str("semantic diff value is not encodable"),
            Self::DuplicateRow => formatter.write_str("semantic snapshot repeats a keyed row"),
            Self::DuplicateResult => {
                formatter.write_str("semantic snapshot repeats a function result identity")
            }
            Self::IdentityRekey => formatter.write_str("semantic snapshot re-key kept its key"),
            Self::DuplicateRekey => formatter.write_str("semantic snapshot repeats a re-key intent"),
        }
    }
}

impl Error for SemanticSnapshotError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ValueEncoding(error) => Some(error),
            Self::DuplicateRow
            | Self::DuplicateResult
            | Self::IdentityRekey
            | Self::DuplicateRekey => None,
        }
    }
}
