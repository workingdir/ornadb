//! Immutable compact-storage exact-key validation.
//!
//! The repository crate owns the committed compact manifest and physical
//! Parquet witness. This module does not claim to implement the complete
//! compact row decoder. It owns the production exact-key boundary that the
//! table/runtime layer already uses: canonical OVB scalar keys and canonical
//! OVB tuple keys. A physical reader can feed its decoded key components into
//! the immutable index here; no range overlap or sampled row is treated as an
//! exact key.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use orna_foundation_v1::{CanonicalValue, OvbRaw, SchemaDescriptor};
use orna_evolution_v1::{MigrationOperation, MigrationPlan};
use orna_repository_v1::{
    CompactCommittedSegmentProjection, CompactManifest, CompactManifestEntry, CompactSegmentRole,
};
use orna_runtime_v1::{
    PublicationFreeze, PublicationMutationState, PublicationRowEncoding,
    PublicationValueEncoding,
};
use sha2::{Digest, Sha256};

/// Immutable profile coordinates used at this exact-key boundary.
pub const COMPACT_STORAGE_PROFILE: &str = "compact-storage-v1";
pub const OVB_PROFILE: &str = "OVB-1";

/// Typed mutation state accepted by the compact writer boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompactWriterMutationState {
    Replacement { value: Vec<u8> },
    Deletion,
}

/// One schema-validated, ordered mutation ready for compact encoding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompactWriterMutation {
    pub sequence: u64,
    pub mutation_id: [u8; 16],
    pub key: CompactKeyIdentity,
    pub state: CompactWriterMutationState,
}

/// Deterministic, inspectable input for a compact writer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompactWriterInput {
    pub table_id: [u8; 16],
    pub schema_fingerprint: [u8; 32],
    pub candidate_generation: u64,
    pub row_encoding_identity: PublicationRowEncoding,
    pub value_encoding_identity: PublicationValueEncoding,
    pub mutations: Vec<CompactWriterMutation>,
    pub candidate_digest: [u8; 32],
}

/// One complete logical row folded from a verified committed segment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompactBaseRow {
    key: CanonicalValue,
    value: Option<CanonicalValue>,
    generation: u64,
    role: CompactSegmentRole,
}

impl CompactBaseRow {
    pub fn key(&self) -> &CanonicalValue {
        &self.key
    }

    pub fn value(&self) -> Option<&CanonicalValue> {
        self.value.as_ref()
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub const fn role(&self) -> CompactSegmentRole {
        self.role
    }
}

/// The schema-bound logical base state consumed before compact publication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompactBaseState {
    table_id: [u8; 16],
    schema_fingerprint: [u8; 32],
    rows: BTreeMap<CompactKeyIdentity, CompactBaseRow>,
}

impl CompactBaseState {
    pub const fn table_id(&self) -> [u8; 16] {
        self.table_id
    }

    pub const fn schema_fingerprint(&self) -> [u8; 32] {
        self.schema_fingerprint
    }

    pub fn rows(&self) -> impl Iterator<Item = &CompactBaseRow> {
        self.rows.values()
    }

    /// Folds a digest-verified ordered candidate against this committed base.
    ///
    /// A final deletion is omitted only when the verified base proves the key
    /// was absent. The candidate digest remains that of the original ordered
    /// freeze, not this compacted writer representation.
    pub fn fold_writer_input(
        &self,
        input: &CompactWriterInput,
    ) -> Result<CompactWriterInput, CompactBaseProjectionError> {
        self.validate_writer_input_identity(input)?;
        let mut latest = BTreeMap::new();
        for mutation in &input.mutations {
            latest.insert(mutation.key.clone(), mutation.clone());
        }
        let mut mutations: Vec<_> = latest
            .into_values()
            .filter(|mutation| match &mutation.state {
                CompactWriterMutationState::Replacement { .. } => true,
                CompactWriterMutationState::Deletion => self
                    .rows
                    .get(&mutation.key)
                    .is_some_and(|row| row.value.is_some()),
            })
            .collect();
        mutations.sort_by_key(|mutation| mutation.sequence);
        let folded = CompactWriterInput {
            table_id: input.table_id,
            schema_fingerprint: input.schema_fingerprint,
            candidate_generation: input.candidate_generation,
            row_encoding_identity: input.row_encoding_identity,
            value_encoding_identity: input.value_encoding_identity,
            mutations,
            candidate_digest: input.candidate_digest,
        };
        self.validate_writer_input(&folded)?;
        Ok(folded)
    }

    /// Consumes generated writer input at the schema/generation boundary used
    /// by the committed base provider.
    pub fn consume_writer_input(
        &self,
        input: &CompactWriterInput,
    ) -> Result<(), CompactBaseProjectionError> {
        self.validate_writer_input(input)
    }

    fn validate_writer_input(
        &self,
        input: &CompactWriterInput,
    ) -> Result<(), CompactBaseProjectionError> {
        self.validate_writer_input_identity(input)?;
        let mut keys = BTreeSet::new();
        for mutation in &input.mutations {
            if !keys.insert(mutation.key.clone()) {
                return Err(CompactBaseProjectionError::DuplicateKeyGeneration);
            }
        }
        Ok(())
    }

    fn validate_writer_input_identity(
        &self,
        input: &CompactWriterInput,
    ) -> Result<(), CompactBaseProjectionError> {
        if input.table_id != self.table_id {
            return Err(CompactBaseProjectionError::WrongTable);
        }
        if input.schema_fingerprint != self.schema_fingerprint {
            return Err(CompactBaseProjectionError::WrongSchema);
        }
        let maximum_generation = self
            .rows
            .values()
            .map(CompactBaseRow::generation)
            .max()
            .unwrap_or(0);
        let next_generation = maximum_generation
            .checked_add(1)
            .ok_or(CompactBaseProjectionError::StaleGeneration)?;
        if input.candidate_generation != next_generation {
            return Err(CompactBaseProjectionError::StaleGeneration);
        }
        Ok(())
    }
}

/// Applies an already-authorized evolution plan to the verified compact base.
/// Schema operations without a physical projection are rejected; rekeys retain
/// the complete canonical row value and emit typed writer mutations.
pub fn apply_migration_plan_to_compact(
    profile: &CompactOvbProfile,
    base: &CompactBaseState,
    plan: &MigrationPlan,
    candidate_generation: u64,
    mutation_ids: &[[u8; 16]],
    candidate_digest: [u8; 32],
) -> Result<CompactWriterInput, CompactBaseProjectionError> {
    let expected_mutation_ids = plan
        .operations()
        .len()
        .checked_mul(2)
        .ok_or(CompactBaseProjectionError::EvolutionInputMismatch)?;
    if mutation_ids.len() != expected_mutation_ids || candidate_generation == 0 {
        return Err(CompactBaseProjectionError::EvolutionInputMismatch);
    }
    let mut mutations = Vec::with_capacity(expected_mutation_ids);
    let mut mutation_id_set = BTreeSet::new();
    let mut targets = BTreeSet::new();
    for (index, (operation, ids)) in plan
        .operations()
        .iter()
        .zip(mutation_ids.chunks_exact(2))
        .enumerate()
    {
        let MigrationOperation::RekeyRow {
            table,
            old_key,
            new_key,
        } = operation
        else {
            return Err(CompactBaseProjectionError::UnsupportedEvolutionOperation);
        };
        let [deletion_id, replacement_id] = [ids[0], ids[1]];
        if table.bytes() != profile.table_id()
            || deletion_id == [0; 16]
            || replacement_id == [0; 16]
            || !mutation_id_set.insert(deletion_id)
            || !mutation_id_set.insert(replacement_id)
        {
            return Err(CompactBaseProjectionError::WrongTable);
        }
        let old_bytes = old_key
            .encode()
            .map_err(|_| CompactBaseProjectionError::InvalidEvolutionKey)?;
        let old_identity = profile
            .decode_key(&old_bytes)
            .map_err(|_| CompactBaseProjectionError::InvalidEvolutionKey)?;
        let new_bytes = new_key
            .encode()
            .map_err(|_| CompactBaseProjectionError::InvalidEvolutionKey)?;
        let new_identity = profile
            .decode_key(&new_bytes)
            .map_err(|_| CompactBaseProjectionError::InvalidEvolutionKey)?;
        let Some(row) = base.rows.get(&old_identity) else {
            return Err(CompactBaseProjectionError::MissingBaseKey);
        };
        let Some(value) = row.value.as_ref() else {
            return Err(CompactBaseProjectionError::MissingBaseValue);
        };
        if old_identity == new_identity
            || base.rows.contains_key(&new_identity)
            || !targets.insert(new_identity.clone())
        {
            return Err(CompactBaseProjectionError::DuplicateEvolutionKey);
        }
        let value = rewrite_rekey_row_value(profile, &old_identity, &new_identity, value)?;
        let value = value
            .encode()
            .map_err(|_| CompactBaseProjectionError::InvalidEvolutionValue)?;
        let sequence = u64::try_from(index)
            .ok()
            .and_then(|index| index.checked_mul(2))
            .and_then(|sequence| sequence.checked_add(1))
            .ok_or(CompactBaseProjectionError::EvolutionInputMismatch)?;
        mutations.push(CompactWriterMutation {
            sequence,
            mutation_id: deletion_id,
            key: old_identity,
            state: CompactWriterMutationState::Deletion,
        });
        mutations.push(CompactWriterMutation {
            sequence: sequence
                .checked_add(1)
                .ok_or(CompactBaseProjectionError::EvolutionInputMismatch)?,
            mutation_id: replacement_id,
            key: new_identity,
            state: CompactWriterMutationState::Replacement { value },
        });
    }
    let input = CompactWriterInput {
        table_id: profile.table_id(),
        schema_fingerprint: profile.schema_fingerprint(),
        candidate_generation,
        row_encoding_identity: PublicationRowEncoding::CompactOvb1,
        value_encoding_identity: PublicationValueEncoding::Ovb1,
        mutations,
        candidate_digest,
    };
    base.consume_writer_input(&input)?;
    Ok(input)
}

fn rewrite_rekey_row_value(
    profile: &CompactOvbProfile,
    old_key: &CompactKeyIdentity,
    new_key: &CompactKeyIdentity,
    value: &CanonicalValue,
) -> Result<CanonicalValue, CompactBaseProjectionError> {
    let old_components =
        decode_key_components(profile, old_key).map_err(|_| CompactBaseProjectionError::InvalidEvolutionKey)?;
    let new_components =
        decode_key_components(profile, new_key).map_err(|_| CompactBaseProjectionError::InvalidEvolutionKey)?;
    let OvbRaw::Tag(60009, payload) = value.raw() else {
        return Err(CompactBaseProjectionError::InvalidEvolutionValue);
    };
    let Some(row) = array(payload) else {
        return Err(CompactBaseProjectionError::InvalidEvolutionValue);
    };
    let [row_type, OvbRaw::Array(fields)] = row else {
        return Err(CompactBaseProjectionError::InvalidEvolutionValue);
    };
    let mut seen_keys = BTreeSet::new();
    let mut rewritten_fields = Vec::with_capacity(fields.len());
    for field in fields {
        let OvbRaw::Array(parts) = field else {
            return Err(CompactBaseProjectionError::InvalidEvolutionValue);
        };
        let [field_id, field_value] = parts.as_slice() else {
            return Err(CompactBaseProjectionError::InvalidEvolutionValue);
        };
        let field_id_bytes =
            uuid(field_id).map_err(|_| CompactBaseProjectionError::InvalidEvolutionValue)?;
        let mut field_value = field_value.clone();
        if let Some(index) = profile
            .key_fields
            .iter()
            .position(|key_field| key_field.id == field_id_bytes)
        {
            if !seen_keys.insert(field_id_bytes) || field_value != old_components[index] {
                return Err(CompactBaseProjectionError::InvalidEvolutionValue);
            }
            field_value = new_components[index].clone();
        }
        rewritten_fields.push(OvbRaw::Array(vec![
            field_id.clone(),
            field_value,
        ]));
    }
    if seen_keys.len() != profile.key_fields.len() {
        return Err(CompactBaseProjectionError::InvalidEvolutionValue);
    }
    CanonicalValue::new(OvbRaw::Tag(
        60009,
        Box::new(OvbRaw::Array(vec![
            row_type.clone(),
            OvbRaw::Array(rewritten_fields),
        ])),
    ))
    .map_err(|_| CompactBaseProjectionError::InvalidEvolutionValue)
}

fn decode_key_components(
    profile: &CompactOvbProfile,
    key: &CompactKeyIdentity,
) -> Result<Vec<OvbRaw>, CompactKeyError> {
    profile.decode_key(key.encoded())?;
    let value = CanonicalValue::decode(key.encoded()).map_err(|_| CompactKeyError::InvalidOvb)?;
    match profile.key_fields.len() {
        0 => Ok(Vec::new()),
        1 => Ok(vec![value.raw().clone()]),
        _ => {
            let OvbRaw::Tag(60015, payload) = value.raw() else {
                return Err(CompactKeyError::KeyArity);
            };
            let components = array(payload).ok_or(CompactKeyError::KeyArity)?;
            if components.len() != profile.key_fields.len() {
                return Err(CompactKeyError::KeyArity);
            }
            Ok(components.to_vec())
        }
    }
}

/// Fail-closed errors while projecting and folding committed compact rows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompactBaseProjectionError {
    Empty,
    WrongTable,
    WrongSchema,
    WrongProfile,
    DuplicateKeyGeneration,
    StaleGeneration,
    EvolutionInputMismatch,
    UnsupportedEvolutionOperation,
    InvalidEvolutionKey,
    InvalidEvolutionValue,
    MissingBaseKey,
    MissingBaseValue,
    DuplicateEvolutionKey,
}

impl fmt::Display for CompactBaseProjectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "compact committed base contains no segments",
            Self::WrongTable => "compact committed base has the wrong table",
            Self::WrongSchema => "compact committed base has the wrong schema",
            Self::WrongProfile => "compact committed base has the wrong profile",
            Self::DuplicateKeyGeneration => {
                "compact committed base repeats a key at one generation"
            }
            Self::StaleGeneration => {
                "compact writer generation does not match the base next generation"
            }
            Self::EvolutionInputMismatch => "evolution input does not match mutation IDs",
            Self::UnsupportedEvolutionOperation => {
                "schema operation lacks a physical compact projection"
            }
            Self::InvalidEvolutionKey => "evolution key is invalid for the compact profile",
            Self::InvalidEvolutionValue => "evolution row value is not canonical",
            Self::MissingBaseKey => "evolution rekey source is absent from the compact base",
            Self::MissingBaseValue => "evolution rekey source has no row value",
            Self::DuplicateEvolutionKey => "evolution rekey target collides with the compact base",
        })
    }
}

impl std::error::Error for CompactBaseProjectionError {}

/// Folds complete rows from verified committed segments by generation.
pub fn fold_compact_committed_base<'a, I>(
    profile: &CompactOvbProfile,
    projections: I,
) -> Result<CompactBaseState, CompactBaseProjectionError>
where
    I: IntoIterator<Item = &'a CompactCommittedSegmentProjection>,
{
    let mut rows: BTreeMap<CompactKeyIdentity, CompactBaseRow> = BTreeMap::new();
    for projection in projections {
        if projection.table_id().as_bytes() != &profile.table_id() {
            return Err(CompactBaseProjectionError::WrongTable);
        }
        if projection.schema_id() != profile.schema_fingerprint() {
            return Err(CompactBaseProjectionError::WrongSchema);
        }
        if projection.profile() != COMPACT_STORAGE_PROFILE {
            return Err(CompactBaseProjectionError::WrongProfile);
        }
        for row in projection.rows() {
            let key_bytes = row
                .key()
                .encode()
                .map_err(|_| CompactBaseProjectionError::WrongSchema)?;
            let key = profile
                .decode_key(&key_bytes)
                .map_err(|_| CompactBaseProjectionError::WrongSchema)?;
            let candidate = CompactBaseRow {
                key: row.key().clone(),
                value: row.value().cloned(),
                generation: projection.generation(),
                role: projection.role(),
            };
            if let Some(existing) = rows.get(&key) {
                if existing.generation == candidate.generation {
                    return Err(CompactBaseProjectionError::DuplicateKeyGeneration);
                }
                if existing.generation > candidate.generation {
                    continue;
                }
            }
            rows.insert(key, candidate);
        }
    }
    Ok(CompactBaseState {
        table_id: profile.table_id(),
        schema_fingerprint: profile.schema_fingerprint(),
        rows,
    })
}

/// Fail-closed validation errors at the runtime-to-compact boundary.
#[derive(Debug, Eq, PartialEq)]
pub enum CompactLoweringError {
    EmptyMutations,
    EmptyTable,
    WrongTable,
    WrongSchema,
    WrongRowEncoding,
    WrongValueEncoding,
    StaleGeneration,
    GenerationMismatch,
    SequenceOutOfOrder,
    DuplicateMutationId,
    DuplicateKey,
    InvalidKey(CompactKeyError),
    KeyWitnessMismatch,
    ValueWitnessMismatch,
    CandidateWitnessMismatch,
    CandidateDigestMismatch,
}

impl fmt::Display for CompactLoweringError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyMutations => f.write_str("compact freeze contains no mutations"),
            Self::EmptyTable => f.write_str("compact freeze has an empty table identity"),
            Self::WrongTable => f.write_str("compact freeze mutation belongs to another table"),
            Self::WrongSchema => f.write_str("compact freeze mutation belongs to another schema"),
            Self::WrongRowEncoding => f.write_str("compact freeze uses an unsupported row encoding"),
            Self::WrongValueEncoding => {
                f.write_str("compact freeze uses an unsupported value encoding")
            }
            Self::StaleGeneration => f.write_str("compact freeze candidate generation is stale"),
            Self::GenerationMismatch => {
                f.write_str("compact freeze mutations use different candidate generations")
            }
            Self::SequenceOutOfOrder => f.write_str("compact freeze mutation order is invalid"),
            Self::DuplicateMutationId => f.write_str("compact freeze repeats a mutation identity"),
            Self::DuplicateKey => f.write_str("compact freeze repeats a logical key"),
            Self::InvalidKey(error) => error.fmt(f),
            Self::KeyWitnessMismatch => f.write_str("compact key equivalence witness mismatches"),
            Self::ValueWitnessMismatch => {
                f.write_str("compact value equivalence witness mismatches")
            }
            Self::CandidateWitnessMismatch => {
                f.write_str("compact candidate equivalence witness mismatches")
            }
            Self::CandidateDigestMismatch => {
                f.write_str("compact candidate digest mismatches the ordered mutations")
            }
        }
    }
}

impl std::error::Error for CompactLoweringError {}

/// Validates and lowers one runtime freeze into deterministic writer input.
///
/// The runtime freeze is the sole source of mutation order and candidate
/// generation. This boundary validates every identity before exposing any
/// writer input; no opaque pre-encoded segment is accepted.
pub fn lower_publication_freeze(
    profile: &CompactOvbProfile,
    freeze: &PublicationFreeze,
) -> Result<CompactWriterInput, CompactLoweringError> {
    if freeze.mutations.is_empty() {
        return Err(CompactLoweringError::EmptyMutations);
    }
    let expected_generation = freeze.mutations[0].candidate_generation;
    let expected_candidate_digest = freeze.candidate_digest;
    if expected_generation == 0 || expected_generation != freeze.checkpoint.generation {
        return Err(CompactLoweringError::StaleGeneration);
    }
    let expected_schema = profile.schema_fingerprint();
    let expected_table = profile.table_id();
    let mut table_name: Option<&str> = None;
    let mut previous_sequence = 0;
    let mut mutation_ids = BTreeSet::new();
    let mut lowered = Vec::with_capacity(freeze.mutations.len());
    let mut candidate_bytes = Vec::new();
    candidate_bytes.extend_from_slice(b"ORNA-COMPACT-CANDIDATE-1\0");
    candidate_bytes.extend_from_slice(&expected_generation.to_be_bytes());
    for mutation in &freeze.mutations {
        if mutation.table.is_empty() {
            return Err(CompactLoweringError::EmptyTable);
        }
        if let Some(expected) = table_name {
            if expected != mutation.table {
                return Err(CompactLoweringError::WrongTable);
            }
        } else {
            table_name = Some(&mutation.table);
        }
        if mutation.table_id != expected_table {
            return Err(CompactLoweringError::WrongTable);
        }
        if mutation.schema_fingerprint != expected_schema {
            return Err(CompactLoweringError::WrongSchema);
        }
        if mutation.row_encoding_identity != PublicationRowEncoding::CompactOvb1 {
            return Err(CompactLoweringError::WrongRowEncoding);
        }
        if mutation.value_encoding_identity != PublicationValueEncoding::Ovb1 {
            return Err(CompactLoweringError::WrongValueEncoding);
        }
        if mutation.candidate_generation != expected_generation {
            return Err(CompactLoweringError::GenerationMismatch);
        }
        if mutation.sequence == 0 || mutation.sequence <= previous_sequence {
            return Err(CompactLoweringError::SequenceOutOfOrder);
        }
        previous_sequence = mutation.sequence;
        if !mutation_ids.insert(mutation.mutation_id) {
            return Err(CompactLoweringError::DuplicateMutationId);
        }
        let key = profile
            .decode_key(&mutation.logical_key)
            .map_err(CompactLoweringError::InvalidKey)?;
        let key_digest: [u8; 32] = Sha256::digest(&mutation.logical_key).into();
        if mutation.equivalence_witness.key_digest != key_digest {
            return Err(CompactLoweringError::KeyWitnessMismatch);
        }
        let (state, state_byte, value) = match &mutation.state {
            PublicationMutationState::Replacement { value } => {
                let digest: [u8; 32] = Sha256::digest(value).into();
                if mutation.equivalence_witness.value_digest != Some(digest) {
                    return Err(CompactLoweringError::ValueWitnessMismatch);
                }
                (
                    CompactWriterMutationState::Replacement {
                        value: value.clone(),
                    },
                    1u8,
                    value.as_slice(),
                )
            }
            PublicationMutationState::Deletion => {
                if mutation.equivalence_witness.value_digest.is_some() {
                    return Err(CompactLoweringError::ValueWitnessMismatch);
                }
                (CompactWriterMutationState::Deletion, 2u8, &[][..])
            }
        };
        candidate_bytes.extend_from_slice(&mutation.sequence.to_be_bytes());
        candidate_bytes.extend_from_slice(
            &u64::try_from(mutation.logical_key.len())
                .map_err(|_| CompactLoweringError::CandidateDigestMismatch)?
                .to_be_bytes(),
        );
        candidate_bytes.extend_from_slice(&mutation.logical_key);
        candidate_bytes.push(state_byte);
        candidate_bytes.extend_from_slice(
            &u64::try_from(value.len())
                .map_err(|_| CompactLoweringError::CandidateDigestMismatch)?
                .to_be_bytes(),
        );
        candidate_bytes.extend_from_slice(value);
        lowered.push(CompactWriterMutation {
            sequence: mutation.sequence,
            mutation_id: mutation.mutation_id,
            key,
            state,
        });
    }
    let candidate_digest: [u8; 32] = Sha256::digest(&candidate_bytes).into();
    for mutation in &freeze.mutations {
        if mutation.equivalence_witness.candidate_digest != expected_candidate_digest {
            return Err(CompactLoweringError::CandidateWitnessMismatch);
        }
    }
    if candidate_digest != expected_candidate_digest {
        return Err(CompactLoweringError::CandidateDigestMismatch);
    }
    Ok(CompactWriterInput {
        table_id: expected_table,
        schema_fingerprint: expected_schema,
        candidate_generation: expected_generation,
        row_encoding_identity: PublicationRowEncoding::CompactOvb1,
        value_encoding_identity: PublicationValueEncoding::Ovb1,
        mutations: lowered,
        candidate_digest,
    })
}


/// A validated compact logical schema projection for primary-key admission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompactOvbProfile {
    schema: CanonicalValue,
    table_id: [u8; 16],
    key_fields: Vec<KeyField>,
    definitions: BTreeMap<[u8; 16], NominalDefinition>,
    schema_fingerprint: [u8; 32],
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct KeyField {
    id: [u8; 16],
    ty: OvbRaw,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct NominalDefinition {
    kind: u64,
    body: OvbRaw,
}

impl CompactOvbProfile {
    /// Builds a profile from the exact, already-validated OVB schema.
    pub fn new(schema: SchemaDescriptor) -> Result<Self, CompactKeyError> {
        let schema = CanonicalValue::new(schema.raw().clone())
            .map_err(|_| CompactKeyError::InvalidProfile)?;
        Self::from_canonical_schema(schema)
    }

    /// Builds a profile from canonical OVB schema bytes.
    ///
    /// This storage boundary keeps schema field ordering (by field ObjectId)
    /// separate from the primary-key ordering in schema field 2. The shared
    /// descriptor validator admits spec-valid descriptors whose two orders
    /// differ, while this constructor retains the exact canonical bytes.
    pub fn from_ovb_schema(bytes: &[u8]) -> Result<Self, CompactKeyError> {
        let schema = SchemaDescriptor::decode(bytes).map_err(|_| CompactKeyError::InvalidOvb)?;
        Self::new(schema)
    }

    fn from_canonical_schema(schema: CanonicalValue) -> Result<Self, CompactKeyError> {
        let fields = integer_map(schema.raw()).ok_or(CompactKeyError::InvalidProfile)?;
        let table_id = uuid(*fields.get(&1).ok_or(CompactKeyError::InvalidProfile)?)?;
        let key_ids = array(*fields.get(&2).ok_or(CompactKeyError::InvalidProfile)?)
            .ok_or(CompactKeyError::InvalidProfile)?
            .iter()
            .map(uuid)
            .collect::<Result<Vec<_>, _>>()?;
        let mut definitions = BTreeMap::new();
        for definition in array(*fields.get(&4).ok_or(CompactKeyError::InvalidProfile)?)
            .ok_or(CompactKeyError::InvalidProfile)?
        {
            let definition = array(definition).ok_or(CompactKeyError::InvalidProfile)?;
            if definition.len() != 3 {
                return Err(CompactKeyError::InvalidProfile);
            }
            if definitions
                .insert(
                    uuid(&definition[0])?,
                    NominalDefinition {
                        kind: integer_value(&definition[1])?,
                        body: definition[2].clone(),
                    },
                )
                .is_some()
            {
                return Err(CompactKeyError::InvalidProfile);
            }
        }
        let declared_fields = array(*fields.get(&3).ok_or(CompactKeyError::InvalidProfile)?)
            .ok_or(CompactKeyError::InvalidProfile)?;
        let mut by_id = BTreeMap::new();
        for field in declared_fields {
            let field = array(field).ok_or(CompactKeyError::InvalidProfile)?;
            if field.len() != 5 {
                return Err(CompactKeyError::InvalidProfile);
            }
            let id = uuid(&field[0])?;
            if by_id
                .insert(
                    id,
                    KeyField {
                        id,
                        ty: field[2].clone(),
                    },
                )
                .is_some()
            {
                return Err(CompactKeyError::InvalidProfile);
            }
        }
        let key_fields = key_ids
            .into_iter()
            .map(|id| by_id.remove(&id).ok_or(CompactKeyError::InvalidProfile))
            .collect::<Result<Vec<_>, _>>()?;
        for field in &key_fields {
            validate_key_type(&field.ty, &definitions)?;
        }
        let schema_fingerprint = orna_value_v1::domain_digest("orna.schema.v1", schema.raw())
            .map_err(|_| CompactKeyError::InvalidProfile)?;
        Ok(Self {
            schema,
            table_id,
            key_fields,
            definitions,
            schema_fingerprint,
        })
    }

    pub fn schema(&self) -> &CanonicalValue {
        &self.schema
    }

    pub const fn table_id(&self) -> [u8; 16] {
        self.table_id
    }

    pub const fn schema_fingerprint(&self) -> [u8; 32] {
        self.schema_fingerprint
    }

    pub fn key_field_ids(&self) -> impl ExactSizeIterator<Item = [u8; 16]> + '_ {
        self.key_fields.iter().map(|field| field.id)
    }

    /// Decodes the canonical OVB key representation used by the
    /// table/runtime boundary. One key component is its scalar OVB value;
    /// multiple components are tag-60015 Tuple values in primary-key order.
    /// Field ObjectIds are schema metadata, not bytes embedded in the key.
    /// A tag-60009 Record is a row/structural value and is never a universal
    /// key envelope.
    pub fn decode_key(&self, bytes: &[u8]) -> Result<CompactKeyIdentity, CompactKeyError> {
        let value = CanonicalValue::decode(bytes).map_err(|_| CompactKeyError::InvalidOvb)?;
        if matches!(value.raw(), OvbRaw::Tag(60009, _))
            && (self.key_fields.len() != 1 || type_code(&self.key_fields[0].ty)? != 4)
        {
            return Err(CompactKeyError::RecordIsNotKey);
        }
        if self.key_fields.is_empty() {
            let OvbRaw::Tag(60014, unit) = value.raw() else {
                return Err(CompactKeyError::KeyArity);
            };
            if !array(unit).is_some_and(<[_]>::is_empty) {
                return Err(CompactKeyError::KeyArity);
            }
        } else if self.key_fields.len() == 1 {
            validate_value(value.raw(), &self.key_fields[0].ty, &self.definitions)?;
        } else {
            let OvbRaw::Tag(60015, tuple) = value.raw() else {
                return Err(CompactKeyError::KeyArity);
            };
            let tuple = array(tuple).ok_or(CompactKeyError::KeyArity)?;
            if tuple.len() != self.key_fields.len() {
                return Err(CompactKeyError::KeyArity);
            }
            for (value, expected) in tuple.iter().zip(&self.key_fields) {
                validate_value(value, &expected.ty, &self.definitions)?;
            }
        }
        Ok(CompactKeyIdentity {
            encoded: bytes.to_vec(),
        })
    }

    /// Builds an immutable exact-key index from canonical key bytes supplied
    /// by a physical compact reader. Malformed or duplicate physical keys
    /// fail the whole snapshot.
    pub fn exact_key_index<'a, I>(&self, keys: I) -> Result<CompactExactKeyIndex, CompactKeyError>
    where
        I: IntoIterator<Item = &'a [u8]>,
    {
        let mut indexed = CompactExactKeyIndex {
            table_id: self.table_id,
            schema_fingerprint: self.schema_fingerprint,
            keys: BTreeSet::new(),
        };
        for bytes in keys {
            let key = self.decode_key(bytes)?;
            if !indexed.keys.insert(key) {
                return Err(CompactKeyError::DuplicateKey);
            }
        }
        Ok(indexed)
    }

    /// Validates a candidate key against an exact immutable index. There is
    /// deliberately no range-only fallback.
    pub fn validate_against_index(
        &self,
        candidate: &[u8],
        index: &CompactExactKeyIndex,
    ) -> Result<CompactKeyIdentity, CompactKeyError> {
        if index.table_id != self.table_id || index.schema_fingerprint != self.schema_fingerprint {
            return Err(CompactKeyError::IndexProfileMismatch);
        }
        let candidate = self.decode_key(candidate)?;
        if index.keys.contains(&candidate) {
            return Err(CompactKeyError::DuplicateKey);
        }
        Ok(candidate)
    }

    /// Convenience for adapters that have an exact key stream but have not
    /// materialised the immutable index yet.
    pub fn reject_exact_duplicate<'a, I>(
        &self,
        candidate: &[u8],
        existing: I,
    ) -> Result<CompactKeyIdentity, CompactKeyError>
    where
        I: IntoIterator<Item = &'a [u8]>,
    {
        let index = self.exact_key_index(existing)?;
        self.validate_against_index(candidate, &index)
    }

    /// Resolves the effective logical key set from every validated manifest
    /// entry. The source owns physical Parquet decoding and yields canonical
    /// key bytes for one entry at a time; this method owns generation folding.
    /// This is the logical key/manifest boundary, not a full Parquet reader.
    pub fn logical_key_index<S>(
        &self,
        manifest: &CompactManifest,
        source: &S,
        key_budget: usize,
    ) -> Result<CompactExactKeyIndex, CompactLogicalKeyError<S::Error>>
    where
        S: CompactExactKeySource,
    {
        if manifest.table().as_bytes() != &self.table_id {
            return Err(CompactLogicalKeyError::Key(CompactKeyError::WrongTable));
        }
        if manifest.schema() != self.schema_fingerprint {
            return Err(CompactLogicalKeyError::Key(CompactKeyError::WrongSchema));
        }
        let mut resolved = LogicalKeyAccumulator::new(key_budget);
        for entry in manifest.entries() {
            let keys = source
                .exact_keys(entry)
                .map_err(CompactLogicalKeyError::Source)?;
            consume_entry_keys(
                self,
                &mut resolved,
                entry.generation(),
                entry.role(),
                entry.row_count(),
                keys,
            )?;
        }
        Ok(CompactExactKeyIndex {
            table_id: self.table_id,
            schema_fingerprint: self.schema_fingerprint,
            keys: resolved.finish(),
        })
    }
}

/// An immutable, canonical exact key identity.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CompactKeyIdentity {
    encoded: Vec<u8>,
}

impl CompactKeyIdentity {
    pub fn encoded(&self) -> &[u8] {
        &self.encoded
    }

    pub fn same_identity(&self, other: &Self) -> bool {
        self.encoded == other.encoded
    }
}

/// An immutable exact-key index for a schema-bound logical compact snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompactExactKeyIndex {
    table_id: [u8; 16],
    schema_fingerprint: [u8; 32],
    keys: BTreeSet<CompactKeyIdentity>,
}

impl CompactExactKeyIndex {
    pub fn contains(&self, key: &CompactKeyIdentity) -> bool {
        self.keys.contains(key)
    }

    pub const fn table_id(&self) -> [u8; 16] {
        self.table_id
    }

    pub const fn schema_fingerprint(&self) -> [u8; 32] {
        self.schema_fingerprint
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

/// A boxed stream of canonical exact keys from a physical adapter.
pub type CompactExactKeys<'a, E> = Box<dyn Iterator<Item = Result<Vec<u8>, E>> + 'a>;

/// Physical adapter seam for exact canonical keys in one manifest entry.
/// Physical Parquet decoding remains outside this module; the caller consumes
/// the iterator under its checked logical-key budget.
pub trait CompactExactKeySource {
    type Error;

    fn exact_keys<'a>(
        &'a self,
        entry: &'a CompactManifestEntry,
    ) -> Result<CompactExactKeys<'a, Self::Error>, Self::Error>;
}

/// Storage-owned logical exact-key reader facade for an immutable compact
/// snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompactLogicalReader {
    profile: CompactOvbProfile,
}

impl CompactLogicalReader {
    pub fn new(profile: CompactOvbProfile) -> Self {
        Self { profile }
    }

    pub fn profile(&self) -> &CompactOvbProfile {
        &self.profile
    }

    pub fn validate_key(&self, bytes: &[u8]) -> Result<CompactKeyIdentity, CompactKeyError> {
        self.profile.decode_key(bytes)
    }

    pub fn validate_key_against_index(
        &self,
        bytes: &[u8],
        index: &CompactExactKeyIndex,
    ) -> Result<CompactKeyIdentity, CompactKeyError> {
        self.profile.validate_against_index(bytes, index)
    }

    pub fn logical_key_index<S>(
        &self,
        manifest: &CompactManifest,
        source: &S,
        key_budget: usize,
    ) -> Result<CompactExactKeyIndex, CompactLogicalKeyError<S::Error>>
    where
        S: CompactExactKeySource,
    {
        self.profile.logical_key_index(manifest, source, key_budget)
    }
}

/// Fail-closed errors from immutable OVB/profile/key admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompactKeyError {
    InvalidProfile,
    InvalidOvb,
    WrongTable,
    WrongSchema,
    IndexProfileMismatch,
    ProhibitedKeyType,
    KeyArity,
    KeyTypeMismatch,
    UnsupportedKeyType,
    RecordIsNotKey,
    DuplicateKey,
}

impl fmt::Display for CompactKeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidProfile => "invalid compact OVB profile",
            Self::InvalidOvb => "invalid canonical OVB key",
            Self::WrongTable => "compact key belongs to another table",
            Self::WrongSchema => "compact manifest belongs to another schema",
            Self::IndexProfileMismatch => "exact-key index belongs to another compact profile",
            Self::ProhibitedKeyType => "compact key type is prohibited by the profile",
            Self::KeyArity => "compact key has the wrong arity",
            Self::KeyTypeMismatch => "compact key value does not match its schema",
            Self::UnsupportedKeyType => "compact key type is not supported by this reader",
            Self::RecordIsNotKey => "an OVB record is not a compact key",
            Self::DuplicateKey => "compact key already exists",
        })
    }
}

impl std::error::Error for CompactKeyError {}

/// Errors raised while resolving a manifest's effective logical keys.
#[derive(Debug, Eq, PartialEq)]
pub enum CompactLogicalKeyError<E> {
    Key(CompactKeyError),
    Source(E),
    KeyBudgetExceeded {
        limit: usize,
    },
    DuplicateKeyGeneration,
    KeyCountMismatch {
        generation: u64,
        expected: u64,
        observed: u64,
    },
}

impl<E> CompactLogicalKeyError<E> {
    fn from_merge(error: LogicalMergeError) -> Self {
        match error {
            LogicalMergeError::KeyBudgetExceeded { limit } => Self::KeyBudgetExceeded { limit },
            LogicalMergeError::DuplicateKeyGeneration => Self::DuplicateKeyGeneration,
        }
    }
}

impl<E: fmt::Display> fmt::Display for CompactLogicalKeyError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Key(error) => error.fmt(f),
            Self::Source(error) => write!(f, "compact exact-key source failed: {error}"),
            Self::KeyBudgetExceeded { limit } => {
                write!(f, "compact logical key budget exceeded: {limit}")
            }
            Self::DuplicateKeyGeneration => {
                f.write_str("compact key appears more than once at one generation")
            }
            Self::KeyCountMismatch {
                generation,
                expected,
                observed,
            } => write!(
                f,
                "compact generation {generation} enumerated {observed} keys, expected {expected}"
            ),
        }
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for CompactLogicalKeyError<E> {}

fn consume_entry_keys<E, I>(
    profile: &CompactOvbProfile,
    resolved: &mut LogicalKeyAccumulator,
    generation: u64,
    role: CompactSegmentRole,
    expected: u64,
    keys: I,
) -> Result<(), CompactLogicalKeyError<E>>
where
    I: IntoIterator<Item = Result<Vec<u8>, E>>,
{
    let mut observed = 0u64;
    for bytes in keys {
        let bytes = bytes.map_err(CompactLogicalKeyError::Source)?;
        observed = observed
            .checked_add(1)
            .ok_or(CompactLogicalKeyError::KeyCountMismatch {
                generation,
                expected,
                observed: u64::MAX,
            })?;
        let key = profile
            .decode_key(&bytes)
            .map_err(CompactLogicalKeyError::Key)?;
        resolved
            .add(generation, role, key)
            .map_err(CompactLogicalKeyError::from_merge)?;
    }
    if observed != expected {
        return Err(CompactLogicalKeyError::KeyCountMismatch {
            generation,
            expected,
            observed,
        });
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LogicalKeyState {
    generation: u64,
    role: CompactSegmentRole,
}

#[derive(Debug)]
struct LogicalKeyAccumulator {
    budget: usize,
    observed: usize,
    keys: BTreeMap<CompactKeyIdentity, LogicalKeyState>,
    seen: BTreeSet<(CompactKeyIdentity, u64)>,
}

impl LogicalKeyAccumulator {
    fn new(budget: usize) -> Self {
        Self {
            budget,
            observed: 0,
            keys: BTreeMap::new(),
            seen: BTreeSet::new(),
        }
    }

    fn add(
        &mut self,
        generation: u64,
        role: CompactSegmentRole,
        key: CompactKeyIdentity,
    ) -> Result<(), LogicalMergeError> {
        if self.observed == self.budget {
            return Err(LogicalMergeError::KeyBudgetExceeded { limit: self.budget });
        }
        self.observed += 1;
        if !self.seen.insert((key.clone(), generation)) {
            return Err(LogicalMergeError::DuplicateKeyGeneration);
        }
        match self.keys.entry(key) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(LogicalKeyState { generation, role });
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                let state = entry.get_mut();
                if state.generation == generation {
                    return Err(LogicalMergeError::DuplicateKeyGeneration);
                }
                if generation > state.generation {
                    *state = LogicalKeyState { generation, role };
                }
            }
        }
        Ok(())
    }

    fn finish(self) -> BTreeSet<CompactKeyIdentity> {
        self.keys
            .into_iter()
            .filter_map(|(key, state)| (state.role != CompactSegmentRole::Deletion).then_some(key))
            .collect()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LogicalMergeError {
    KeyBudgetExceeded { limit: usize },
    DuplicateKeyGeneration,
}

fn integer_map(raw: &OvbRaw) -> Option<BTreeMap<u64, &OvbRaw>> {
    let OvbRaw::Map(entries) = raw else {
        return None;
    };
    let mut map = BTreeMap::new();
    for (key, value) in entries {
        let OvbRaw::Int(key) = key else {
            return None;
        };
        let key = key.to_string().parse::<u64>().ok()?;
        if map.insert(key, value).is_some() {
            return None;
        }
    }
    Some(map)
}

fn validate_key_type(
    raw: &OvbRaw,
    definitions: &BTreeMap<[u8; 16], NominalDefinition>,
) -> Result<(), CompactKeyError> {
    let node = array(raw).ok_or(CompactKeyError::InvalidProfile)?;
    match type_code(raw)? {
        0 => match node.get(1) {
            Some(OvbRaw::Text(name)) if name == "Float" || name == "Blob" => {
                Err(CompactKeyError::ProhibitedKeyType)
            }
            Some(OvbRaw::Text(name))
                if matches!(
                    name.as_str(),
                    "Bool" | "Int" | "Decimal" | "Str" | "Uuid" | "Date" | "Instant"
                ) =>
            {
                Ok(())
            }
            Some(OvbRaw::Text(_)) => Err(CompactKeyError::UnsupportedKeyType),
            _ => Err(CompactKeyError::InvalidProfile),
        },
        3 => {
            let components = array(&node[1]).ok_or(CompactKeyError::InvalidProfile)?;
            if components.is_empty() {
                return Err(CompactKeyError::UnsupportedKeyType);
            }
            for node in components {
                validate_key_type(node, definitions)?;
            }
            Ok(())
        }
        5 => {
            let definition = definitions
                .get(&uuid(&node[1])?)
                .ok_or(CompactKeyError::InvalidProfile)?;
            if definition.kind != 1
                || array(&definition.body)
                    .ok_or(CompactKeyError::InvalidProfile)?
                    .iter()
                    .all(|variant| {
                        array(variant)
                            .and_then(|variant| variant.get(2))
                            .and_then(array)
                            .is_some_and(|fields| !fields.is_empty())
                    })
            {
                Err(CompactKeyError::UnsupportedKeyType)
            } else {
                Ok(())
            }
        }
        6 => {
            uuid(&node[1])?;
            uuid(&node[2])?;
            let components = array(&node[3]).ok_or(CompactKeyError::InvalidProfile)?;
            if components.is_empty() {
                return Err(CompactKeyError::UnsupportedKeyType);
            }
            for node in components {
                validate_key_type(node, definitions)?;
            }
            Ok(())
        }
        1 | 2 | 4 | 7 | 8 | 9 => Err(CompactKeyError::UnsupportedKeyType),
        _ => Err(CompactKeyError::InvalidProfile),
    }
}

fn integer_value(raw: &OvbRaw) -> Result<u64, CompactKeyError> {
    let OvbRaw::Int(value) = raw else {
        return Err(CompactKeyError::InvalidProfile);
    };
    value
        .to_string()
        .parse()
        .map_err(|_| CompactKeyError::InvalidProfile)
}

fn array(raw: &OvbRaw) -> Option<&[OvbRaw]> {
    match raw {
        OvbRaw::Array(values) => Some(values),
        _ => None,
    }
}

fn uuid(raw: &OvbRaw) -> Result<[u8; 16], CompactKeyError> {
    let OvbRaw::Tag(37, value) = raw else {
        return Err(CompactKeyError::InvalidProfile);
    };
    let OvbRaw::Bytes(bytes) = value.as_ref() else {
        return Err(CompactKeyError::InvalidProfile);
    };
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| CompactKeyError::InvalidProfile)
}

fn type_code(raw: &OvbRaw) -> Result<u64, CompactKeyError> {
    let node = array(raw).ok_or(CompactKeyError::InvalidProfile)?;
    let OvbRaw::Int(code) = node.first().ok_or(CompactKeyError::InvalidProfile)? else {
        return Err(CompactKeyError::InvalidProfile);
    };
    code.to_string()
        .parse()
        .map_err(|_| CompactKeyError::InvalidProfile)
}

fn validate_value(
    value: &OvbRaw,
    ty: &OvbRaw,
    definitions: &BTreeMap<[u8; 16], NominalDefinition>,
) -> Result<(), CompactKeyError> {
    let node = array(ty).ok_or(CompactKeyError::InvalidProfile)?;
    let code = type_code(ty)?;
    match code {
        0 => {
            let OvbRaw::Text(name) = node.get(1).ok_or(CompactKeyError::InvalidProfile)? else {
                return Err(CompactKeyError::InvalidProfile);
            };
            let valid = match name.as_str() {
                "Bool" => matches!(value, OvbRaw::Bool(_)),
                "Int" => matches!(value, OvbRaw::Int(_)),
                "Decimal" => matches!(value, OvbRaw::Tag(60000, _)),
                "Str" => matches!(value, OvbRaw::Text(_)),
                "Uuid" => matches!(value, OvbRaw::Tag(37, _)),
                "Date" => matches!(value, OvbRaw::Tag(60001, _)),
                "Instant" => matches!(value, OvbRaw::Tag(60002, _)),
                _ => return Err(CompactKeyError::UnsupportedKeyType),
            };
            if valid {
                Ok(())
            } else {
                Err(CompactKeyError::KeyTypeMismatch)
            }
        }
        3 => {
            let types = array(&node[1]).ok_or(CompactKeyError::InvalidProfile)?;
            let OvbRaw::Tag(60015, tuple) = value else {
                return Err(CompactKeyError::KeyTypeMismatch);
            };
            let tuple = array(tuple).ok_or(CompactKeyError::KeyTypeMismatch)?;
            if tuple.len() != types.len() {
                return Err(CompactKeyError::KeyTypeMismatch);
            }
            tuple
                .iter()
                .zip(types)
                .try_for_each(|(value, ty)| validate_value(value, ty, definitions))
        }
        5 => {
            let type_id = uuid(&node[1])?;
            let definition = definitions
                .get(&type_id)
                .ok_or(CompactKeyError::InvalidProfile)?;
            if definition.kind != 1 {
                return Err(CompactKeyError::UnsupportedKeyType);
            }
            validate_enum(value, type_id, definition)
        }
        6 => {
            let OvbRaw::Tag(60021, reference) = value else {
                return Err(CompactKeyError::KeyTypeMismatch);
            };
            let reference = array(reference).ok_or(CompactKeyError::KeyTypeMismatch)?;
            if reference.len() != 3
                || uuid(&reference[0])? != uuid(&node[1])?
                || uuid(&reference[1])? != uuid(&node[2])?
            {
                return Err(CompactKeyError::KeyTypeMismatch);
            }
            let types = array(&node[3]).ok_or(CompactKeyError::InvalidProfile)?;
            validate_key_components(&reference[2], types, definitions)
        }
        _ => Err(CompactKeyError::InvalidProfile),
    }
}

fn validate_key_components(
    value: &OvbRaw,
    types: &[OvbRaw],
    definitions: &BTreeMap<[u8; 16], NominalDefinition>,
) -> Result<(), CompactKeyError> {
    match types {
        [] => {
            if matches!(value, OvbRaw::Tag(60014, unit) if array(unit).is_some_and(<[_]>::is_empty))
            {
                Ok(())
            } else {
                Err(CompactKeyError::KeyArity)
            }
        }
        [ty] => validate_value(value, ty, definitions),
        types => {
            let OvbRaw::Tag(60015, tuple) = value else {
                return Err(CompactKeyError::KeyArity);
            };
            let tuple = array(tuple).ok_or(CompactKeyError::KeyArity)?;
            if tuple.len() != types.len() {
                return Err(CompactKeyError::KeyArity);
            }
            for (value, ty) in tuple.iter().zip(types) {
                validate_value(value, ty, definitions)?;
            }
            Ok(())
        }
    }
}

fn validate_enum(
    value: &OvbRaw,
    type_id: [u8; 16],
    definition: &NominalDefinition,
) -> Result<(), CompactKeyError> {
    let OvbRaw::Tag(60008, encoded) = value else {
        return Err(CompactKeyError::KeyTypeMismatch);
    };
    let encoded = array(encoded).ok_or(CompactKeyError::KeyTypeMismatch)?;
    if encoded.len() != 3 || uuid(&encoded[0])? != type_id {
        return Err(CompactKeyError::KeyTypeMismatch);
    }
    let variant_id = uuid(&encoded[1])?;
    let variants = array(&definition.body).ok_or(CompactKeyError::InvalidProfile)?;
    let variant = variants
        .iter()
        .map(|variant| array(variant).ok_or(CompactKeyError::InvalidProfile))
        .find_map(|variant| match variant {
            Ok(variant) if uuid(&variant[0]).ok() == Some(variant_id) => Some(Ok(variant)),
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .transpose()?
        .ok_or(CompactKeyError::KeyTypeMismatch)?;
    let fields = array(&variant[2]).ok_or(CompactKeyError::InvalidProfile)?;
    if fields.is_empty() && matches!(encoded[2], OvbRaw::Null) {
        Ok(())
    } else if fields.is_empty() {
        Err(CompactKeyError::KeyTypeMismatch)
    } else {
        Err(CompactKeyError::UnsupportedKeyType)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_foundation_v1::OvbRaw;
    use orna_repository_v1::Uuid;

    const TABLE: [u8; 16] = [0x10; 16];
    const OTHER_TABLE: [u8; 16] = [0x11; 16];
    const KEY_A: [u8; 16] = [0x20; 16];
    const KEY_B: [u8; 16] = [0x21; 16];
    const ENUM_TYPE: [u8; 16] = [0x30; 16];
    const ENUM_READY: [u8; 16] = [0x31; 16];
    const ENUM_WITH_VALUE: [u8; 16] = [0x32; 16];
    const ENUM_VALUE_FIELD: [u8; 16] = [0x33; 16];
    const REF_DATABASE: [u8; 16] = [0x40; 16];
    const REF_TABLE: [u8; 16] = [0x41; 16];

    fn uuid_raw(bytes: [u8; 16]) -> OvbRaw {
        OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(bytes.to_vec())))
    }

    fn primitive(name: &str) -> OvbRaw {
        OvbRaw::Array(vec![OvbRaw::Int(0.into()), OvbRaw::Text(name.into())])
    }

    fn schema_raw_for_table(
        table: [u8; 16],
        fields: Vec<OvbRaw>,
        keys: Vec<OvbRaw>,
        definitions: Vec<OvbRaw>,
    ) -> OvbRaw {
        OvbRaw::Map(vec![
            (OvbRaw::Int(0.into()), OvbRaw::Int(1.into())),
            (OvbRaw::Int(1.into()), uuid_raw(table)),
            (OvbRaw::Int(2.into()), OvbRaw::Array(keys)),
            (OvbRaw::Int(3.into()), OvbRaw::Array(fields)),
            (OvbRaw::Int(4.into()), OvbRaw::Array(definitions)),
        ])
    }

    fn schema_raw(fields: Vec<OvbRaw>, keys: Vec<OvbRaw>) -> OvbRaw {
        schema_raw_for_table(TABLE, fields, keys, vec![])
    }

    fn schema(fields: Vec<OvbRaw>, keys: Vec<OvbRaw>) -> SchemaDescriptor {
        SchemaDescriptor::new(schema_raw(fields, keys)).unwrap()
    }

    fn field(id: [u8; 16], name: &str, ty: OvbRaw) -> OvbRaw {
        OvbRaw::Array(vec![
            uuid_raw(id),
            OvbRaw::Text(name.into()),
            ty,
            OvbRaw::Int(0.into()),
            OvbRaw::Array(vec![OvbRaw::Int(0.into())]),
        ])
    }

    fn scalar_profile() -> CompactOvbProfile {
        CompactOvbProfile::new(schema(
            vec![field(KEY_A, "id", primitive("Int"))],
            vec![uuid_raw(KEY_A)],
        ))
        .unwrap()
    }

    fn reordered_profile() -> CompactOvbProfile {
        // Schema fields are ordered by ObjectId (A then B); the primary-key
        // declaration is B then A and tuple values must follow that order.
        let bytes = CanonicalValue::new(schema_raw(
            vec![
                field(KEY_A, "first", primitive("Int")),
                field(KEY_B, "second", primitive("Str")),
            ],
            vec![uuid_raw(KEY_B), uuid_raw(KEY_A)],
        ))
        .unwrap()
        .encode()
        .unwrap();
        CompactOvbProfile::from_ovb_schema(&bytes).unwrap()
    }

    fn decimal_profile() -> CompactOvbProfile {
        CompactOvbProfile::new(schema(
            vec![field(KEY_A, "amount", primitive("Decimal"))],
            vec![uuid_raw(KEY_A)],
        ))
        .unwrap()
    }

    fn profile_with_key_type(ty: OvbRaw) -> Result<CompactOvbProfile, CompactKeyError> {
        let currency = OvbRaw::Array(vec![
            uuid_raw(ENUM_TYPE),
            OvbRaw::Int(4.into()),
            OvbRaw::Array(vec![OvbRaw::Text("USD".into()), OvbRaw::Int(0.into())]),
        ]);
        let schema = SchemaDescriptor::new(schema_raw_for_table(
            TABLE,
            vec![field(KEY_A, "id", ty)],
            vec![uuid_raw(KEY_A)],
            vec![currency],
        ))
        .map_err(|_| CompactKeyError::InvalidProfile)?;
        CompactOvbProfile::new(schema)
    }

    fn profile_for_table(table: [u8; 16]) -> CompactOvbProfile {
        let schema = SchemaDescriptor::new(schema_raw_for_table(
            table,
            vec![field(KEY_A, "id", primitive("Int"))],
            vec![uuid_raw(KEY_A)],
            vec![],
        ))
        .unwrap();
        CompactOvbProfile::new(schema).unwrap()
    }

    fn scalar_key(id: i64) -> Vec<u8> {
        CanonicalValue::new(OvbRaw::Int(id.into()))
            .unwrap()
            .encode()
            .unwrap()
    }
    fn row_value(id: i64) -> CanonicalValue {
        CanonicalValue::new(OvbRaw::Tag(
            60009,
            Box::new(OvbRaw::Array(vec![
                OvbRaw::Null,
                OvbRaw::Array(vec![OvbRaw::Array(vec![
                    uuid_raw(KEY_A),
                    OvbRaw::Int(id.into()),
                ])]),
            ])),
        ))
        .unwrap()
    }
    fn fixture_profile() -> CompactOvbProfile {
        const SOURCE: &str =
            include_str!("../../../../../../../reference/Orna-1.0.0/examples/valid/table-explicit-key.orna");
        let header = SOURCE
            .lines()
            .find(|line| line.trim_start().starts_with("pub table "))
            .expect("checked-in schema fixture has a table declaration")
            .trim();
        let declaration = header
            .strip_prefix("pub table ")
            .expect("fixture table declaration is public");
        let (table_name, key_declarations) = declaration
            .split_once('(')
            .expect("fixture table declares an explicit key");
        assert_eq!(table_name, "Contact");
        let (key_declaration, _) = key_declarations
            .split_once(')')
            .expect("fixture key declaration closes");
        let (key_name, key_type) = key_declaration
            .split_once(':')
            .expect("fixture key has a type");
        let mut fields = vec![field(
            KEY_A,
            key_name.trim(),
            primitive(key_type.trim()),
        )];
        let body = SOURCE
            .split_once('{')
            .expect("fixture table has a body")
            .1
            .split_once('}')
            .expect("fixture table body closes")
            .0;
        for declaration in body.lines().map(str::trim).filter(|line| !line.is_empty()) {
            let (name, ty) = declaration
                .trim_end_matches(',')
                .split_once(':')
                .expect("fixture stored field has a type");
            let id = match name.trim() {
                "name" => KEY_B,
                "email" => [0x22; 16],
                other => panic!("unexpected Contact field {other}"),
            };
            let ty = ty.trim().trim_end_matches(',');
            let optional = ty.ends_with('?');
            let base_type = ty.trim_end_matches('?').trim();
            let field_type = primitive(base_type);
            let field_type = if optional {
                OvbRaw::Array(vec![OvbRaw::Int(9.into()), field_type])
            } else {
                field_type
            };
            fields.push(OvbRaw::Array(vec![
                uuid_raw(id),
                OvbRaw::Text(name.trim().to_owned()),
                field_type,
                OvbRaw::Int(1.into()),
                OvbRaw::Array(vec![OvbRaw::Int(0.into())]),
            ]));
        }
        CompactOvbProfile::new(schema(fields, vec![uuid_raw(KEY_A)])).unwrap()
    }

    fn fixture_key(value: &str) -> Vec<u8> {
        CanonicalValue::new(OvbRaw::Text(value.to_owned()))
            .unwrap()
            .encode()
            .unwrap()
    }

    fn fixture_row(key: &str, name: &str, email: Option<&str>) -> CanonicalValue {
        CanonicalValue::new(OvbRaw::Tag(
            60009,
            Box::new(OvbRaw::Array(vec![
                OvbRaw::Null,
                OvbRaw::Array(vec![
                    OvbRaw::Array(vec![uuid_raw(KEY_A), OvbRaw::Text(key.to_owned())]),
                    OvbRaw::Array(vec![uuid_raw(KEY_B), OvbRaw::Text(name.to_owned())]),
                    OvbRaw::Array(vec![
                        uuid_raw([0x22; 16]),
                        email.map_or(OvbRaw::Null, |value| OvbRaw::Text(value.to_owned())),
                    ]),
                ]),
            ])),
        ))
        .unwrap()
    }

    fn publication_mutation(
        profile: &CompactOvbProfile,
        sequence: u64,
        key: &str,
        state: PublicationMutationState,
    ) -> orna_runtime_v1::PublicationFreezeMutation {
        let logical_key = fixture_key(key);
        let key_digest = Sha256::digest(&logical_key).into();
        let value_digest = match &state {
            PublicationMutationState::Replacement { value } => {
                Some(Sha256::digest(value).into())
            }
            PublicationMutationState::Deletion => None,
        };
        orna_runtime_v1::PublicationFreezeMutation {
            table_id: profile.table_id(),
            sequence,
            mutation_id: [sequence as u8; 16],
            table: "Contact".to_owned(),
            logical_key,
            state,
            schema_fingerprint: profile.schema_fingerprint(),
            row_encoding_identity: PublicationRowEncoding::CompactOvb1,
            value_encoding_identity: PublicationValueEncoding::Ovb1,
            candidate_generation: 2,
            equivalence_witness: orna_runtime_v1::PublicationEquivalenceWitness {
                key_digest,
                value_digest,
                candidate_digest: [0; 32],
            },
        }
    }

    fn fixture_freeze(
        _profile: &CompactOvbProfile,
        mut mutations: Vec<orna_runtime_v1::PublicationFreezeMutation>,
    ) -> PublicationFreeze {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"ORNA-COMPACT-CANDIDATE-1\0");
        bytes.extend_from_slice(&2_u64.to_be_bytes());
        for mutation in &mutations {
            bytes.extend_from_slice(&mutation.sequence.to_be_bytes());
            bytes.extend_from_slice(&(mutation.logical_key.len() as u64).to_be_bytes());
            bytes.extend_from_slice(&mutation.logical_key);
            match &mutation.state {
                PublicationMutationState::Replacement { value } => {
                    bytes.push(1);
                    bytes.extend_from_slice(&(value.len() as u64).to_be_bytes());
                    bytes.extend_from_slice(value);
                }
                PublicationMutationState::Deletion => {
                    bytes.push(2);
                    bytes.extend_from_slice(&0_u64.to_be_bytes());
                }
            }
        }
        let candidate_digest: [u8; 32] = Sha256::digest(&bytes).into();
        for mutation in &mut mutations {
            mutation.equivalence_witness.candidate_digest = candidate_digest;
        }
        PublicationFreeze {
            intent_id: [0x77; 16],
            checkpoint: orna_runtime_v1::Checkpoint {
                generation: 2,
                digest: [0x78; 32],
                mutation_sequence: mutations.last().unwrap().sequence,
            },
            candidate_digest,
            mutations,
        }
    }

    #[test]
    fn ordered_freeze_chains_fold_against_fixture_schema_and_committed_base() {
        let profile = fixture_profile();
        let empty_base = fold_compact_committed_base(&profile, std::iter::empty()).unwrap();
        assert!(empty_base.rows().next().is_none());
        let existing_value = fixture_row("present", "Before", Some("before@example.test"));
        let changing_value = fixture_row("changing", "Before", None);
        let mut rows = BTreeMap::new();
        for (key_text, value) in [
            ("present", existing_value),
            ("changing", changing_value),
        ] {
            let encoded_key = fixture_key(key_text);
            let key = profile.decode_key(&encoded_key).unwrap();
            rows.insert(
                key,
                CompactBaseRow {
                    key: CanonicalValue::decode(&encoded_key).unwrap(),
                    value: Some(value),
                    generation: 1,
                    role: CompactSegmentRole::Data,
                },
            );
        }
        let base = CompactBaseState {
            table_id: profile.table_id(),
            schema_fingerprint: profile.schema_fingerprint(),
            rows,
        };
        let freeze = fixture_freeze(
            &profile,
            vec![
                publication_mutation(
                    &profile,
                    1,
                    "new",
                    PublicationMutationState::Replacement {
                        value: fixture_row("new", "Temporary", None).encode().unwrap(),
                    },
                ),
                publication_mutation(
                    &profile,
                    2,
                    "new",
                    PublicationMutationState::Deletion,
                ),
                publication_mutation(
                    &profile,
                    3,
                    "present",
                    PublicationMutationState::Replacement {
                        value: fixture_row("present", "Updated", None).encode().unwrap(),
                    },
                ),
                publication_mutation(
                    &profile,
                    4,
                    "present",
                    PublicationMutationState::Deletion,
                ),
                publication_mutation(
                    &profile,
                    5,
                    "changing",
                    PublicationMutationState::Deletion,
                ),
                publication_mutation(
                    &profile,
                    6,
                    "changing",
                    PublicationMutationState::Replacement {
                        value: fixture_row("changing", "Final", None).encode().unwrap(),
                    },
                ),
            ],
        );

        let raw = lower_publication_freeze(&profile, &freeze).unwrap();
        assert_eq!(raw.mutations.len(), 6);
        assert_eq!(raw.candidate_digest, freeze.candidate_digest);
        let folded = base.fold_writer_input(&raw).unwrap();
        base.consume_writer_input(&folded).unwrap();

        assert_eq!(folded.mutations.len(), 2);
        assert_eq!(folded.mutations[0].sequence, 4);
        assert_eq!(
            folded.mutations[0].key.encoded(),
            fixture_key("present")
        );
        assert_eq!(folded.mutations[0].state, CompactWriterMutationState::Deletion);
        assert_eq!(folded.mutations[1].sequence, 6);
        assert_eq!(
            folded.mutations[1].key.encoded(),
            fixture_key("changing")
        );
        let CompactWriterMutationState::Replacement { value } = &folded.mutations[1].state
        else {
            panic!("last replacement must remain the complete final mutation");
        };
        assert_eq!(
            CanonicalValue::decode(value).unwrap(),
            fixture_row("changing", "Final", None)
        );
        assert_eq!(folded.candidate_digest, freeze.candidate_digest);
    }

    #[test]
    fn ordered_freeze_rejects_bad_raw_candidate_witnesses_and_digest() {
        let profile = fixture_profile();
        let valid = || {
            fixture_freeze(
                &profile,
                vec![publication_mutation(
                    &profile,
                    1,
                    "new",
                    PublicationMutationState::Replacement {
                        value: fixture_row("new", "Final", None).encode().unwrap(),
                    },
                )],
            )
        };

        let mut bad_witness = valid();
        bad_witness.mutations[0].equivalence_witness.candidate_digest = [0x99; 32];
        assert_eq!(
            lower_publication_freeze(&profile, &bad_witness),
            Err(CompactLoweringError::CandidateWitnessMismatch)
        );

        let mut bad_digest = valid();
        bad_digest.candidate_digest = [0x98; 32];
        bad_digest.mutations[0]
            .equivalence_witness
            .candidate_digest = bad_digest.candidate_digest;
        assert_eq!(
            lower_publication_freeze(&profile, &bad_digest),
            Err(CompactLoweringError::CandidateDigestMismatch)
        );
    }


    fn enum_profile() -> CompactOvbProfile {
        let enum_type = OvbRaw::Array(vec![OvbRaw::Int(5.into()), uuid_raw(ENUM_TYPE)]);
        let definition = OvbRaw::Array(vec![
            uuid_raw(ENUM_TYPE),
            OvbRaw::Int(1.into()),
            OvbRaw::Array(vec![
                OvbRaw::Array(vec![
                    uuid_raw(ENUM_READY),
                    OvbRaw::Text("Ready".into()),
                    OvbRaw::Array(vec![]),
                ]),
                OvbRaw::Array(vec![
                    uuid_raw(ENUM_WITH_VALUE),
                    OvbRaw::Text("WithValue".into()),
                    OvbRaw::Array(vec![field(ENUM_VALUE_FIELD, "value", primitive("Int"))]),
                ]),
            ]),
        ]);
        let schema = schema_raw_for_table(
            TABLE,
            vec![field(KEY_A, "state", enum_type)],
            vec![uuid_raw(KEY_A)],
            vec![definition],
        );
        let schema = SchemaDescriptor::new(schema).unwrap();
        CompactOvbProfile::new(schema).unwrap()
    }

    fn stored_ref_profile() -> CompactOvbProfile {
        let reference_type = OvbRaw::Array(vec![
            OvbRaw::Int(6.into()),
            uuid_raw(REF_DATABASE),
            uuid_raw(REF_TABLE),
            OvbRaw::Array(vec![primitive("Int")]),
        ]);
        CompactOvbProfile::new(schema(
            vec![field(KEY_A, "ref", reference_type)],
            vec![uuid_raw(KEY_A)],
        ))
        .unwrap()
    }

    fn tuple_key() -> Vec<u8> {
        CanonicalValue::new(OvbRaw::Tag(
            60015,
            Box::new(OvbRaw::Array(vec![
                OvbRaw::Text("west".into()),
                OvbRaw::Int(7.into()),
            ])),
        ))
        .unwrap()
        .encode()
        .unwrap()
    }

    fn fold_segments(
        profile: &CompactOvbProfile,
        segments: &[(u64, CompactSegmentRole, Vec<i64>)],
        budget: usize,
    ) -> Result<BTreeSet<CompactKeyIdentity>, LogicalMergeError> {
        let mut resolved = LogicalKeyAccumulator::new(budget);
        for (generation, role, ids) in segments {
            for id in ids {
                let key = profile
                    .decode_key(&scalar_key(*id))
                    .expect("test key matches scalar profile");
                resolved.add(*generation, *role, key)?;
            }
        }
        Ok(resolved.finish())
    }

    struct EmptySource;

    impl CompactExactKeySource for EmptySource {
        type Error = ();

        fn exact_keys<'a>(
            &'a self,
            _entry: &'a CompactManifestEntry,
        ) -> Result<CompactExactKeys<'a, Self::Error>, Self::Error> {
            Ok(Box::new(std::iter::empty()))
        }
    }

    #[test]
    fn scalar_key_is_decoded_from_the_runtime_representation() {
        let decoded = scalar_profile().decode_key(&scalar_key(42)).unwrap();
        assert_eq!(decoded.encoded(), scalar_key(42));
    }

    #[test]
    fn prohibited_float_and_blob_key_profiles_fail_closed() {
        assert_eq!(
            profile_with_key_type(primitive("Float")),
            Err(CompactKeyError::ProhibitedKeyType)
        );
        assert_eq!(
            profile_with_key_type(primitive("Blob")),
            Err(CompactKeyError::ProhibitedKeyType)
        );
    }

    #[test]
    fn non_permitted_key_types_fail_at_profile_admission() {
        let types = vec![
            OvbRaw::Array(vec![OvbRaw::Int(1.into()), primitive("Int")]),
            OvbRaw::Array(vec![OvbRaw::Int(2.into()), primitive("Int")]),
            OvbRaw::Array(vec![OvbRaw::Int(9.into()), primitive("Int")]),
            OvbRaw::Array(vec![
                OvbRaw::Int(4.into()),
                OvbRaw::Array(vec![OvbRaw::Array(vec![
                    OvbRaw::Text("value".into()),
                    primitive("Int"),
                ])]),
            ]),
            OvbRaw::Array(vec![
                OvbRaw::Int(7.into()),
                primitive("Int"),
                uuid_raw(TABLE),
            ]),
            OvbRaw::Array(vec![OvbRaw::Int(8.into()), uuid_raw(ENUM_TYPE)]),
            primitive("Duration"),
        ];
        for ty in types {
            assert_eq!(
                profile_with_key_type(ty),
                Err(CompactKeyError::UnsupportedKeyType)
            );
        }
    }

    #[test]
    fn payload_free_enum_and_stored_reference_keys_use_schema_encoding() {
        let enum_profile = enum_profile();
        let enum_key = CanonicalValue::new(OvbRaw::Tag(
            60008,
            Box::new(OvbRaw::Array(vec![
                uuid_raw(ENUM_TYPE),
                uuid_raw(ENUM_READY),
                OvbRaw::Null,
            ])),
        ))
        .unwrap()
        .encode()
        .unwrap();
        assert!(enum_profile.decode_key(&enum_key).is_ok());
        let payload_enum_key = CanonicalValue::new(OvbRaw::Tag(
            60008,
            Box::new(OvbRaw::Array(vec![
                uuid_raw(ENUM_TYPE),
                uuid_raw(ENUM_WITH_VALUE),
                OvbRaw::Tag(
                    60009,
                    Box::new(OvbRaw::Array(vec![
                        OvbRaw::Null,
                        OvbRaw::Array(vec![OvbRaw::Array(vec![
                            uuid_raw(ENUM_VALUE_FIELD),
                            OvbRaw::Int(7.into()),
                        ])]),
                    ])),
                ),
            ])),
        ))
        .unwrap()
        .encode()
        .unwrap();
        assert_eq!(
            enum_profile.decode_key(&payload_enum_key),
            Err(CompactKeyError::UnsupportedKeyType)
        );

        let reference_profile = stored_ref_profile();
        let reference_key = CanonicalValue::new(OvbRaw::Tag(
            60021,
            Box::new(OvbRaw::Array(vec![
                uuid_raw(REF_DATABASE),
                uuid_raw(REF_TABLE),
                OvbRaw::Int(7.into()),
            ])),
        ))
        .unwrap()
        .encode()
        .unwrap();
        assert!(reference_profile.decode_key(&reference_key).is_ok());
    }

    #[test]
    fn exact_key_indexes_reject_cross_profile_use() {
        let profile = scalar_profile();
        let index = profile
            .exact_key_index([scalar_key(7)].iter().map(Vec::as_slice))
            .unwrap();
        assert_eq!(index.table_id(), TABLE);
        assert_eq!(index.schema_fingerprint(), profile.schema_fingerprint());

        let other_schema = profile_with_key_type(primitive("Str")).unwrap();
        assert_eq!(
            other_schema.validate_against_index(&scalar_key(7), &index),
            Err(CompactKeyError::IndexProfileMismatch)
        );

        let other_table = profile_for_table(OTHER_TABLE);
        assert_eq!(
            other_table.validate_against_index(&scalar_key(7), &index),
            Err(CompactKeyError::IndexProfileMismatch)
        );
    }

    #[test]
    fn logical_key_index_checks_manifest_table_and_schema() {
        let profile = scalar_profile();
        let source = EmptySource;
        let matching =
            CompactManifest::empty(Uuid::from_bytes(TABLE), profile.schema_fingerprint());
        assert!(
            profile
                .logical_key_index(&matching, &source, 0)
                .unwrap()
                .is_empty()
        );

        let wrong_table =
            CompactManifest::empty(Uuid::from_bytes(OTHER_TABLE), profile.schema_fingerprint());
        assert_eq!(
            profile.logical_key_index(&wrong_table, &source, 0),
            Err(CompactLogicalKeyError::Key(CompactKeyError::WrongTable))
        );

        let wrong_schema = CompactManifest::empty(Uuid::from_bytes(TABLE), [0x99; 32]);
        assert_eq!(
            profile.logical_key_index(&wrong_schema, &source, 0),
            Err(CompactLogicalKeyError::Key(CompactKeyError::WrongSchema))
        );
    }

    #[test]
    fn logical_generation_data_then_delete_removes_effective_key() {
        let profile = scalar_profile();
        let resolved = fold_segments(
            &profile,
            &[
                (1, CompactSegmentRole::Data, vec![7]),
                (2, CompactSegmentRole::Deletion, vec![7]),
            ],
            2,
        );
        assert!(resolved.unwrap().is_empty());
    }

    #[test]
    fn logical_generation_data_then_replacement_restores_effective_key() {
        let profile = scalar_profile();
        let resolved = fold_segments(
            &profile,
            &[
                (1, CompactSegmentRole::Data, vec![7]),
                (2, CompactSegmentRole::Replacement, vec![7]),
            ],
            2,
        )
        .unwrap();
        assert!(resolved.contains(&profile.decode_key(&scalar_key(7)).unwrap()));
    }

    #[test]
    fn logical_generation_delete_then_data_restores_effective_key() {
        let profile = scalar_profile();
        let resolved = fold_segments(
            &profile,
            &[
                (1, CompactSegmentRole::Deletion, vec![7]),
                (2, CompactSegmentRole::Data, vec![7]),
            ],
            2,
        )
        .unwrap();
        assert!(resolved.contains(&profile.decode_key(&scalar_key(7)).unwrap()));
    }

    #[test]
    fn logical_generation_rejects_same_key_at_same_generation() {
        let profile = scalar_profile();
        assert_eq!(
            fold_segments(
                &profile,
                &[
                    (1, CompactSegmentRole::Data, vec![7]),
                    (1, CompactSegmentRole::Deletion, vec![7]),
                ],
                2,
            ),
            Err(LogicalMergeError::DuplicateKeyGeneration)
        );
    }

    #[test]
    fn logical_generation_rejects_out_of_order_same_generation_duplicates() {
        let profile = scalar_profile();
        for segments in [
            vec![
                (1, CompactSegmentRole::Data, vec![7]),
                (2, CompactSegmentRole::Deletion, vec![7]),
                (1, CompactSegmentRole::Replacement, vec![7]),
            ],
            vec![
                (2, CompactSegmentRole::Data, vec![7]),
                (1, CompactSegmentRole::Deletion, vec![7]),
                (1, CompactSegmentRole::Data, vec![7]),
            ],
        ] {
            assert_eq!(
                fold_segments(&profile, &segments, 3),
                Err(LogicalMergeError::DuplicateKeyGeneration)
            );
        }
    }

    #[test]
    fn logical_generation_enforces_key_budget() {
        let profile = scalar_profile();
        assert_eq!(
            fold_segments(&profile, &[(1, CompactSegmentRole::Data, vec![7, 8])], 1,),
            Err(LogicalMergeError::KeyBudgetExceeded { limit: 1 })
        );
    }

    #[test]
    fn logical_generation_rejects_truncated_exact_key_stream() {
        let profile = scalar_profile();
        let mut resolved = LogicalKeyAccumulator::new(4);
        assert_eq!(
            consume_entry_keys(
                &profile,
                &mut resolved,
                11,
                CompactSegmentRole::Data,
                2,
                vec![Ok::<Vec<u8>, ()>(scalar_key(7))],
            ),
            Err(CompactLogicalKeyError::KeyCountMismatch {
                generation: 11,
                expected: 2,
                observed: 1,
            })
        );
    }

    #[test]
    fn logical_generation_rejects_overlong_exact_key_stream() {
        let profile = scalar_profile();
        let mut resolved = LogicalKeyAccumulator::new(4);
        assert_eq!(
            consume_entry_keys(
                &profile,
                &mut resolved,
                12,
                CompactSegmentRole::Data,
                1,
                vec![
                    Ok::<Vec<u8>, ()>(scalar_key(7)),
                    Ok::<Vec<u8>, ()>(scalar_key(8)),
                ],
            ),
            Err(CompactLogicalKeyError::KeyCountMismatch {
                generation: 12,
                expected: 1,
                observed: 2,
            })
        );
    }

    #[test]
    fn tuple_components_follow_primary_key_order_not_field_id_order() {
        let profile = reordered_profile();
        assert_eq!(
            profile.key_field_ids().collect::<Vec<_>>(),
            vec![KEY_B, KEY_A]
        );
        assert!(profile.decode_key(&tuple_key()).is_ok());

        let field_id_order = CanonicalValue::new(OvbRaw::Tag(
            60015,
            Box::new(OvbRaw::Array(vec![
                OvbRaw::Int(7.into()),
                OvbRaw::Text("west".into()),
            ])),
        ))
        .unwrap()
        .encode()
        .unwrap();
        assert_eq!(
            profile.decode_key(&field_id_order),
            Err(CompactKeyError::KeyTypeMismatch)
        );
    }

    #[test]
    fn corrupt_record_and_noncanonical_decimal_inputs_fail_closed() {
        let profile = scalar_profile();
        let mut corrupt = scalar_key(42);
        corrupt.pop();
        assert_eq!(
            profile.decode_key(&corrupt),
            Err(CompactKeyError::InvalidOvb)
        );

        let record = CanonicalValue::new(OvbRaw::Tag(
            60009,
            Box::new(OvbRaw::Array(vec![OvbRaw::Null, OvbRaw::Array(vec![])])),
        ))
        .unwrap()
        .encode()
        .unwrap();
        assert_eq!(
            profile.decode_key(&record),
            Err(CompactKeyError::RecordIsNotKey)
        );

        let decimal = decimal_profile();
        let canonical = CanonicalValue::new(OvbRaw::Tag(
            60000,
            Box::new(OvbRaw::Array(vec![
                OvbRaw::Int(1.into()),
                OvbRaw::Int((-1).into()),
            ])),
        ))
        .unwrap()
        .encode()
        .unwrap();
        assert!(decimal.decode_key(&canonical).is_ok());
        // 10e-2 is numerically equal to 1e-1 but is not canonical OVB.
        assert_eq!(
            decimal.decode_key(&[0xd9, 0xea, 0x60, 0x82, 0x0a, 0x21]),
            Err(CompactKeyError::InvalidOvb)
        );
    }

    #[test]
    fn exact_index_rejects_duplicate_keys_without_range_inference() {
        let profile = scalar_profile();
        let existing = [scalar_key(7), scalar_key(42)];
        let index = profile
            .exact_key_index(existing.iter().map(Vec::as_slice))
            .unwrap();
        assert_eq!(index.len(), 2);
        assert_eq!(
            CompactLogicalReader::new(profile.clone())
                .validate_key_against_index(&scalar_key(42), &index),
            Err(CompactKeyError::DuplicateKey)
        );
        assert!(
            CompactLogicalReader::new(profile)
                .validate_key_against_index(&scalar_key(43), &index)
                .is_ok()
        );
    }

    #[test]
    fn committed_base_consumes_generated_writer_input_identity() {
        let state = CompactBaseState {
            table_id: TABLE,
            schema_fingerprint: [0x41; 32],
            rows: BTreeMap::new(),
        };
        let input = CompactWriterInput {
            table_id: TABLE,
            schema_fingerprint: [0x41; 32],
            candidate_generation: 1,
            row_encoding_identity: PublicationRowEncoding::CompactOvb1,
            value_encoding_identity: PublicationValueEncoding::Ovb1,
            mutations: Vec::new(),
            candidate_digest: [0x42; 32],
        };
        assert!(state.consume_writer_input(&input).is_ok());
        assert_eq!(
            state.consume_writer_input(&CompactWriterInput {
                table_id: OTHER_TABLE,
                ..input
            }),
            Err(CompactBaseProjectionError::WrongTable)
        );
    }

    #[test]
    fn committed_base_requires_exact_next_generation() {
        let profile = scalar_profile();
        let key = profile.decode_key(&scalar_key(7)).unwrap();
        let mut rows = BTreeMap::new();
        rows.insert(
            key.clone(),
            CompactBaseRow {
                key: CanonicalValue::decode(&scalar_key(7)).unwrap(),
                value: Some(row_value(7)),
                generation: 4,
                role: CompactSegmentRole::Data,
            },
        );
        let state = CompactBaseState {
            table_id: TABLE,
            schema_fingerprint: profile.schema_fingerprint(),
            rows,
        };
        let input = CompactWriterInput {
            table_id: TABLE,
            schema_fingerprint: profile.schema_fingerprint(),
            candidate_generation: 6,
            row_encoding_identity: PublicationRowEncoding::CompactOvb1,
            value_encoding_identity: PublicationValueEncoding::Ovb1,
            mutations: Vec::new(),
            candidate_digest: [0x42; 32],
        };
        assert_eq!(
            state.consume_writer_input(&input),
            Err(CompactBaseProjectionError::StaleGeneration)
        );
        assert!(state
            .consume_writer_input(&CompactWriterInput {
                candidate_generation: 5,
                ..input
            })
            .is_ok());
    }

    #[test]
    fn committed_base_rejects_duplicate_writer_keys_at_generation() {
        let profile = scalar_profile();
        let key = profile.decode_key(&scalar_key(7)).unwrap();
        let state = CompactBaseState {
            table_id: TABLE,
            schema_fingerprint: profile.schema_fingerprint(),
            rows: BTreeMap::new(),
        };
        let input = CompactWriterInput {
            table_id: TABLE,
            schema_fingerprint: profile.schema_fingerprint(),
            candidate_generation: 1,
            row_encoding_identity: PublicationRowEncoding::CompactOvb1,
            value_encoding_identity: PublicationValueEncoding::Ovb1,
            mutations: vec![
                CompactWriterMutation {
                    sequence: 1,
                    mutation_id: [1; 16],
                    key: key.clone(),
                    state: CompactWriterMutationState::Deletion,
                },
                CompactWriterMutation {
                    sequence: 2,
                    mutation_id: [2; 16],
                    key,
                    state: CompactWriterMutationState::Deletion,
                },
            ],
            candidate_digest: [0x42; 32],
        };
        assert_eq!(
            state.consume_writer_input(&input),
            Err(CompactBaseProjectionError::DuplicateKeyGeneration)
        );
    }
    #[test]
    fn evolution_rekey_consumes_verified_base_and_emits_writer_input() {
        let profile = scalar_profile();
        let table = orna_evolution_v1::ObjectId::new(TABLE);
        let field = orna_evolution_v1::Field {
            id: orna_evolution_v1::ObjectId::new(KEY_A),
            name: "id".into(),
            ty: orna_evolution_v1::FieldType::Int,
            role: orna_evolution_v1::FieldRole::Key,
            optional: false,
            introduction_fallback: None,
        };
        let schema = orna_evolution_v1::Schema {
            version: orna_evolution_v1::EvolutionVersion::V1_0,
            tables: vec![orna_evolution_v1::Table {
                id: table,
                name: "accounts".into(),
                explicit_key: true,
                fields: vec![field],
            }],
        };
        let plan = orna_evolution_v1::plan(
            &schema,
            &schema,
            &orna_evolution_v1::PlanningRequest {
                fence: orna_evolution_v1::VersionFence::V1,
                rekeys: vec![orna_evolution_v1::RekeyIntent {
                    table,
                    old_key: CanonicalValue::new(OvbRaw::Int(7.into())).unwrap(),
                    new_key: CanonicalValue::new(OvbRaw::Int(8.into())).unwrap(),
                }],
            },
        )
        .unwrap();
        let old = profile.decode_key(&scalar_key(7)).unwrap();
        let mut rows = BTreeMap::new();
        rows.insert(
            old,
            CompactBaseRow {
                key: CanonicalValue::decode(&scalar_key(7)).unwrap(),
                value: Some(row_value(7)),
                generation: 1,
                role: CompactSegmentRole::Data,
            },
        );
        let base = CompactBaseState {
            table_id: TABLE,
            schema_fingerprint: profile.schema_fingerprint(),
            rows,
        };
        let input = apply_migration_plan_to_compact(
            &profile,
            &base,
            &plan,
            2,
            &[[0x55; 16], [0x56; 16]],
            [0x66; 32],
        )
        .unwrap();
        assert_eq!(input.candidate_generation, 2);
        assert_eq!(input.mutations.len(), 2);
        assert_eq!(input.mutations[0].key.encoded(), scalar_key(7));
        assert!(matches!(
            input.mutations[0].state,
            CompactWriterMutationState::Deletion
        ));
        assert_eq!(input.mutations[1].key.encoded(), scalar_key(8));
        assert!(matches!(
            input.mutations[1].state,
            CompactWriterMutationState::Replacement { .. }
        ));
        let CompactWriterMutationState::Replacement { value } = &input.mutations[1].state else {
            panic!("rekey must emit a replacement");
        };
        assert_eq!(CanonicalValue::decode(value).unwrap(), row_value(8));

        let mut evolved_schema = schema.clone();
        evolved_schema.tables[0].fields.push(orna_evolution_v1::Field {
            id: orna_evolution_v1::ObjectId::new(KEY_B),
            name: "label".into(),
            ty: orna_evolution_v1::FieldType::Str,
            role: orna_evolution_v1::FieldRole::Stored,
            optional: true,
            introduction_fallback: None,
        });
        let schema_plan = orna_evolution_v1::plan(
            &schema,
            &evolved_schema,
            &orna_evolution_v1::PlanningRequest {
                fence: orna_evolution_v1::VersionFence::V1,
                rekeys: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(
            apply_migration_plan_to_compact(
                &profile,
                &base,
                &schema_plan,
                2,
                &[[0x57; 16], [0x58; 16]],
                [0x66; 32],
            ),
            Err(CompactBaseProjectionError::UnsupportedEvolutionOperation)
        );
    }
}
