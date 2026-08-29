//! Backend-neutral planning for supported durable object storage changes.

use std::{error::Error, fmt};

use crate::{
    CatalogueRevisionId, FieldId, SourceRevisionId, TypeId,
    catalogue::{
        FieldDefinition, ObjectTypeDefinition, OnDeleteAction, ValueTypeDefinition, ValueTypeKind,
        ValueTypeMutability, ValueTypePersistence,
    },
    revision::{ActiveDatabaseRevision, DeployableRevision, RevisionPair},
    types::StandardScalar,
};

/// Plans the physical changes supported by the initial durable-object slice.
///
/// The result contains no backend names, types, or statements. Unsupported
/// changes fail closed before a storage adapter receives a plan.
pub fn plan_physical_changes(
    active: &ActiveDatabaseRevision,
    candidate: &DeployableRevision,
) -> Result<PhysicalPlan, PhysicalPlanError> {
    if candidate.expected_base() != active.pair() {
        return Err(PhysicalPlanError::ExpectedBaseMismatch {
            expected: candidate.expected_base(),
            active: active.pair(),
        });
    }

    for active_object in active.catalogue().object_types() {
        if candidate
            .candidate()
            .object_type_by_id(active_object.id())
            .is_none()
        {
            return Err(PhysicalPlanError::UnsupportedObjectDrop {
                object_type: active_object.id(),
            });
        }
    }

    let active_revision = PhysicalRevision::Active(active);
    let candidate_revision = PhysicalRevision::Deployable(candidate);

    let mut add_field = None;
    for active_object in active.catalogue().object_types() {
        let candidate_object = candidate
            .candidate()
            .object_type_by_id(active_object.id())
            .ok_or(PhysicalPlanError::UnsupportedObjectDrop {
                object_type: active_object.id(),
            })?;
        let appended_value_type = candidate_object
            .fields()
            .get(active_object.fields().len()..)
            .and_then(|added| {
                let [field] = added else {
                    return None;
                };
                field.resolved_type().value_type()
            });
        let active_projection = project_physical_object(active_revision, active_object)?;
        let candidate_projection = project_physical_object(candidate_revision, candidate_object)?;
        if active_projection != candidate_projection {
            if add_field.is_some() {
                return Err(PhysicalPlanError::UnsupportedExistingObjectChange {
                    object_type: active_object.id(),
                });
            }
            let field = appended_nullable_value_scalar_field(
                &active_projection,
                &candidate_projection,
                appended_value_type,
            )
            .ok_or(PhysicalPlanError::UnsupportedExistingObjectChange {
                object_type: active_object.id(),
            })?;
            add_field = Some(AddField {
                object_type: active_object.id(),
                field,
            });
        }
    }

    let create_objects = candidate
        .candidate()
        .object_types()
        .iter()
        .filter(|object_type| {
            active
                .catalogue()
                .object_type_by_id(object_type.id())
                .is_none()
        })
        .map(|object_type| project_physical_object(candidate_revision, object_type))
        .collect::<Result<_, _>>()?;

    Ok(PhysicalPlan {
        create_objects,
        add_field,
    })
}

fn appended_nullable_value_scalar_field(
    active: &CreateObject,
    candidate: &CreateObject,
    appended_value_type: Option<TypeId>,
) -> Option<CreateField> {
    let added = candidate.fields.strip_prefix(active.fields.as_slice())?;
    let [field] = added else {
        return None;
    };
    if appended_value_type.is_none()
        || !matches!(
            field.field_type,
            PhysicalFieldType::Scalar(
                StandardScalar::Boolean
                    | StandardScalar::Integer
                    | StandardScalar::BigInt
                    | StandardScalar::Float
                    | StandardScalar::CharacterLargeObject
                    | StandardScalar::BinaryLargeObject
            )
        )
        || !field.nullable
        || field.unique
    {
        return None;
    }
    Some(field.clone())
}

/// Projects an active catalogue into physical object storage facts.
pub fn active_physical_catalogue(
    active: &ActiveDatabaseRevision,
) -> Result<PhysicalCatalogue, PhysicalPlanError> {
    let revision = PhysicalRevision::Active(active);
    let objects = active
        .catalogue()
        .object_types()
        .iter()
        .map(|object_type| project_physical_object(revision, object_type))
        .collect::<Result<_, _>>()?;
    Ok(PhysicalCatalogue { objects })
}

/// One complete ordered physical catalogue projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PhysicalCatalogue {
    objects: Vec<CreateObject>,
}

impl PhysicalCatalogue {
    /// Returns physical objects in catalogue snapshot order.
    pub fn objects(&self) -> &[CreateObject] {
        &self.objects
    }
}

/// The stable public identity of the physical migration artifact format.
pub const FORMAT_IDENTITY: &str = "orna.migration-ledger";
/// The canonical version of the physical migration artifact format.
pub const FORMAT_VERSION: u32 = 1;
/// The exact header prefix of every physical migration artifact.
pub const MAGIC: [u8; 8] = *b"ORNAML\0\0";

const CREATE_OBJECT_OPERATION_TAG: u8 = 1;
const ADD_FIELD_OPERATION_TAG: u8 = 2;
const SCALAR_FIELD_TYPE_TAG: u8 = 1;
const ENUM_FIELD_TYPE_TAG: u8 = 2;
const RECORD_FIELD_TYPE_TAG: u8 = 3;
const REFERENCE_FIELD_TYPE_TAG: u8 = 4;
const MAX_PHYSICAL_ARTIFACT_BYTES: usize = 16 * 1024 * 1024;
const MAX_PHYSICAL_OPERATIONS: u32 = 65_536;
const MAX_PHYSICAL_FIELDS: u32 = 65_536;

/// An error returned when a physical migration artifact cannot be formed or
/// recovered from canonical bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PhysicalMigrationArtifactError {
    /// The physical planner rejected the active-to-candidate transition.
    Planning(PhysicalPlanError),
    /// Canonical artifact bytes could not represent one collection length.
    CanonicalHash(crate::canonical_hash::CanonicalHashError),
    /// The canonical bytes do not start with the physical artifact magic.
    InvalidMagic,
    /// The canonical bytes use an unsupported format version.
    UnsupportedVersion(u32),
    /// The embedded expected base differs from the supplied revision metadata.
    ExpectedBaseMismatch {
        /// The revision pair supplied by the recovery metadata.
        expected: RevisionPair,
        /// The revision pair embedded in the canonical bytes.
        actual: RevisionPair,
    },
    /// The embedded candidate pair differs from the supplied revision metadata.
    CandidatePairMismatch {
        /// The revision pair supplied by the recovery metadata.
        expected: RevisionPair,
        /// The revision pair embedded in the canonical bytes.
        actual: RevisionPair,
    },
    /// The canonical bytes contain an unknown physical operation tag.
    InvalidOperationTag(u8),
    /// The canonical bytes contain an unknown physical field-type tag.
    InvalidFieldTypeTag(u8),
    /// The canonical bytes contain an unknown standard-scalar tag.
    InvalidScalarTag(u8),
    /// The canonical bytes contain an unknown reference delete-action tag.
    InvalidDeleteTag(u8),
    /// A Boolean field in the canonical bytes was not zero or one.
    InvalidBoolean {
        /// The encoded Boolean category.
        context: &'static str,
        /// The encoded byte.
        value: u8,
    },
    /// A canonical collection exceeds its fixed format bound.
    CollectionLimit {
        /// The collection category.
        kind: &'static str,
        /// The encoded collection length.
        count: usize,
        /// The largest accepted collection length.
        maximum: u32,
    },
    /// The canonical artifact exceeds its fixed byte bound.
    ArtifactSizeLimit {
        /// The supplied artifact size.
        size: usize,
        /// The largest accepted artifact size.
        maximum: usize,
    },
    /// The canonical artifact ends before a complete value can be read.
    Truncated,
    /// The canonical artifact contains bytes after a complete value.
    TrailingBytes,
    /// The bytes do not equal the canonical re-encoding of their decoded model.
    CanonicalBytesMismatch,
    /// The supplied digest does not cover the canonical bytes.
    DigestMismatch {
        /// The digest calculated over the canonical bytes.
        expected: crate::revision::Sha256Digest,
        /// The digest supplied with the canonical bytes.
        actual: crate::revision::Sha256Digest,
    },
}

impl fmt::Display for PhysicalMigrationArtifactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Planning(error) => {
                write!(formatter, "physical migration planning failed: {error}")
            }
            Self::CanonicalHash(error) => {
                write!(
                    formatter,
                    "physical migration artifact encoding failed: {error}"
                )
            }
            Self::InvalidMagic => {
                formatter.write_str("invalid orna.migration-ledger artifact magic")
            }
            Self::UnsupportedVersion(version) => write!(
                formatter,
                "unsupported orna.migration-ledger artifact version {version}"
            ),
            Self::ExpectedBaseMismatch { expected, actual } => write!(
                formatter,
                "physical migration artifact embeds base {actual:?}; recovery metadata supplies {expected:?}"
            ),
            Self::CandidatePairMismatch { expected, actual } => write!(
                formatter,
                "physical migration artifact embeds candidate {actual:?}; recovery metadata supplies {expected:?}"
            ),
            Self::InvalidOperationTag(tag) => {
                write!(formatter, "invalid physical migration operation tag {tag}")
            }
            Self::InvalidFieldTypeTag(tag) => {
                write!(formatter, "invalid physical migration field-type tag {tag}")
            }
            Self::InvalidScalarTag(tag) => {
                write!(formatter, "invalid physical migration scalar tag {tag}")
            }
            Self::InvalidDeleteTag(tag) => {
                write!(
                    formatter,
                    "invalid physical migration delete-action tag {tag}"
                )
            }
            Self::InvalidBoolean { context, value } => {
                write!(formatter, "invalid {context} Boolean byte {value}")
            }
            Self::CollectionLimit {
                kind,
                count,
                maximum,
            } => write!(
                formatter,
                "physical migration {kind} count {count} exceeds the limit {maximum}"
            ),
            Self::ArtifactSizeLimit { size, maximum } => write!(
                formatter,
                "physical migration artifact size {size} exceeds the limit {maximum}"
            ),
            Self::Truncated => formatter.write_str("truncated physical migration artifact"),
            Self::TrailingBytes => {
                formatter.write_str("trailing bytes after physical migration artifact")
            }
            Self::CanonicalBytesMismatch => {
                formatter.write_str("physical migration artifact bytes are not canonical")
            }
            Self::DigestMismatch { expected, actual } => write!(
                formatter,
                "physical migration artifact digest {actual:?} does not match canonical bytes ({expected:?})"
            ),
        }
    }
}

impl Error for PhysicalMigrationArtifactError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Planning(error) => Some(error),
            Self::CanonicalHash(error) => Some(error),
            _ => None,
        }
    }
}

impl From<PhysicalPlanError> for PhysicalMigrationArtifactError {
    fn from(error: PhysicalPlanError) -> Self {
        Self::Planning(error)
    }
}

/// One ordered backend-neutral physical operation in a migration artifact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PhysicalOperation {
    /// Creates one complete durable object relation.
    CreateObject(CreateObject),
    /// Adds one field to one existing durable object relation.
    AddField(AddField),
}

/// One deterministic, revision-bound physical migration artifact.
///
/// The artifact is built from a validated [`PhysicalPlan`]. Operations retain
/// the plan's physical execution order: new objects in candidate catalogue
/// order, followed by the one existing-object field addition when present.
/// Its canonical bytes contain only stable revision identities and typed
/// physical projections.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PhysicalMigrationArtifact {
    expected_base: RevisionPair,
    candidate_pair: RevisionPair,
    operations: Vec<PhysicalOperation>,
    canonical_bytes: Vec<u8>,
    digest: crate::revision::Sha256Digest,
}

impl PhysicalMigrationArtifact {
    /// Builds an artifact by planning the supplied active-to-candidate change.
    pub fn from_revisions(
        active: &ActiveDatabaseRevision,
        candidate: &DeployableRevision,
    ) -> Result<Self, PhysicalMigrationArtifactError> {
        let plan = plan_physical_changes(active, candidate)?;
        Self::from_plan(active.pair(), candidate.candidate_pair(), &plan)
    }

    /// Binds a validated physical plan to its expected base and candidate pair.
    pub fn from_plan(
        expected_base: RevisionPair,
        candidate_pair: RevisionPair,
        plan: &PhysicalPlan,
    ) -> Result<Self, PhysicalMigrationArtifactError> {
        let mut operations = Vec::with_capacity(
            plan.create_objects
                .len()
                .saturating_add(usize::from(plan.add_field.is_some())),
        );
        operations.extend(
            plan.create_objects
                .iter()
                .cloned()
                .map(PhysicalOperation::CreateObject),
        );
        if let Some(add_field) = &plan.add_field {
            operations.push(PhysicalOperation::AddField(add_field.clone()));
        }

        let (canonical_bytes, digest) =
            encode_physical_migration(expected_base, candidate_pair, &operations)?;
        Ok(Self {
            expected_base,
            candidate_pair,
            operations,
            canonical_bytes,
            digest,
        })
    }

    /// Recovers an artifact after validating its canonical bytes and digest.
    ///
    /// The supplied revision pairs are recovery metadata. Both must match the
    /// corresponding revision pair embedded in the canonical bytes.
    pub fn from_canonical_bytes(
        expected_base: RevisionPair,
        candidate_pair: RevisionPair,
        canonical_bytes: &[u8],
        digest: crate::revision::Sha256Digest,
    ) -> Result<Self, PhysicalMigrationArtifactError> {
        if canonical_bytes.len() > MAX_PHYSICAL_ARTIFACT_BYTES {
            return Err(PhysicalMigrationArtifactError::ArtifactSizeLimit {
                size: canonical_bytes.len(),
                maximum: MAX_PHYSICAL_ARTIFACT_BYTES,
            });
        }

        let mut reader = PhysicalMigrationReader::new(canonical_bytes);
        if reader.array::<8>()? != MAGIC {
            return Err(PhysicalMigrationArtifactError::InvalidMagic);
        }
        let version = reader.u32()?;
        if version != FORMAT_VERSION {
            return Err(PhysicalMigrationArtifactError::UnsupportedVersion(version));
        }

        let embedded_expected_base = reader.revision_pair()?;
        if embedded_expected_base != expected_base {
            return Err(PhysicalMigrationArtifactError::ExpectedBaseMismatch {
                expected: expected_base,
                actual: embedded_expected_base,
            });
        }
        let embedded_candidate_pair = reader.revision_pair()?;
        if embedded_candidate_pair != candidate_pair {
            return Err(PhysicalMigrationArtifactError::CandidatePairMismatch {
                expected: candidate_pair,
                actual: embedded_candidate_pair,
            });
        }

        let operation_count =
            reader.sequence_len("physical operations", MAX_PHYSICAL_OPERATIONS)?;
        let mut operations = Vec::with_capacity(operation_count);
        for _ in 0..operation_count {
            operations.push(decode_physical_operation(&mut reader)?);
        }
        reader.require_finished()?;

        let (reencoded_bytes, reencoded_digest) =
            encode_physical_migration(expected_base, candidate_pair, &operations)?;
        if reencoded_bytes.as_slice() != canonical_bytes {
            return Err(PhysicalMigrationArtifactError::CanonicalBytesMismatch);
        }
        if reencoded_digest != digest {
            return Err(PhysicalMigrationArtifactError::DigestMismatch {
                expected: reencoded_digest,
                actual: digest,
            });
        }

        Ok(Self {
            expected_base,
            candidate_pair,
            operations,
            canonical_bytes: canonical_bytes.to_vec(),
            digest,
        })
    }

    /// Returns the source and catalogue revisions that must currently be active.
    pub const fn expected_base(&self) -> RevisionPair {
        self.expected_base
    }

    /// Returns the source and catalogue revisions produced by the candidate.
    pub const fn candidate_pair(&self) -> RevisionPair {
        self.candidate_pair
    }

    /// Returns operations in their deterministic physical execution order.
    pub fn operations(&self) -> &[PhysicalOperation] {
        &self.operations
    }

    /// Returns the complete canonical artifact bytes.
    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical_bytes
    }

    /// Returns the SHA-256 digest of the canonical artifact payload.
    pub const fn digest(&self) -> crate::revision::Sha256Digest {
        self.digest
    }
}

fn encode_physical_migration(
    expected_base: RevisionPair,
    candidate_pair: RevisionPair,
    operations: &[PhysicalOperation],
) -> Result<(Vec<u8>, crate::revision::Sha256Digest), PhysicalMigrationArtifactError> {
    let mut encoder = PhysicalMigrationEncoder::new();
    encoder.bytes(&MAGIC);
    encoder.u32(FORMAT_VERSION);
    encoder.revision_pair(expected_base);
    encoder.revision_pair(candidate_pair);
    encoder.sequence_len(
        operations.len(),
        "physical operations",
        MAX_PHYSICAL_OPERATIONS,
    )?;
    for operation in operations {
        encode_physical_operation(&mut encoder, operation)?;
    }
    let canonical_bytes = encoder.finish();
    if canonical_bytes.len() > MAX_PHYSICAL_ARTIFACT_BYTES {
        return Err(PhysicalMigrationArtifactError::ArtifactSizeLimit {
            size: canonical_bytes.len(),
            maximum: MAX_PHYSICAL_ARTIFACT_BYTES,
        });
    }
    let digest = crate::canonical_hash::artifact_payload_digest(&canonical_bytes)
        .map_err(PhysicalMigrationArtifactError::CanonicalHash)?;
    Ok((canonical_bytes, digest))
}

fn encode_physical_operation(
    encoder: &mut PhysicalMigrationEncoder,
    operation: &PhysicalOperation,
) -> Result<(), PhysicalMigrationArtifactError> {
    match operation {
        PhysicalOperation::CreateObject(object) => {
            encoder.u8(CREATE_OBJECT_OPERATION_TAG);
            encode_create_object(encoder, object)?;
        }
        PhysicalOperation::AddField(add_field) => {
            encoder.u8(ADD_FIELD_OPERATION_TAG);
            encoder.type_id(add_field.object_type());
            encode_create_field(encoder, add_field.field())?;
        }
    }
    Ok(())
}

fn encode_create_object(
    encoder: &mut PhysicalMigrationEncoder,
    object: &CreateObject,
) -> Result<(), PhysicalMigrationArtifactError> {
    encoder.type_id(object.type_id());
    encoder.sequence_len(
        object.fields().len(),
        "physical object fields",
        MAX_PHYSICAL_FIELDS,
    )?;
    for field in object.fields() {
        encode_create_field(encoder, field)?;
    }
    Ok(())
}

fn encode_create_field(
    encoder: &mut PhysicalMigrationEncoder,
    field: &CreateField,
) -> Result<(), PhysicalMigrationArtifactError> {
    encoder.field_id(field.field_id());
    match field.field_type() {
        PhysicalFieldType::Scalar(scalar) => {
            encoder.u8(SCALAR_FIELD_TYPE_TAG);
            encoder.standard_scalar(scalar);
        }
        PhysicalFieldType::Enum(type_id) => {
            encoder.u8(ENUM_FIELD_TYPE_TAG);
            encoder.type_id(type_id);
        }
        PhysicalFieldType::Record(type_id) => {
            encoder.u8(RECORD_FIELD_TYPE_TAG);
            encoder.type_id(type_id);
        }
        PhysicalFieldType::Reference { target, on_delete } => {
            encoder.u8(REFERENCE_FIELD_TYPE_TAG);
            encoder.type_id(target);
            encoder.on_delete(on_delete);
        }
    }
    encoder.boolean(field.nullable());
    encoder.boolean(field.unique());
    Ok(())
}

fn decode_physical_operation(
    reader: &mut PhysicalMigrationReader<'_>,
) -> Result<PhysicalOperation, PhysicalMigrationArtifactError> {
    match reader.u8()? {
        CREATE_OBJECT_OPERATION_TAG => Ok(PhysicalOperation::CreateObject(decode_create_object(
            reader,
        )?)),
        ADD_FIELD_OPERATION_TAG => {
            let object_type = reader.type_id()?;
            Ok(PhysicalOperation::AddField(AddField {
                object_type,
                field: decode_create_field(reader)?,
            }))
        }
        tag => Err(PhysicalMigrationArtifactError::InvalidOperationTag(tag)),
    }
}

fn decode_create_object(
    reader: &mut PhysicalMigrationReader<'_>,
) -> Result<CreateObject, PhysicalMigrationArtifactError> {
    let type_id = reader.type_id()?;
    let field_count = reader.sequence_len("physical object fields", MAX_PHYSICAL_FIELDS)?;
    let mut fields = Vec::with_capacity(field_count);
    for _ in 0..field_count {
        fields.push(decode_create_field(reader)?);
    }
    Ok(CreateObject { type_id, fields })
}

fn decode_create_field(
    reader: &mut PhysicalMigrationReader<'_>,
) -> Result<CreateField, PhysicalMigrationArtifactError> {
    let field_id = reader.field_id()?;
    let field_type = match reader.u8()? {
        SCALAR_FIELD_TYPE_TAG => PhysicalFieldType::Scalar(decode_standard_scalar(reader.u8()?)?),
        ENUM_FIELD_TYPE_TAG => PhysicalFieldType::Enum(reader.type_id()?),
        RECORD_FIELD_TYPE_TAG => PhysicalFieldType::Record(reader.type_id()?),
        REFERENCE_FIELD_TYPE_TAG => PhysicalFieldType::Reference {
            target: reader.type_id()?,
            on_delete: decode_on_delete(reader.u8()?)?,
        },
        tag => return Err(PhysicalMigrationArtifactError::InvalidFieldTypeTag(tag)),
    };
    let nullable = reader.boolean("physical field nullable")?;
    let unique = reader.boolean("physical field unique")?;
    Ok(CreateField {
        field_id,
        field_type,
        nullable,
        unique,
    })
}

fn decode_standard_scalar(tag: u8) -> Result<StandardScalar, PhysicalMigrationArtifactError> {
    match tag {
        1 => Ok(StandardScalar::Boolean),
        2 => Ok(StandardScalar::Integer),
        3 => Ok(StandardScalar::BigInt),
        4 => Ok(StandardScalar::Float),
        5 => Ok(StandardScalar::Decimal),
        6 => Ok(StandardScalar::CharacterLargeObject),
        7 => Ok(StandardScalar::BinaryLargeObject),
        8 => Ok(StandardScalar::Uuid),
        9 => Ok(StandardScalar::Date),
        10 => Ok(StandardScalar::Time),
        11 => Ok(StandardScalar::Timestamp),
        12 => Ok(StandardScalar::Duration),
        13 => Ok(StandardScalar::Void),
        tag => Err(PhysicalMigrationArtifactError::InvalidScalarTag(tag)),
    }
}

fn decode_on_delete(tag: u8) -> Result<Option<OnDeleteAction>, PhysicalMigrationArtifactError> {
    match tag {
        0 => Ok(None),
        1 => Ok(Some(OnDeleteAction::Restrict)),
        2 => Ok(Some(OnDeleteAction::SetNull)),
        3 => Ok(Some(OnDeleteAction::Cascade)),
        tag => Err(PhysicalMigrationArtifactError::InvalidDeleteTag(tag)),
    }
}

struct PhysicalMigrationReader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> PhysicalMigrationReader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], PhysicalMigrationArtifactError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(PhysicalMigrationArtifactError::Truncated)?;
        let bytes = self
            .bytes
            .get(self.position..end)
            .ok_or(PhysicalMigrationArtifactError::Truncated)?;
        self.position = end;
        Ok(bytes)
    }

    fn array<const LENGTH: usize>(
        &mut self,
    ) -> Result<[u8; LENGTH], PhysicalMigrationArtifactError> {
        self.take(LENGTH)?
            .try_into()
            .map_err(|_| PhysicalMigrationArtifactError::Truncated)
    }

    fn u8(&mut self) -> Result<u8, PhysicalMigrationArtifactError> {
        Ok(self.array::<1>()?[0])
    }

    fn u32(&mut self) -> Result<u32, PhysicalMigrationArtifactError> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    fn boolean(&mut self, context: &'static str) -> Result<bool, PhysicalMigrationArtifactError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            value => Err(PhysicalMigrationArtifactError::InvalidBoolean { context, value }),
        }
    }

    fn sequence_len(
        &mut self,
        kind: &'static str,
        maximum: u32,
    ) -> Result<usize, PhysicalMigrationArtifactError> {
        let count = self.u32()? as usize;
        if count > maximum as usize {
            return Err(PhysicalMigrationArtifactError::CollectionLimit {
                kind,
                count,
                maximum,
            });
        }
        Ok(count)
    }

    fn type_id(&mut self) -> Result<TypeId, PhysicalMigrationArtifactError> {
        Ok(TypeId::from_bytes(self.array()?))
    }

    fn field_id(&mut self) -> Result<FieldId, PhysicalMigrationArtifactError> {
        Ok(FieldId::from_bytes(self.array()?))
    }

    fn revision_pair(&mut self) -> Result<RevisionPair, PhysicalMigrationArtifactError> {
        Ok(RevisionPair::new(
            SourceRevisionId::from_bytes(self.array()?),
            CatalogueRevisionId::from_bytes(self.array()?),
        ))
    }

    fn require_finished(&self) -> Result<(), PhysicalMigrationArtifactError> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(PhysicalMigrationArtifactError::TrailingBytes)
        }
    }
}

struct PhysicalMigrationEncoder {
    bytes: Vec<u8>,
}

impl PhysicalMigrationEncoder {
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }

    fn bytes(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn u32(&mut self, value: u32) {
        self.bytes(&value.to_be_bytes());
    }

    fn boolean(&mut self, value: bool) {
        self.u8(u8::from(value));
    }

    fn sequence_len(
        &mut self,
        length: usize,
        value: &'static str,
        maximum: u32,
    ) -> Result<(), PhysicalMigrationArtifactError> {
        if length > maximum as usize {
            return Err(PhysicalMigrationArtifactError::CollectionLimit {
                kind: value,
                count: length,
                maximum,
            });
        }
        let length = u32::try_from(length).map_err(|_| {
            PhysicalMigrationArtifactError::CanonicalHash(
                crate::canonical_hash::CanonicalHashError::LengthExceedsU32 { value, length },
            )
        })?;
        self.u32(length);
        Ok(())
    }

    fn id(&mut self, id: [u8; 16]) {
        self.bytes(&id);
    }

    fn type_id(&mut self, id: TypeId) {
        self.id(id.to_bytes());
    }

    fn field_id(&mut self, id: FieldId) {
        self.id(id.to_bytes());
    }

    fn revision_pair(&mut self, pair: RevisionPair) {
        self.id(pair.source().to_bytes());
        self.id(pair.catalogue().to_bytes());
    }

    fn standard_scalar(&mut self, scalar: StandardScalar) {
        let tag = match scalar {
            StandardScalar::Boolean => 1,
            StandardScalar::Integer => 2,
            StandardScalar::BigInt => 3,
            StandardScalar::Float => 4,
            StandardScalar::Decimal => 5,
            StandardScalar::CharacterLargeObject => 6,
            StandardScalar::BinaryLargeObject => 7,
            StandardScalar::Uuid => 8,
            StandardScalar::Date => 9,
            StandardScalar::Time => 10,
            StandardScalar::Timestamp => 11,
            StandardScalar::Duration => 12,
            StandardScalar::Void => 13,
        };
        self.u8(tag);
    }

    fn on_delete(&mut self, action: Option<OnDeleteAction>) {
        self.u8(match action {
            None => 0,
            Some(OnDeleteAction::Restrict) => 1,
            Some(OnDeleteAction::SetNull) => 2,
            Some(OnDeleteAction::Cascade) => 3,
        });
    }
}

/// One complete ordered set of supported physical changes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PhysicalPlan {
    create_objects: Vec<CreateObject>,
    add_field: Option<AddField>,
}

impl PhysicalPlan {
    /// Constructs an empty physical plan for a historical baseline.
    ///
    /// The baseline records revision lineage when the database predates the
    /// physical planner; it intentionally contains no replayable operations.
    pub const fn empty() -> Self {
        Self {
            create_objects: Vec::new(),
            add_field: None,
        }
    }

    /// Returns new durable object relations in candidate catalogue order.
    pub fn create_objects(&self) -> &[CreateObject] {
        &self.create_objects
    }

    /// Returns the one admitted existing-object field addition, when present.
    pub const fn add_field(&self) -> Option<&AddField> {
        self.add_field.as_ref()
    }
}

/// One appended field on one existing durable object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AddField {
    object_type: TypeId,
    field: CreateField,
}

impl AddField {
    /// Returns the stable identity of the existing object type.
    pub const fn object_type(&self) -> TypeId {
        self.object_type
    }

    /// Returns the appended field's backend-neutral physical projection.
    pub const fn field(&self) -> &CreateField {
        &self.field
    }
}

/// One physical object projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateObject {
    type_id: TypeId,
    fields: Vec<CreateField>,
}

impl CreateObject {
    /// Returns the stable object-type identity.
    pub const fn type_id(&self) -> TypeId {
        self.type_id
    }

    /// Returns physical fields in declaration ordinal order.
    pub fn fields(&self) -> &[CreateField] {
        &self.fields
    }
}

/// One backend-neutral physical field projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateField {
    field_id: FieldId,
    field_type: PhysicalFieldType,
    nullable: bool,
    unique: bool,
}

impl CreateField {
    /// Returns the stable field identity.
    pub const fn field_id(&self) -> FieldId {
        self.field_id
    }

    /// Returns the backend-neutral storage type.
    pub const fn field_type(&self) -> PhysicalFieldType {
        self.field_type
    }

    /// Reports whether the physical field can contain null.
    pub const fn nullable(&self) -> bool {
        self.nullable
    }

    /// Reports whether the physical field requires one-column uniqueness.
    pub const fn unique(&self) -> bool {
        self.unique
    }
}

/// The closed field types supported by initial physical creation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhysicalFieldType {
    /// A standard scalar with a backend storage encoding.
    Scalar(StandardScalar),
    /// An application enum stored through its stable catalogue identity.
    Enum(TypeId),
    /// A nominal record value stored as canonical Orna value bytes.
    Record(TypeId),
    /// A typed object reference with its delete action.
    Reference {
        /// The referenced durable object type.
        target: TypeId,
        /// The selected delete action, or the language default.
        on_delete: Option<OnDeleteAction>,
    },
}

/// A fail-closed error returned for an unsupported physical change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PhysicalPlanError {
    /// The deployable candidate does not target the supplied active pair.
    ExpectedBaseMismatch {
        expected: RevisionPair,
        active: RevisionPair,
    },
    /// An active durable object type is absent from the complete candidate.
    UnsupportedObjectDrop { object_type: TypeId },
    /// An active durable object type has an unsupported storage change.
    UnsupportedExistingObjectChange { object_type: TypeId },
    /// A new field uses a named value type without a storage contract.
    UnsupportedNamedFieldType { object_type: TypeId, field: FieldId },
    /// A record field is nullable before nullable record values are defined.
    UnsupportedNullableRecordField { object_type: TypeId, field: FieldId },
    /// A resolved value type is absent from the pinned standard library.
    MissingValueTypeDefinition {
        object_type: TypeId,
        field: FieldId,
        value_type: TypeId,
    },
    /// A resolved value type does not have a supported physical contract.
    UnsupportedValueTypeContract {
        object_type: TypeId,
        field: FieldId,
        value_type: TypeId,
        contract: String,
    },
    /// A resolved value type is valid only in transient positions.
    TransientValueType {
        object_type: TypeId,
        field: FieldId,
        value_type: TypeId,
    },
    /// A new field uses the non-storable VOID scalar.
    UnsupportedVoidField { object_type: TypeId, field: FieldId },
    /// The field requests uniqueness outside the Text or required-Reference shapes.
    UnsupportedUniqueField { object_type: TypeId, field: FieldId },
    /// Initial physical creation does not yet install field defaults.
    UnsupportedFieldDefault { object_type: TypeId, field: FieldId },
    /// A typed reference names no object in the complete candidate catalogue.
    UnknownReferenceTarget {
        object_type: TypeId,
        field: FieldId,
        target: TypeId,
    },
    /// A delete action is incompatible with the field type or nullability.
    InvalidDeleteAction { object_type: TypeId, field: FieldId },
}

impl fmt::Display for PhysicalPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExpectedBaseMismatch { .. } => {
                formatter.write_str("physical candidate base is not active")
            }
            Self::UnsupportedObjectDrop { .. } => {
                formatter.write_str("durable object drops are not supported")
            }
            Self::UnsupportedExistingObjectChange { .. } => {
                formatter.write_str("changes to existing object storage are not supported")
            }
            Self::UnsupportedNamedFieldType { .. } => {
                formatter.write_str("named field storage is not supported")
            }
            Self::UnsupportedNullableRecordField { .. } => {
                formatter.write_str("nullable record fields are not supported")
            }
            Self::MissingValueTypeDefinition { .. } => formatter
                .write_str("physical value type is absent from the pinned standard library"),
            Self::UnsupportedValueTypeContract { .. } => {
                formatter.write_str("physical value type contract is not supported")
            }
            Self::TransientValueType { .. } => {
                formatter.write_str("transient value types cannot be stored")
            }
            Self::UnsupportedVoidField { .. } => {
                formatter.write_str("VOID fields cannot be stored")
            }
            Self::UnsupportedUniqueField { .. } => formatter
                .write_str("UNIQUE is supported only for TEXT fields or required REF fields"),
            Self::UnsupportedFieldDefault { .. } => {
                formatter.write_str("physical field defaults are not supported")
            }
            Self::UnknownReferenceTarget { .. } => {
                formatter.write_str("physical REF target is absent from the candidate")
            }
            Self::InvalidDeleteAction { .. } => {
                formatter.write_str("physical field delete action is invalid")
            }
        }
    }
}

impl Error for PhysicalPlanError {}

#[derive(Clone, Copy)]
enum PhysicalRevision<'a> {
    Active(&'a ActiveDatabaseRevision),
    Deployable(&'a DeployableRevision),
}

impl<'a> PhysicalRevision<'a> {
    fn catalogue(self) -> &'a crate::catalogue::CatalogueSnapshot {
        match self {
            Self::Active(active) => active.catalogue(),
            Self::Deployable(candidate) => candidate.candidate(),
        }
    }

    fn standard_catalogue(self) -> Option<&'a crate::catalogue::CatalogueSnapshot> {
        match self {
            Self::Active(active) => active.catalogue_hash_context().standard(),
            Self::Deployable(candidate) => candidate.catalogue_hash_context().standard(),
        }
        .map(crate::revision::VerifiedStandardLibrarySnapshot::catalogue)
    }
}

fn project_physical_object(
    revision: PhysicalRevision<'_>,
    object_type: &ObjectTypeDefinition,
) -> Result<CreateObject, PhysicalPlanError> {
    let fields = object_type
        .fields()
        .iter()
        .map(|field| project_physical_field(revision, object_type.id(), field))
        .collect::<Result<_, _>>()?;
    Ok(CreateObject {
        type_id: object_type.id(),
        fields,
    })
}

fn project_physical_field(
    revision: PhysicalRevision<'_>,
    object_type: TypeId,
    field: &FieldDefinition,
) -> Result<CreateField, PhysicalPlanError> {
    if field.default_expression().is_some() && !field.unique() {
        return Err(PhysicalPlanError::UnsupportedFieldDefault {
            object_type,
            field: field.id(),
        });
    }
    let resolved_type = field.resolved_type();
    let legacy_scalar = resolved_type.legacy_scalar();
    let named_type = resolved_type.named_type();
    let reference_target = resolved_type.reference_target();
    let value_type = resolved_type.value_type();

    let projected = (|| {
        if let Some(scalar) = legacy_scalar {
            if field.unique() && revision.standard_catalogue().is_some() {
                Err(PhysicalPlanError::UnsupportedUniqueField {
                    object_type,
                    field: field.id(),
                })
            } else {
                Ok(PhysicalFieldType::Scalar(scalar))
            }
        } else if let Some(named_type) = named_type {
            if revision.catalogue().enum_type_by_id(named_type).is_some() {
                Ok(PhysicalFieldType::Enum(named_type))
            } else if revision
                .catalogue()
                .record_value_type_by_id(named_type)
                .is_some()
            {
                if field.nullable() {
                    return Err(PhysicalPlanError::UnsupportedNullableRecordField {
                        object_type,
                        field: field.id(),
                    });
                }
                Ok(PhysicalFieldType::Record(named_type))
            } else {
                Err(PhysicalPlanError::UnsupportedNamedFieldType {
                    object_type,
                    field: field.id(),
                })
            }
        } else if let Some(target) = reference_target {
            if revision.catalogue().object_type_by_id(target).is_none() {
                return Err(PhysicalPlanError::UnknownReferenceTarget {
                    object_type,
                    field: field.id(),
                    target,
                });
            }
            Ok(PhysicalFieldType::Reference {
                target,
                on_delete: field.on_delete(),
            })
        } else if let Some(value_type) = value_type {
            project_value_type(revision, object_type, field.id(), value_type)
        } else {
            // Unknown resolved-type projections must fail closed.
            Err(PhysicalPlanError::UnsupportedNamedFieldType {
                object_type,
                field: field.id(),
            })
        }
    })();
    let field_type = if field.unique() {
        // The unique-shape error remains authoritative for every closed type.
        // Projection errors stay exact for fields that do not request UNIQUE.
        match projected {
            Ok(field_type @ PhysicalFieldType::Scalar(StandardScalar::CharacterLargeObject)) => {
                field_type
            }
            Ok(field_type @ PhysicalFieldType::Reference { .. }) if !field.nullable() => field_type,
            Ok(_) | Err(_) => {
                return Err(PhysicalPlanError::UnsupportedUniqueField {
                    object_type,
                    field: field.id(),
                });
            }
        }
    } else {
        projected?
    };

    if field_type == PhysicalFieldType::Scalar(StandardScalar::Void) {
        return Err(PhysicalPlanError::UnsupportedVoidField {
            object_type,
            field: field.id(),
        });
    }
    if field.default_expression().is_some() {
        return Err(PhysicalPlanError::UnsupportedFieldDefault {
            object_type,
            field: field.id(),
        });
    }
    if let PhysicalFieldType::Reference { .. } = field_type
        && field.on_delete() == Some(OnDeleteAction::SetNull)
        && !field.nullable()
    {
        return Err(PhysicalPlanError::InvalidDeleteAction {
            object_type,
            field: field.id(),
        });
    }
    if !matches!(field_type, PhysicalFieldType::Reference { .. }) && field.on_delete().is_some() {
        return Err(PhysicalPlanError::InvalidDeleteAction {
            object_type,
            field: field.id(),
        });
    }

    Ok(CreateField {
        field_id: field.id(),
        field_type,
        nullable: field.nullable(),
        unique: field.unique(),
    })
}

fn project_value_type(
    revision: PhysicalRevision<'_>,
    object_type: TypeId,
    field: FieldId,
    value_type: TypeId,
) -> Result<PhysicalFieldType, PhysicalPlanError> {
    let definition = revision
        .standard_catalogue()
        .and_then(|catalogue| catalogue.value_type_by_id(value_type));
    project_value_type_definition(definition, object_type, field, value_type)
}

fn project_value_type_definition(
    definition: Option<&ValueTypeDefinition>,
    object_type: TypeId,
    field: FieldId,
    value_type: TypeId,
) -> Result<PhysicalFieldType, PhysicalPlanError> {
    let Some(definition) = definition else {
        return Err(PhysicalPlanError::MissingValueTypeDefinition {
            object_type,
            field,
            value_type,
        });
    };
    let contract = definition.representation_contract();
    if definition.kind() != ValueTypeKind::Primitive
        || definition.mutability() != ValueTypeMutability::Immutable
    {
        return Err(PhysicalPlanError::UnsupportedValueTypeContract {
            object_type,
            field,
            value_type,
            contract: contract.to_owned(),
        });
    }
    let scalar = match contract {
        "orna.kernel.value.boolean@1" => StandardScalar::Boolean,
        "orna.kernel.value.integer@1" => StandardScalar::Integer,
        "orna.kernel.value.bigint@1" => StandardScalar::BigInt,
        "orna.kernel.value.float@1" => StandardScalar::Float,
        "orna.kernel.value.decimal@1" => StandardScalar::Decimal,
        "orna.kernel.value.character-large-object@1" => StandardScalar::CharacterLargeObject,
        "orna.kernel.value.binary-large-object@1" => StandardScalar::BinaryLargeObject,
        "orna.kernel.value.uuid@1" => StandardScalar::Uuid,
        "orna.kernel.value.date@1" => StandardScalar::Date,
        "orna.kernel.value.time@1" => StandardScalar::Time,
        "orna.kernel.value.timestamp@1" => StandardScalar::Timestamp,
        "orna.kernel.value.duration@1" => StandardScalar::Duration,
        "orna.kernel.value.void@1" => StandardScalar::Void,
        _ => {
            return Err(PhysicalPlanError::UnsupportedValueTypeContract {
                object_type,
                field,
                value_type,
                contract: contract.to_owned(),
            });
        }
    };
    match definition.persistence() {
        ValueTypePersistence::Persistable => {}
        ValueTypePersistence::Transient if scalar != StandardScalar::Void => {
            return Err(PhysicalPlanError::TransientValueType {
                object_type,
                field,
                value_type,
            });
        }
        ValueTypePersistence::Transient => {}
    }
    Ok(PhysicalFieldType::Scalar(scalar))
}

#[cfg(test)]
mod tests {
    use crate::canonical_hash::{
        calculate_standard_library_digest_for_test, source_bundle_digest,
        source_revision_record_digest, source_unit_content_digest,
        verify_standard_library_snapshot,
    };
    use crate::{
        CatalogueRevisionId, ExpressionId, FieldId, SchemaId, SourceBundleId, SourceRevisionId,
        SourceUnitId, StandardLibraryRevisionId, TypeId,
        catalogue::{
            CatalogueSnapshot, EnumTypeDefinition, FieldDefinition, ObjectTypeDefinition,
            QualifiedSemanticName, RecordValueFieldDefinition, RecordValueTypeDefinition,
            SchemaDefinition, ValueTypeDefinition, ValueTypeMutability, ValueTypePersistence,
        },
        revision::{
            ActiveDatabaseRevision, ActiveDatabaseRevisionInput, ActiveRevisionContent,
            CatalogueHashContext, DefinitionIdentity, DefinitionOrigin, DeployableRevision,
            DeployableRevisionContent, DeployableRevisionInput, RevisionPair, Sha256Digest,
            SourceOrigin, StandardLibraryDigestVersion, StandardLibrarySnapshot,
            StoredSourceRevision, StoredSourceUnit,
        },
        types::{ResolvedType, TypeDescriptor},
    };

    use super::*;

    const SCHEMA_ID: SchemaId = SchemaId::from_bytes([1; 16]);
    const FIRST_TYPE: TypeId = TypeId::from_bytes([10; 16]);
    const SECOND_TYPE: TypeId = TypeId::from_bytes([11; 16]);
    const FIRST_FIELD: FieldId = FieldId::from_bytes([20; 16]);
    const SECOND_FIELD: FieldId = FieldId::from_bytes([21; 16]);
    const STANDARD_SCHEMA_ID: SchemaId = SchemaId::from_bytes([30; 16]);
    const STANDARD_TYPES_SCHEMA_ID: SchemaId = SchemaId::from_bytes([31; 16]);

    #[test]
    fn exact_existing_objects_need_no_physical_change() {
        let object = object(
            FIRST_TYPE,
            "first",
            vec![field(
                FIRST_FIELD,
                "value",
                0,
                ResolvedType::scalar(StandardScalar::Integer),
                true,
            )],
        );
        let active = active(vec![object.clone()], 1);
        let candidate = candidate(&active, vec![object], 2);

        assert_eq!(
            plan_physical_changes(&active, &candidate).unwrap(),
            PhysicalPlan {
                create_objects: Vec::new(),
                add_field: None,
            }
        );
    }

    #[test]
    fn plans_one_appended_nullable_boolean_field_on_an_existing_object() {
        let boolean = TypeId::from_bytes([0xd1; 16]);
        let standard = verified_standard(vec![standard_value_type(
            boolean,
            "orna.kernel.value.boolean@1",
            ValueTypePersistence::Persistable,
        )]);
        let existing = field(
            FIRST_FIELD,
            "first_value",
            0,
            ResolvedType::Value(boolean),
            false,
        );
        let appended = field(
            SECOND_FIELD,
            "second_value",
            1,
            ResolvedType::Value(boolean),
            true,
        );
        let active = active_version_two(
            vec![object(FIRST_TYPE, "first", vec![existing.clone()])],
            standard.clone(),
            1,
        );
        let candidate = candidate_version_two(
            &active,
            vec![object(FIRST_TYPE, "first", vec![existing, appended])],
            standard,
            2,
        );

        let plan = plan_physical_changes(&active, &candidate).unwrap();
        assert!(
            plan.create_objects().is_empty(),
            "appending one field must not plan a new object relation"
        );
        let add_field = plan.add_field().expect("one field append must be planned");
        assert_eq!(
            add_field.object_type(),
            FIRST_TYPE,
            "the appended field must belong to the existing object type"
        );
        let planned = add_field.field();
        assert_eq!(
            planned.field_id(),
            SECOND_FIELD,
            "the appended field must be the second field"
        );
        assert_eq!(
            planned.field_type(),
            PhysicalFieldType::Scalar(StandardScalar::Boolean),
            "the appended field must be a Boolean scalar"
        );
        assert!(planned.nullable(), "the appended field must be nullable");
        assert!(!planned.unique(), "the appended field must not be unique");
    }

    #[test]
    fn replayed_existing_object_with_both_fields_plans_no_change() {
        let fields = vec![
            field(
                FIRST_FIELD,
                "first_value",
                0,
                ResolvedType::scalar(StandardScalar::Boolean),
                false,
            ),
            field(
                SECOND_FIELD,
                "second_value",
                1,
                ResolvedType::scalar(StandardScalar::Boolean),
                true,
            ),
        ];
        let active = active(vec![object(FIRST_TYPE, "first", fields.clone())], 1);
        let candidate = candidate(&active, vec![object(FIRST_TYPE, "first", fields)], 2);

        let plan = plan_physical_changes(&active, &candidate).unwrap();
        assert!(
            plan.create_objects().is_empty(),
            "an exact replay must not plan any new object"
        );
        assert!(
            plan.add_field().is_none(),
            "an exact replay must not plan any field addition"
        );
    }

    #[test]
    fn semantic_name_changes_do_not_block_one_appended_nullable_boolean() {
        let boolean = TypeId::from_bytes([0xd1; 16]);
        let standard = verified_standard(vec![standard_value_type(
            boolean,
            "orna.kernel.value.boolean@1",
            ValueTypePersistence::Persistable,
        )]);
        let active = active_version_two(
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "first_value",
                    0,
                    ResolvedType::Value(boolean),
                    false,
                )],
            )],
            standard.clone(),
            1,
        );
        let candidate = candidate_version_two(
            &active,
            vec![object(
                FIRST_TYPE,
                "renamed",
                vec![
                    field(
                        FIRST_FIELD,
                        "renamed_value",
                        0,
                        ResolvedType::Value(boolean),
                        false,
                    ),
                    field(
                        SECOND_FIELD,
                        "second_value",
                        1,
                        ResolvedType::Value(boolean),
                        true,
                    ),
                ],
            )],
            standard,
            2,
        );

        let plan = plan_physical_changes(&active, &candidate).unwrap();
        assert!(
            plan.create_objects().is_empty(),
            "a renamed existing object must not plan a new relation"
        );
        let add_field = plan
            .add_field()
            .expect("the renamed object must plan its addition");
        assert_eq!(add_field.object_type(), FIRST_TYPE);
        let planned = add_field.field();
        assert_eq!(planned.field_id(), SECOND_FIELD);
        assert_eq!(
            planned.field_type(),
            PhysicalFieldType::Scalar(StandardScalar::Boolean)
        );
        assert!(planned.nullable() && !planned.unique());
    }

    #[test]
    fn one_new_object_and_one_appended_field_share_one_plan() {
        let boolean = TypeId::from_bytes([0xd1; 16]);
        let standard = verified_standard(vec![standard_value_type(
            boolean,
            "orna.kernel.value.boolean@1",
            ValueTypePersistence::Persistable,
        )]);
        let active = active_version_two(
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "first_value",
                    0,
                    ResolvedType::Value(boolean),
                    false,
                )],
            )],
            standard.clone(),
            1,
        );
        let candidate = candidate_version_two(
            &active,
            vec![
                object(
                    FIRST_TYPE,
                    "first",
                    vec![
                        field(
                            FIRST_FIELD,
                            "first_value",
                            0,
                            ResolvedType::Value(boolean),
                            false,
                        ),
                        field(
                            SECOND_FIELD,
                            "second_value",
                            1,
                            ResolvedType::Value(boolean),
                            true,
                        ),
                    ],
                ),
                object(
                    SECOND_TYPE,
                    "second",
                    vec![field(
                        FIRST_FIELD,
                        "only",
                        0,
                        ResolvedType::Value(boolean),
                        false,
                    )],
                ),
            ],
            standard,
            2,
        );

        let plan = plan_physical_changes(&active, &candidate).unwrap();
        let [created] = plan.create_objects() else {
            panic!("one new object must be planned");
        };
        assert_eq!(created.type_id(), SECOND_TYPE);
        assert_eq!(created.fields().len(), 1);
        let add_field = plan
            .add_field()
            .expect("the field addition must be planned");
        assert_eq!(add_field.object_type(), FIRST_TYPE);
        assert_eq!(add_field.field().field_id(), SECOND_FIELD);
    }

    #[test]
    fn rejected_field_edits_keep_the_exact_existing_object_error() {
        let active = active(
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "first_value",
                    0,
                    ResolvedType::scalar(StandardScalar::Boolean),
                    false,
                )],
            )],
            1,
        );
        let cases = [
            // Appended required Boolean.
            vec![
                field(
                    FIRST_FIELD,
                    "first_value",
                    0,
                    ResolvedType::scalar(StandardScalar::Boolean),
                    false,
                ),
                field(
                    SECOND_FIELD,
                    "second_value",
                    1,
                    ResolvedType::scalar(StandardScalar::Boolean),
                    false,
                ),
            ],
            // Appended nullable non-Boolean scalar.
            vec![
                field(
                    FIRST_FIELD,
                    "first_value",
                    0,
                    ResolvedType::scalar(StandardScalar::Boolean),
                    false,
                ),
                field(
                    SECOND_FIELD,
                    "second_value",
                    1,
                    ResolvedType::scalar(StandardScalar::Integer),
                    true,
                ),
            ],
            // Reordered rather than appended.
            vec![
                field(
                    SECOND_FIELD,
                    "second_value",
                    0,
                    ResolvedType::scalar(StandardScalar::Boolean),
                    true,
                ),
                field(
                    FIRST_FIELD,
                    "first_value",
                    1,
                    ResolvedType::scalar(StandardScalar::Boolean),
                    false,
                ),
            ],
            // The active field type changed.
            vec![field(
                FIRST_FIELD,
                "first_value",
                0,
                ResolvedType::scalar(StandardScalar::Integer),
                false,
            )],
            // The active field nullability changed.
            vec![field(
                FIRST_FIELD,
                "first_value",
                0,
                ResolvedType::scalar(StandardScalar::Boolean),
                true,
            )],
            // Two appended nullable Boolean fields.
            vec![
                field(
                    FIRST_FIELD,
                    "first_value",
                    0,
                    ResolvedType::scalar(StandardScalar::Boolean),
                    false,
                ),
                field(
                    SECOND_FIELD,
                    "second_value",
                    1,
                    ResolvedType::scalar(StandardScalar::Boolean),
                    true,
                ),
                field(
                    FieldId::from_bytes([22; 16]),
                    "third_value",
                    2,
                    ResolvedType::scalar(StandardScalar::Boolean),
                    true,
                ),
            ],
        ];
        for fields in cases {
            let candidate = candidate(&active, vec![object(FIRST_TYPE, "first", fields)], 2);
            assert_eq!(
                plan_physical_changes(&active, &candidate),
                Err(PhysicalPlanError::UnsupportedExistingObjectChange {
                    object_type: FIRST_TYPE,
                }),
                "the rejected field edit must close as the exact existing-object error"
            );
        }
    }

    #[test]
    fn defaulted_and_unique_appended_fields_keep_their_exact_projection_errors() {
        let active = active(
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "first_value",
                    0,
                    ResolvedType::scalar(StandardScalar::Boolean),
                    false,
                )],
            )],
            1,
        );
        let defaulted = candidate(
            &active,
            vec![object(
                FIRST_TYPE,
                "first",
                vec![
                    field(
                        FIRST_FIELD,
                        "first_value",
                        0,
                        ResolvedType::scalar(StandardScalar::Boolean),
                        false,
                    ),
                    field_with_options(
                        SECOND_FIELD,
                        "second_value",
                        1,
                        ResolvedType::scalar(StandardScalar::Boolean),
                        true,
                        false,
                        Some(ExpressionId::from_bytes([0x51; 16])),
                        None,
                    ),
                ],
            )],
            2,
        );
        assert_eq!(
            plan_physical_changes(&active, &defaulted),
            Err(PhysicalPlanError::UnsupportedFieldDefault {
                object_type: FIRST_TYPE,
                field: SECOND_FIELD,
            }),
            "a defaulted appended field must retain the exact default error"
        );

        let unique = candidate(
            &active,
            vec![object(
                FIRST_TYPE,
                "first",
                vec![
                    field(
                        FIRST_FIELD,
                        "first_value",
                        0,
                        ResolvedType::scalar(StandardScalar::Boolean),
                        false,
                    ),
                    field_with_options(
                        SECOND_FIELD,
                        "second_value",
                        1,
                        ResolvedType::scalar(StandardScalar::Boolean),
                        true,
                        true,
                        None,
                        None,
                    ),
                ],
            )],
            2,
        );
        assert_eq!(
            plan_physical_changes(&active, &unique),
            Err(PhysicalPlanError::UnsupportedUniqueField {
                object_type: FIRST_TYPE,
                field: SECOND_FIELD,
            }),
            "a unique appended field must retain the exact projection error"
        );
    }

    #[test]
    fn two_existing_objects_each_appending_a_field_reject_on_the_second() {
        let boolean = TypeId::from_bytes([0xd1; 16]);
        let standard = verified_standard(vec![standard_value_type(
            boolean,
            "orna.kernel.value.boolean@1",
            ValueTypePersistence::Persistable,
        )]);
        let active = active_version_two(
            vec![
                object(
                    FIRST_TYPE,
                    "first",
                    vec![field(
                        FIRST_FIELD,
                        "value",
                        0,
                        ResolvedType::Value(boolean),
                        false,
                    )],
                ),
                object(
                    SECOND_TYPE,
                    "second",
                    vec![field(
                        FIRST_FIELD,
                        "value",
                        0,
                        ResolvedType::Value(boolean),
                        false,
                    )],
                ),
            ],
            standard.clone(),
            1,
        );
        let candidate = candidate_version_two(
            &active,
            vec![
                object(
                    FIRST_TYPE,
                    "first",
                    vec![
                        field(FIRST_FIELD, "value", 0, ResolvedType::Value(boolean), false),
                        field(
                            SECOND_FIELD,
                            "second_value",
                            1,
                            ResolvedType::Value(boolean),
                            true,
                        ),
                    ],
                ),
                object(
                    SECOND_TYPE,
                    "second",
                    vec![
                        field(FIRST_FIELD, "value", 0, ResolvedType::Value(boolean), false),
                        field(
                            SECOND_FIELD,
                            "second_value",
                            1,
                            ResolvedType::Value(boolean),
                            true,
                        ),
                    ],
                ),
            ],
            standard,
            2,
        );

        assert_eq!(
            plan_physical_changes(&active, &candidate),
            Err(PhysicalPlanError::UnsupportedExistingObjectChange {
                object_type: SECOND_TYPE,
            }),
            "the second existing-object addition must reject the whole plan"
        );
    }

    #[test]
    fn invalid_existing_object_transition_hides_a_valid_new_object() {
        let active = active(
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "first_value",
                    0,
                    ResolvedType::scalar(StandardScalar::Boolean),
                    false,
                )],
            )],
            1,
        );
        let candidate = candidate(
            &active,
            vec![
                object(
                    FIRST_TYPE,
                    "first",
                    vec![
                        field(
                            FIRST_FIELD,
                            "first_value",
                            0,
                            ResolvedType::scalar(StandardScalar::Boolean),
                            false,
                        ),
                        field(
                            SECOND_FIELD,
                            "second_value",
                            1,
                            ResolvedType::scalar(StandardScalar::Boolean),
                            false,
                        ),
                    ],
                ),
                object(
                    SECOND_TYPE,
                    "second",
                    vec![field(
                        FIRST_FIELD,
                        "only",
                        0,
                        ResolvedType::scalar(StandardScalar::Boolean),
                        false,
                    )],
                ),
            ],
            2,
        );

        assert_eq!(
            plan_physical_changes(&active, &candidate),
            Err(PhysicalPlanError::UnsupportedExistingObjectChange {
                object_type: FIRST_TYPE,
            }),
            "the existing-object error must precede any new-object planning"
        );
    }

    #[test]
    fn appends_each_admitted_nullable_value_scalar_with_exact_identity() {
        let admitted = [
            (0xd1, "orna.kernel.value.boolean@1", StandardScalar::Boolean),
            (0xd2, "orna.kernel.value.integer@1", StandardScalar::Integer),
            (0xd3, "orna.kernel.value.bigint@1", StandardScalar::BigInt),
            (0xd4, "orna.kernel.value.float@1", StandardScalar::Float),
            (
                0xd5,
                "orna.kernel.value.character-large-object@1",
                StandardScalar::CharacterLargeObject,
            ),
            (
                0xd6,
                "orna.kernel.value.binary-large-object@1",
                StandardScalar::BinaryLargeObject,
            ),
        ];
        let standard = verified_standard(
            admitted
                .iter()
                .map(|(id, contract, _)| {
                    standard_value_type(
                        TypeId::from_bytes([*id; 16]),
                        contract,
                        ValueTypePersistence::Persistable,
                    )
                })
                .collect(),
        );
        let active = active_version_two(
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "stored",
                    0,
                    ResolvedType::Value(TypeId::from_bytes([0xd1; 16])),
                    false,
                )],
            )],
            standard.clone(),
            1,
        );

        for (index, (id, _, scalar)) in admitted.into_iter().enumerate() {
            let value_type = TypeId::from_bytes([id; 16]);
            let candidate = candidate_version_two(
                &active,
                vec![object(
                    FIRST_TYPE,
                    "first",
                    vec![
                        field(
                            FIRST_FIELD,
                            "stored",
                            0,
                            ResolvedType::Value(TypeId::from_bytes([0xd1; 16])),
                            false,
                        ),
                        field(
                            SECOND_FIELD,
                            "added",
                            1,
                            ResolvedType::Value(value_type),
                            true,
                        ),
                    ],
                )],
                standard.clone(),
                u8::try_from(index + 2).unwrap(),
            );

            let plan = plan_physical_changes(&active, &candidate).unwrap();
            assert!(
                plan.create_objects().is_empty(),
                "one appended field must not plan a new object relation"
            );
            let add_field = plan
                .add_field()
                .expect("one admitted value append must be planned");
            assert_eq!(
                add_field.object_type(),
                FIRST_TYPE,
                "the append must target the existing object"
            );
            assert_eq!(
                add_field.field(),
                &CreateField {
                    field_id: SECOND_FIELD,
                    field_type: PhysicalFieldType::Scalar(scalar),
                    nullable: true,
                    unique: false,
                },
                "the append must carry the exact scalar, identity, and nullability"
            );
        }
    }

    #[test]
    fn value_appends_that_break_existing_object_causality_keep_exact_rejections() {
        let boolean = TypeId::from_bytes([0xd1; 16]);
        let standard = verified_standard(vec![standard_value_type(
            boolean,
            "orna.kernel.value.boolean@1",
            ValueTypePersistence::Persistable,
        )]);
        let stored = field(
            FIRST_FIELD,
            "stored",
            0,
            ResolvedType::Value(boolean),
            false,
        );
        let appended = field(SECOND_FIELD, "added", 1, ResolvedType::Value(boolean), true);
        let active = active_version_two(
            vec![object(FIRST_TYPE, "first", vec![stored.clone()])],
            standard.clone(),
            1,
        );
        let expected = Err(PhysicalPlanError::UnsupportedExistingObjectChange {
            object_type: FIRST_TYPE,
        });

        // A required appended value field stays closed.
        let required = candidate_version_two(
            &active,
            vec![object(
                FIRST_TYPE,
                "first",
                vec![
                    stored.clone(),
                    field(
                        SECOND_FIELD,
                        "added",
                        1,
                        ResolvedType::Value(boolean),
                        false,
                    ),
                ],
            )],
            standard.clone(),
            2,
        );
        assert_eq!(
            plan_physical_changes(&active, &required),
            expected,
            "a required appended value field must stay closed"
        );

        // A value field inserted before the active prefix stays closed.
        let inserted = candidate_version_two(
            &active,
            vec![object(
                FIRST_TYPE,
                "first",
                vec![
                    field(
                        FieldId::from_bytes([22; 16]),
                        "inserted",
                        0,
                        ResolvedType::Value(boolean),
                        true,
                    ),
                    field(
                        FIRST_FIELD,
                        "stored",
                        1,
                        ResolvedType::Value(boolean),
                        false,
                    ),
                ],
            )],
            standard.clone(),
            3,
        );
        assert_eq!(
            plan_physical_changes(&active, &inserted),
            expected,
            "an inserted value field before the prefix must stay closed"
        );

        // Two appended value fields stay closed.
        let two = candidate_version_two(
            &active,
            vec![object(
                FIRST_TYPE,
                "first",
                vec![
                    stored,
                    appended,
                    field(
                        FieldId::from_bytes([22; 16]),
                        "extra",
                        2,
                        ResolvedType::Value(boolean),
                        true,
                    ),
                ],
            )],
            standard.clone(),
            4,
        );
        assert_eq!(
            plan_physical_changes(&active, &two),
            expected,
            "two appended value fields must stay closed"
        );

        // Reordered value fields stay closed against their own two-field pair.
        let active_two = active_version_two(
            vec![object(
                FIRST_TYPE,
                "first",
                vec![
                    field(FIRST_FIELD, "first", 0, ResolvedType::Value(boolean), false),
                    field(
                        SECOND_FIELD,
                        "second",
                        1,
                        ResolvedType::Value(boolean),
                        true,
                    ),
                ],
            )],
            standard.clone(),
            5,
        );
        let reordered = candidate_version_two(
            &active_two,
            vec![object(
                FIRST_TYPE,
                "first",
                vec![
                    field(
                        SECOND_FIELD,
                        "second",
                        0,
                        ResolvedType::Value(boolean),
                        true,
                    ),
                    field(FIRST_FIELD, "first", 1, ResolvedType::Value(boolean), false),
                ],
            )],
            standard,
            6,
        );
        assert_eq!(
            plan_physical_changes(&active_two, &reordered),
            expected,
            "reordered value fields must stay closed"
        );
    }

    #[test]
    fn multiple_invalid_existing_object_changes_never_expose_a_partial_plan() {
        let boolean = TypeId::from_bytes([0xd1; 16]);
        let standard = verified_standard(vec![standard_value_type(
            boolean,
            "orna.kernel.value.boolean@1",
            ValueTypePersistence::Persistable,
        )]);
        let active = active_version_two(
            vec![
                object(
                    FIRST_TYPE,
                    "first",
                    vec![field(
                        FIRST_FIELD,
                        "stored",
                        0,
                        ResolvedType::Value(boolean),
                        false,
                    )],
                ),
                object(
                    SECOND_TYPE,
                    "second",
                    vec![field(
                        FIRST_FIELD,
                        "stored",
                        0,
                        ResolvedType::Value(boolean),
                        false,
                    )],
                ),
            ],
            standard.clone(),
            1,
        );
        let candidate = candidate_version_two(
            &active,
            vec![
                object(
                    FIRST_TYPE,
                    "first",
                    vec![
                        field(
                            FIRST_FIELD,
                            "stored",
                            0,
                            ResolvedType::Value(boolean),
                            false,
                        ),
                        field(
                            SECOND_FIELD,
                            "added",
                            1,
                            ResolvedType::Value(boolean),
                            false,
                        ),
                    ],
                ),
                object(
                    SECOND_TYPE,
                    "second",
                    vec![
                        field(
                            FieldId::from_bytes([22; 16]),
                            "inserted",
                            0,
                            ResolvedType::Value(boolean),
                            true,
                        ),
                        field(
                            FIRST_FIELD,
                            "stored",
                            1,
                            ResolvedType::Value(boolean),
                            false,
                        ),
                    ],
                ),
            ],
            standard,
            2,
        );

        assert!(
            matches!(
                plan_physical_changes(&active, &candidate),
                Err(PhysicalPlanError::UnsupportedExistingObjectChange { .. })
            ),
            "multiple invalid existing-object changes must expose no partial plan"
        );
    }

    #[test]
    fn legacy_resolved_scalars_do_not_admit_an_appended_field() {
        let active = active(
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "stored",
                    0,
                    ResolvedType::scalar(StandardScalar::Boolean),
                    false,
                )],
            )],
            1,
        );
        for (index, scalar) in [
            StandardScalar::Boolean,
            StandardScalar::Integer,
            StandardScalar::BigInt,
            StandardScalar::Float,
            StandardScalar::CharacterLargeObject,
            StandardScalar::BinaryLargeObject,
        ]
        .into_iter()
        .enumerate()
        {
            let candidate = candidate(
                &active,
                vec![object(
                    FIRST_TYPE,
                    "first",
                    vec![
                        field(
                            FIRST_FIELD,
                            "stored",
                            0,
                            ResolvedType::scalar(StandardScalar::Boolean),
                            false,
                        ),
                        field(SECOND_FIELD, "added", 1, ResolvedType::scalar(scalar), true),
                    ],
                )],
                10 + index as u8,
            );
            assert_eq!(
                plan_physical_changes(&active, &candidate),
                Err(PhysicalPlanError::UnsupportedExistingObjectChange {
                    object_type: FIRST_TYPE,
                }),
                "a legacy scalar must not admit an existing-object field append"
            );
        }
    }

    #[test]
    fn appended_value_fields_retain_closed_value_contracts() {
        let boolean = TypeId::from_bytes([0xd1; 16]);
        let closed = [
            (0xe1, "orna.kernel.value.decimal@1"),
            (0xe2, "orna.kernel.value.uuid@1"),
            (0xe3, "orna.kernel.value.date@1"),
            (0xe4, "orna.kernel.value.time@1"),
            (0xe5, "orna.kernel.value.timestamp@1"),
            (0xe6, "orna.kernel.value.duration@1"),
        ];
        let mut value_types = vec![standard_value_type(
            boolean,
            "orna.kernel.value.boolean@1",
            ValueTypePersistence::Persistable,
        )];
        value_types.extend(closed.iter().map(|(id, contract)| {
            standard_value_type(
                TypeId::from_bytes([*id; 16]),
                contract,
                ValueTypePersistence::Persistable,
            )
        }));
        let standard = verified_standard(value_types);
        let active = active_version_two(
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "stored",
                    0,
                    ResolvedType::Value(boolean),
                    false,
                )],
            )],
            standard.clone(),
            1,
        );
        for (index, (id, _)) in closed.into_iter().enumerate() {
            let candidate = candidate_version_two(
                &active,
                vec![object(
                    FIRST_TYPE,
                    "first",
                    vec![
                        field(
                            FIRST_FIELD,
                            "stored",
                            0,
                            ResolvedType::Value(boolean),
                            false,
                        ),
                        field(
                            SECOND_FIELD,
                            "added",
                            1,
                            ResolvedType::Value(TypeId::from_bytes([id; 16])),
                            true,
                        ),
                    ],
                )],
                standard.clone(),
                u8::try_from(index + 2).unwrap(),
            );
            assert_eq!(
                plan_physical_changes(&active, &candidate),
                Err(PhysicalPlanError::UnsupportedExistingObjectChange {
                    object_type: FIRST_TYPE,
                }),
                "a closed value contract must not admit an existing-object field append"
            );
        }
    }

    #[test]
    fn appended_value_fields_retain_unsupported_and_transient_contract_errors() {
        // The missing and opaque wrong-contract cases cannot reach planning.
        // The revision constructor rejects a field whose value type is absent
        // from the pinned standard, and a field that resolves to an opaque
        // value type. `ValueTypeMutability` exposes exactly one constructible
        // variant, `Immutable`, so a non-immutable definition cannot be built.
        // The projection-level `MissingValueTypeDefinition` and opaque-contract
        // errors remain covered by the direct projection tests above.
        let boolean = TypeId::from_bytes([0xd1; 16]);
        let standard = verified_standard(vec![
            standard_value_type(
                boolean,
                "orna.kernel.value.boolean@1",
                ValueTypePersistence::Persistable,
            ),
            standard_value_type(
                TypeId::from_bytes([0xe7; 16]),
                "orna.kernel.value.custom@1",
                ValueTypePersistence::Persistable,
            ),
            standard_value_type(
                TypeId::from_bytes([0xe8; 16]),
                "orna.kernel.value.boolean@1",
                ValueTypePersistence::Transient,
            ),
        ]);
        let active = active_version_two(
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "stored",
                    0,
                    ResolvedType::Value(boolean),
                    false,
                )],
            )],
            standard.clone(),
            1,
        );

        let cases = [
            (
                TypeId::from_bytes([0xe7; 16]),
                PhysicalPlanError::UnsupportedValueTypeContract {
                    object_type: FIRST_TYPE,
                    field: SECOND_FIELD,
                    value_type: TypeId::from_bytes([0xe7; 16]),
                    contract: "orna.kernel.value.custom@1".to_owned(),
                },
            ),
            (
                TypeId::from_bytes([0xe8; 16]),
                PhysicalPlanError::TransientValueType {
                    object_type: FIRST_TYPE,
                    field: SECOND_FIELD,
                    value_type: TypeId::from_bytes([0xe8; 16]),
                },
            ),
        ];
        for (index, (value_type, expected)) in cases.into_iter().enumerate() {
            let candidate = candidate_version_two(
                &active,
                vec![object(
                    FIRST_TYPE,
                    "first",
                    vec![
                        field(
                            FIRST_FIELD,
                            "stored",
                            0,
                            ResolvedType::Value(boolean),
                            false,
                        ),
                        field(
                            SECOND_FIELD,
                            "added",
                            1,
                            ResolvedType::Value(value_type),
                            true,
                        ),
                    ],
                )],
                standard.clone(),
                u8::try_from(index + 2).unwrap(),
            );
            assert_eq!(
                plan_physical_changes(&active, &candidate),
                Err(expected),
                "the appended field must retain its exact value-contract error"
            );
        }
    }

    #[test]
    fn appended_void_value_field_keeps_the_void_projection_error() {
        let boolean = TypeId::from_bytes([0xd1; 16]);
        let void = TypeId::from_bytes([0xeb; 16]);
        let standard = verified_standard(vec![
            standard_value_type(
                boolean,
                "orna.kernel.value.boolean@1",
                ValueTypePersistence::Persistable,
            ),
            standard_value_type(
                void,
                "orna.kernel.value.void@1",
                ValueTypePersistence::Persistable,
            ),
        ]);
        let active = active_version_two(
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "stored",
                    0,
                    ResolvedType::Value(boolean),
                    false,
                )],
            )],
            standard.clone(),
            1,
        );
        let candidate = candidate_version_two(
            &active,
            vec![object(
                FIRST_TYPE,
                "first",
                vec![
                    field(
                        FIRST_FIELD,
                        "stored",
                        0,
                        ResolvedType::Value(boolean),
                        false,
                    ),
                    field(SECOND_FIELD, "added", 1, ResolvedType::Value(void), true),
                ],
            )],
            standard,
            2,
        );

        assert_eq!(
            plan_physical_changes(&active, &candidate),
            Err(PhysicalPlanError::UnsupportedVoidField {
                object_type: FIRST_TYPE,
                field: SECOND_FIELD,
            }),
            "an appended VOID field must keep the void projection error"
        );
    }

    #[test]
    fn value_appended_field_projection_errors_precede_admission_rejection() {
        let boolean = TypeId::from_bytes([0xd1; 16]);
        let standard = verified_standard(vec![standard_value_type(
            boolean,
            "orna.kernel.value.boolean@1",
            ValueTypePersistence::Persistable,
        )]);
        let active = active_version_two(
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "stored",
                    0,
                    ResolvedType::Value(boolean),
                    false,
                )],
            )],
            standard.clone(),
            1,
        );
        let stored = field(
            FIRST_FIELD,
            "stored",
            0,
            ResolvedType::Value(boolean),
            false,
        );

        let defaulted = candidate_version_two(
            &active,
            vec![object(
                FIRST_TYPE,
                "first",
                vec![
                    stored.clone(),
                    field_with_options(
                        SECOND_FIELD,
                        "added",
                        1,
                        ResolvedType::Value(boolean),
                        true,
                        false,
                        Some(ExpressionId::from_bytes([0x51; 16])),
                        None,
                    ),
                ],
            )],
            standard.clone(),
            2,
        );
        assert_eq!(
            plan_physical_changes(&active, &defaulted),
            Err(PhysicalPlanError::UnsupportedFieldDefault {
                object_type: FIRST_TYPE,
                field: SECOND_FIELD,
            }),
            "a defaulted appended value field must keep the default projection error"
        );

        let unique = candidate_version_two(
            &active,
            vec![object(
                FIRST_TYPE,
                "first",
                vec![
                    stored.clone(),
                    field_with_options(
                        SECOND_FIELD,
                        "added",
                        1,
                        ResolvedType::Value(boolean),
                        true,
                        true,
                        None,
                        None,
                    ),
                ],
            )],
            standard.clone(),
            3,
        );
        assert_eq!(
            plan_physical_changes(&active, &unique),
            Err(PhysicalPlanError::UnsupportedUniqueField {
                object_type: FIRST_TYPE,
                field: SECOND_FIELD,
            }),
            "a unique appended value field must keep the unique projection error"
        );

        let delete = candidate_version_two(
            &active,
            vec![object(
                FIRST_TYPE,
                "first",
                vec![
                    stored,
                    field_with_options(
                        SECOND_FIELD,
                        "added",
                        1,
                        ResolvedType::Value(boolean),
                        true,
                        false,
                        None,
                        Some(OnDeleteAction::Restrict),
                    ),
                ],
            )],
            standard,
            4,
        );
        assert_eq!(
            plan_physical_changes(&active, &delete),
            Err(PhysicalPlanError::InvalidDeleteAction {
                object_type: FIRST_TYPE,
                field: SECOND_FIELD,
            }),
            "an appended value field with a delete action must keep the delete projection error"
        );
    }

    #[test]
    fn replayed_value_pairs_plan_no_field_operation() {
        let boolean = TypeId::from_bytes([0xd1; 16]);
        let standard = verified_standard(vec![standard_value_type(
            boolean,
            "orna.kernel.value.boolean@1",
            ValueTypePersistence::Persistable,
        )]);
        let fields = vec![
            field(
                FIRST_FIELD,
                "stored",
                0,
                ResolvedType::Value(boolean),
                false,
            ),
            field(SECOND_FIELD, "added", 1, ResolvedType::Value(boolean), true),
        ];
        let active = active_version_two(
            vec![object(FIRST_TYPE, "first", fields.clone())],
            standard.clone(),
            1,
        );
        let candidate = candidate_version_two(
            &active,
            vec![object(FIRST_TYPE, "first", fields)],
            standard,
            2,
        );

        let plan = plan_physical_changes(&active, &candidate).unwrap();
        assert!(
            plan.create_objects().is_empty(),
            "an exact value replay must not plan a new object"
        );
        assert!(
            plan.add_field().is_none(),
            "an exact value replay must not plan a field addition"
        );
    }

    #[test]
    fn projects_a_verified_value_contract_from_a_candidate() {
        let value_type = TypeId::from_bytes([0xa1; 16]);
        let standard = verified_standard(vec![standard_value_type(
            value_type,
            "orna.kernel.value.boolean@1",
            ValueTypePersistence::Persistable,
        )]);
        let active = active_version_two(Vec::new(), standard.clone(), 1);
        let candidate = candidate_version_two(
            &active,
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "value",
                    0,
                    ResolvedType::Value(value_type),
                    false,
                )],
            )],
            standard,
            2,
        );

        assert_eq!(
            plan_physical_changes(&active, &candidate)
                .unwrap()
                .create_objects(),
            [CreateObject {
                type_id: FIRST_TYPE,
                fields: vec![CreateField {
                    field_id: FIRST_FIELD,
                    field_type: PhysicalFieldType::Scalar(StandardScalar::Boolean),
                    nullable: false,
                    unique: false,
                }],
            }]
        );
    }

    #[test]
    fn projects_catalogue_enums_as_named_physical_fields() {
        let enum_type = TypeId::from_bytes([0xa2; 16]);
        let standard = verified_standard(Vec::new());
        let active = active_version_two(Vec::new(), standard.clone(), 1);
        let source = source(2, Some(active.pair().source()));
        let catalogue = CatalogueSnapshot::new_with_enum_types(
            CatalogueRevisionId::from_bytes([3; 16]),
            vec![schema()],
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "stage",
                    0,
                    ResolvedType::named(enum_type),
                    false,
                )],
            )],
            vec![],
            vec![EnumTypeDefinition::new(
                enum_type,
                name(&["demo", "stage"]),
                ["lead", "qualified"],
            )],
            vec![],
        )
        .unwrap();
        let origins = origins(&source, &catalogue);
        let candidate = DeployableRevision::new_with_catalogue_hash_context(
            DeployableRevisionInput::new(
                active.pair(),
                source,
                active.pair().catalogue(),
                catalogue,
                digest(2),
                DeployableRevisionContent::new(origins, Vec::new(), Vec::new(), Vec::new())
                    .with_current_function_revisions(Vec::new()),
            ),
            CatalogueHashContext::version_two(standard),
        )
        .unwrap();

        assert_eq!(
            plan_physical_changes(&active, &candidate)
                .unwrap()
                .create_objects(),
            [CreateObject {
                type_id: FIRST_TYPE,
                fields: vec![CreateField {
                    field_id: FIRST_FIELD,
                    field_type: PhysicalFieldType::Enum(enum_type),
                    nullable: false,
                    unique: false,
                }],
            }]
        );
    }

    #[test]
    fn projects_record_values_as_canonical_byte_fields() {
        let record_type = TypeId::from_bytes([0xa3; 16]);
        let standard = verified_standard(vec![standard_value_type(
            TypeId::from_bytes([0xa4; 16]),
            "orna.kernel.value.boolean@1",
            ValueTypePersistence::Persistable,
        )]);
        let active = active_version_two(Vec::new(), standard.clone(), 1);
        let source = source(2, Some(active.pair().source()));
        let catalogue = CatalogueSnapshot::new_with_functions_and_record_value_types(
            CatalogueRevisionId::from_bytes([3; 16]),
            vec![schema()],
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "status",
                    0,
                    ResolvedType::named(record_type),
                    false,
                )],
            )],
            vec![],
            vec![],
            vec![RecordValueTypeDefinition::new(
                record_type,
                name(&["demo", "status"]),
                vec![
                    RecordValueFieldDefinition::try_new_descriptor(
                        SECOND_FIELD,
                        "active",
                        0,
                        TypeDescriptor::named(TypeId::from_bytes([0xa4; 16])),
                    )
                    .unwrap(),
                ],
            )],
            vec![],
            vec![],
        )
        .unwrap();
        let origins = origins(&source, &catalogue);
        let candidate = DeployableRevision::new_with_catalogue_hash_context(
            DeployableRevisionInput::new(
                active.pair(),
                source,
                active.pair().catalogue(),
                catalogue,
                digest(2),
                DeployableRevisionContent::new(origins, Vec::new(), Vec::new(), Vec::new())
                    .with_current_function_revisions(Vec::new()),
            ),
            CatalogueHashContext::version_two(standard),
        )
        .unwrap();

        assert_eq!(
            plan_physical_changes(&active, &candidate)
                .unwrap()
                .create_objects()[0]
                .fields()[0]
                .field_type(),
            PhysicalFieldType::Record(record_type)
        );
        let nullable = field(
            FieldId::from_bytes([22; 16]),
            "optional_status",
            0,
            ResolvedType::named(record_type),
            true,
        );
        assert_eq!(
            project_physical_field(
                PhysicalRevision::Deployable(&candidate),
                FIRST_TYPE,
                &nullable,
            ),
            Err(PhysicalPlanError::UnsupportedNullableRecordField {
                object_type: FIRST_TYPE,
                field: nullable.id(),
            })
        );
    }

    #[test]
    fn projects_every_pinned_kernel_value_contract() {
        let contracts = [
            (0xb0, "orna.kernel.value.boolean@1", StandardScalar::Boolean),
            (0xb1, "orna.kernel.value.integer@1", StandardScalar::Integer),
            (0xb2, "orna.kernel.value.bigint@1", StandardScalar::BigInt),
            (0xb3, "orna.kernel.value.float@1", StandardScalar::Float),
            (0xb4, "orna.kernel.value.decimal@1", StandardScalar::Decimal),
            (
                0xb5,
                "orna.kernel.value.character-large-object@1",
                StandardScalar::CharacterLargeObject,
            ),
            (
                0xb6,
                "orna.kernel.value.binary-large-object@1",
                StandardScalar::BinaryLargeObject,
            ),
            (0xb7, "orna.kernel.value.uuid@1", StandardScalar::Uuid),
            (0xb8, "orna.kernel.value.date@1", StandardScalar::Date),
            (0xb9, "orna.kernel.value.time@1", StandardScalar::Time),
            (
                0xba,
                "orna.kernel.value.timestamp@1",
                StandardScalar::Timestamp,
            ),
            (
                0xbb,
                "orna.kernel.value.duration@1",
                StandardScalar::Duration,
            ),
            (0xbc, "orna.kernel.value.void@1", StandardScalar::Void),
        ];
        let standard = verified_standard(
            contracts
                .iter()
                .map(|(id, contract, _)| {
                    standard_value_type(
                        TypeId::from_bytes([*id; 16]),
                        contract,
                        ValueTypePersistence::Persistable,
                    )
                })
                .collect(),
        );
        let active = active_version_two(Vec::new(), standard.clone(), 1);

        for (index, (id, _, scalar)) in contracts.into_iter().enumerate() {
            let value_type = TypeId::from_bytes([id; 16]);
            let object_type = TypeId::from_bytes([id.wrapping_add(0x20); 16]);
            let candidate = candidate_version_two(
                &active,
                vec![object(
                    object_type,
                    "value_holder",
                    vec![field(
                        FIRST_FIELD,
                        "value",
                        0,
                        ResolvedType::Value(value_type),
                        false,
                    )],
                )],
                standard.clone(),
                u8::try_from(index + 2).unwrap(),
            );

            let expected = if scalar == StandardScalar::Void {
                Err(PhysicalPlanError::UnsupportedVoidField {
                    object_type,
                    field: FIRST_FIELD,
                })
            } else {
                Ok(PhysicalPlan {
                    create_objects: vec![CreateObject {
                        type_id: object_type,
                        fields: vec![CreateField {
                            field_id: FIRST_FIELD,
                            field_type: PhysicalFieldType::Scalar(scalar),
                            nullable: false,
                            unique: false,
                        }],
                    }],
                    add_field: None,
                })
            };
            assert_eq!(plan_physical_changes(&active, &candidate), expected);
        }
    }

    #[test]
    fn active_value_catalogue_and_legacy_scalar_candidate_storage_are_equal() {
        let value_type = TypeId::from_bytes([0xc1; 16]);
        let standard = verified_standard(vec![standard_value_type(
            value_type,
            "orna.kernel.value.integer@1",
            ValueTypePersistence::Persistable,
        )]);
        let value_active = active_version_two(
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "value",
                    0,
                    ResolvedType::Value(value_type),
                    false,
                )],
            )],
            standard.clone(),
            1,
        );

        assert_eq!(
            active_physical_catalogue(&value_active).unwrap().objects(),
            [CreateObject {
                type_id: FIRST_TYPE,
                fields: vec![CreateField {
                    field_id: FIRST_FIELD,
                    field_type: PhysicalFieldType::Scalar(StandardScalar::Integer),
                    nullable: false,
                    unique: false,
                }],
            }]
        );

        let legacy_active = active(
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "value",
                    0,
                    ResolvedType::scalar(StandardScalar::Integer),
                    false,
                )],
            )],
            3,
        );
        let value_candidate = candidate_version_two(
            &legacy_active,
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "value",
                    0,
                    ResolvedType::Value(value_type),
                    false,
                )],
            )],
            standard,
            4,
        );

        assert_eq!(
            plan_physical_changes(&legacy_active, &value_candidate),
            Ok(PhysicalPlan {
                create_objects: Vec::new(),
                add_field: None,
            })
        );
    }

    #[test]
    fn active_physical_catalogue_rejects_hostile_fields_through_the_shared_projector() {
        let active = active(
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "value",
                    0,
                    ResolvedType::Named(TypeId::from_bytes([0xcf; 16])),
                    false,
                )],
            )],
            1,
        );

        assert_eq!(
            active_physical_catalogue(&active),
            Err(PhysicalPlanError::UnsupportedNamedFieldType {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
            })
        );
    }

    #[test]
    fn value_contract_errors_retain_identity_contract_and_no_source() {
        let value_type = TypeId::from_bytes([0xc2; 16]);
        let missing =
            project_value_type_definition(None, FIRST_TYPE, FIRST_FIELD, value_type).unwrap_err();
        assert_eq!(
            missing,
            PhysicalPlanError::MissingValueTypeDefinition {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
                value_type,
            }
        );
        assert_eq!(
            missing.to_string(),
            "physical value type is absent from the pinned standard library"
        );
        assert!(std::error::Error::source(&missing).is_none());

        let unsupported = project_value_type_definition(
            Some(&standard_value_type(
                value_type,
                "orna.kernel.value.custom@1",
                ValueTypePersistence::Persistable,
            )),
            FIRST_TYPE,
            FIRST_FIELD,
            value_type,
        )
        .unwrap_err();
        assert_eq!(
            unsupported,
            PhysicalPlanError::UnsupportedValueTypeContract {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
                value_type,
                contract: "orna.kernel.value.custom@1".to_owned(),
            }
        );
        assert_eq!(
            unsupported.to_string(),
            "physical value type contract is not supported"
        );
        assert!(std::error::Error::source(&unsupported).is_none());
    }

    #[test]
    fn value_field_gates_preserve_transient_void_and_delete_precedence() {
        let transient_boolean = TypeId::from_bytes([0xc3; 16]);
        let transient_void = TypeId::from_bytes([0xc4; 16]);
        let unknown_contract = TypeId::from_bytes([0xc5; 16]);
        let persistable_boolean = TypeId::from_bytes([0xc6; 16]);
        let standard = verified_standard(vec![
            standard_value_type(
                transient_boolean,
                "orna.kernel.value.boolean@1",
                ValueTypePersistence::Transient,
            ),
            standard_value_type(
                transient_void,
                "orna.kernel.value.void@1",
                ValueTypePersistence::Transient,
            ),
            standard_value_type(
                unknown_contract,
                "orna.kernel.value.custom@1",
                ValueTypePersistence::Persistable,
            ),
            standard_value_type(
                persistable_boolean,
                "orna.kernel.value.boolean@1",
                ValueTypePersistence::Persistable,
            ),
        ]);
        let active = active_version_two(Vec::new(), standard.clone(), 1);

        let transient = candidate_version_two(
            &active,
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "value",
                    0,
                    ResolvedType::Value(transient_boolean),
                    false,
                )],
            )],
            standard.clone(),
            2,
        );
        let transient_error = plan_physical_changes(&active, &transient).unwrap_err();
        assert_eq!(
            transient_error,
            PhysicalPlanError::TransientValueType {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
                value_type: transient_boolean,
            }
        );
        assert_eq!(
            transient_error.to_string(),
            "transient value types cannot be stored"
        );
        assert!(std::error::Error::source(&transient_error).is_none());

        let void = candidate_version_two(
            &active,
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "value",
                    0,
                    ResolvedType::Value(transient_void),
                    false,
                )],
            )],
            standard.clone(),
            3,
        );
        assert_eq!(
            plan_physical_changes(&active, &void),
            Err(PhysicalPlanError::UnsupportedVoidField {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
            })
        );

        let unique_before_contract = candidate_version_two(
            &active,
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field_with_options(
                    FIRST_FIELD,
                    "value",
                    0,
                    ResolvedType::Value(unknown_contract),
                    false,
                    true,
                    None,
                    None,
                )],
            )],
            standard.clone(),
            4,
        );
        assert_eq!(
            plan_physical_changes(&active, &unique_before_contract),
            Err(PhysicalPlanError::UnsupportedUniqueField {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
            })
        );

        let default_before_contract = candidate_version_two(
            &active,
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field_with_options(
                    FIRST_FIELD,
                    "value",
                    0,
                    ResolvedType::Value(unknown_contract),
                    false,
                    false,
                    Some(ExpressionId::from_bytes([0xc7; 16])),
                    None,
                )],
            )],
            standard.clone(),
            5,
        );
        assert_eq!(
            plan_physical_changes(&active, &default_before_contract),
            Err(PhysicalPlanError::UnsupportedFieldDefault {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
            })
        );

        let unsupported_contract = candidate_version_two(
            &active,
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "value",
                    0,
                    ResolvedType::Value(unknown_contract),
                    false,
                )],
            )],
            standard.clone(),
            6,
        );
        assert_eq!(
            plan_physical_changes(&active, &unsupported_contract),
            Err(PhysicalPlanError::UnsupportedValueTypeContract {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
                value_type: unknown_contract,
                contract: "orna.kernel.value.custom@1".to_owned(),
            })
        );

        let active_unsupported_contract = active_version_two(
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "value",
                    0,
                    ResolvedType::Value(unknown_contract),
                    false,
                )],
            )],
            standard.clone(),
            7,
        );
        assert_eq!(
            active_physical_catalogue(&active_unsupported_contract),
            Err(PhysicalPlanError::UnsupportedValueTypeContract {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
                value_type: unknown_contract,
                contract: "orna.kernel.value.custom@1".to_owned(),
            })
        );

        let delete_after_contract = candidate_version_two(
            &active,
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field_with_options(
                    FIRST_FIELD,
                    "value",
                    0,
                    ResolvedType::Value(persistable_boolean),
                    false,
                    false,
                    None,
                    Some(OnDeleteAction::Restrict),
                )],
            )],
            standard,
            8,
        );
        assert_eq!(
            plan_physical_changes(&active, &delete_after_contract),
            Err(PhysicalPlanError::InvalidDeleteAction {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
            })
        );
    }

    #[test]
    fn planner_checks_base_drops_existing_projections_and_new_objects_in_order() {
        let existing = object(
            FIRST_TYPE,
            "first",
            vec![field(
                FIRST_FIELD,
                "value",
                0,
                ResolvedType::scalar(StandardScalar::Integer),
                false,
            )],
        );
        let physical_active = active(vec![existing.clone()], 1);
        let stale_candidate = candidate(&physical_active, Vec::new(), 2);
        let other_active = active(Vec::new(), 3);
        assert!(matches!(
            plan_physical_changes(&other_active, &stale_candidate),
            Err(PhysicalPlanError::ExpectedBaseMismatch { .. })
        ));

        let invalid_existing = object(
            FIRST_TYPE,
            "first",
            vec![field(
                FIRST_FIELD,
                "value",
                0,
                ResolvedType::Named(TypeId::from_bytes([0xc8; 16])),
                false,
            )],
        );
        let second = object(SECOND_TYPE, "second", Vec::new());
        let invalid_active = active(vec![invalid_existing.clone(), second], 4);
        assert_eq!(
            active_physical_catalogue(&invalid_active),
            Err(PhysicalPlanError::UnsupportedNamedFieldType {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
            })
        );
        let dropped_before_projection = candidate(
            &invalid_active,
            vec![
                invalid_existing.clone(),
                object(
                    TypeId::from_bytes([0xc9; 16]),
                    "new",
                    vec![field(
                        FIRST_FIELD,
                        "value",
                        0,
                        ResolvedType::Named(TypeId::from_bytes([0xca; 16])),
                        false,
                    )],
                ),
            ],
            5,
        );
        assert_eq!(
            plan_physical_changes(&invalid_active, &dropped_before_projection),
            Err(PhysicalPlanError::UnsupportedObjectDrop {
                object_type: SECOND_TYPE,
            })
        );

        let invalid_survivor = candidate(
            &physical_active,
            vec![
                invalid_existing,
                object(
                    TypeId::from_bytes([0xcb; 16]),
                    "new",
                    vec![field(
                        FIRST_FIELD,
                        "value",
                        0,
                        ResolvedType::Named(TypeId::from_bytes([0xcc; 16])),
                        false,
                    )],
                ),
            ],
            6,
        );
        assert_eq!(
            plan_physical_changes(&physical_active, &invalid_survivor),
            Err(PhysicalPlanError::UnsupportedNamedFieldType {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
            })
        );

        let changed_before_new = candidate(
            &physical_active,
            vec![
                object(
                    FIRST_TYPE,
                    "first",
                    vec![field(
                        FIRST_FIELD,
                        "value",
                        0,
                        ResolvedType::scalar(StandardScalar::BigInt),
                        false,
                    )],
                ),
                object(
                    TypeId::from_bytes([0xcd; 16]),
                    "new",
                    vec![field(
                        FIRST_FIELD,
                        "value",
                        0,
                        ResolvedType::Named(TypeId::from_bytes([0xce; 16])),
                        false,
                    )],
                ),
            ],
            7,
        );
        assert_eq!(
            plan_physical_changes(&physical_active, &changed_before_new),
            Err(PhysicalPlanError::UnsupportedExistingObjectChange {
                object_type: FIRST_TYPE,
            })
        );

        let invalid_new_after_equal = candidate(
            &physical_active,
            vec![
                existing,
                object(
                    TypeId::from_bytes([0xcf; 16]),
                    "new",
                    vec![field(
                        FIRST_FIELD,
                        "value",
                        0,
                        ResolvedType::Named(TypeId::from_bytes([0xd0; 16])),
                        false,
                    )],
                ),
            ],
            8,
        );
        assert_eq!(
            plan_physical_changes(&physical_active, &invalid_new_after_equal),
            Err(PhysicalPlanError::UnsupportedNamedFieldType {
                object_type: TypeId::from_bytes([0xcf; 16]),
                field: FIRST_FIELD,
            })
        );
    }

    #[test]
    fn plans_mutual_references_in_candidate_and_field_order() {
        let first = object(
            FIRST_TYPE,
            "first",
            vec![reference_field(
                FIRST_FIELD,
                "second",
                0,
                SECOND_TYPE,
                false,
                Some(OnDeleteAction::Restrict),
            )],
        );
        let second = object(
            SECOND_TYPE,
            "second",
            vec![reference_field(
                SECOND_FIELD,
                "first",
                0,
                FIRST_TYPE,
                true,
                Some(OnDeleteAction::SetNull),
            )],
        );
        let active = active(Vec::new(), 1);
        let candidate = candidate(&active, vec![second, first], 2);

        let plan = plan_physical_changes(&active, &candidate).unwrap();

        assert_eq!(
            plan.create_objects()
                .iter()
                .map(CreateObject::type_id)
                .collect::<Vec<_>>(),
            vec![SECOND_TYPE, FIRST_TYPE]
        );
        assert_eq!(
            plan.create_objects()[0].fields(),
            [CreateField {
                field_id: SECOND_FIELD,
                field_type: PhysicalFieldType::Reference {
                    target: FIRST_TYPE,
                    on_delete: Some(OnDeleteAction::SetNull),
                },
                nullable: true,
                unique: false,
            }]
        );
        assert_eq!(
            plan.create_objects()[1].fields()[0].field_type(),
            PhysicalFieldType::Reference {
                target: SECOND_TYPE,
                on_delete: Some(OnDeleteAction::Restrict),
            }
        );
    }

    #[test]
    fn plans_new_unique_text_fields_from_exact_scalar_and_value_authority() {
        let scalar_nullable = object(
            FIRST_TYPE,
            "scalar_nullable",
            vec![field_with_options(
                FIRST_FIELD,
                "email",
                0,
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                true,
                true,
                None,
                None,
            )],
        );
        let scalar_required = object(
            SECOND_TYPE,
            "scalar_required",
            vec![field_with_options(
                SECOND_FIELD,
                "name",
                0,
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                false,
                true,
                None,
                None,
            )],
        );
        let empty = active(Vec::new(), 1);
        let scalar_candidate = candidate(
            &empty,
            vec![scalar_nullable.clone(), scalar_required.clone()],
            2,
        );
        let scalar_plan = plan_physical_changes(&empty, &scalar_candidate)
            .expect("version-one scalar Text UNIQUE fields must be physical");
        assert_eq!(
            scalar_plan
                .create_objects()
                .iter()
                .map(CreateObject::type_id)
                .collect::<Vec<_>>(),
            vec![FIRST_TYPE, SECOND_TYPE]
        );
        for (field, nullable) in scalar_plan
            .create_objects()
            .iter()
            .map(|object| &object.fields()[0])
            .zip([true, false])
        {
            assert_eq!(
                field.field_type(),
                PhysicalFieldType::Scalar(StandardScalar::CharacterLargeObject)
            );
            assert_eq!(field.nullable(), nullable);
            assert!(field.unique());
        }

        let text = TypeId::from_bytes([0xd9; 16]);
        let standard = verified_standard(vec![standard_value_type(
            text,
            "orna.kernel.value.character-large-object@1",
            ValueTypePersistence::Persistable,
        )]);
        let value_nullable = object(
            FIRST_TYPE,
            "value_nullable",
            vec![field_with_options(
                FIRST_FIELD,
                "email",
                0,
                ResolvedType::Value(text),
                true,
                true,
                None,
                None,
            )],
        );
        let value_required = object(
            SECOND_TYPE,
            "value_required",
            vec![field_with_options(
                SECOND_FIELD,
                "name",
                0,
                ResolvedType::Value(text),
                false,
                true,
                None,
                None,
            )],
        );
        let value_empty = active_version_two(Vec::new(), standard.clone(), 3);
        let value_candidate = candidate_version_two(
            &value_empty,
            vec![value_nullable, value_required],
            standard,
            4,
        );
        let value_plan = plan_physical_changes(&value_empty, &value_candidate)
            .expect("verified version-two Text UNIQUE fields must be physical");
        assert_eq!(
            value_plan
                .create_objects()
                .iter()
                .map(|object| {
                    let field = &object.fields()[0];
                    (
                        object.type_id(),
                        field.field_type(),
                        field.nullable(),
                        field.unique(),
                    )
                })
                .collect::<Vec<_>>(),
            vec![
                (
                    FIRST_TYPE,
                    PhysicalFieldType::Scalar(StandardScalar::CharacterLargeObject),
                    true,
                    true,
                ),
                (
                    SECOND_TYPE,
                    PhysicalFieldType::Scalar(StandardScalar::CharacterLargeObject),
                    false,
                    true,
                ),
            ]
        );
    }

    #[test]
    fn unique_text_replay_and_semantic_rename_keep_storage_unchanged() {
        let installed = object(
            FIRST_TYPE,
            "person",
            vec![field_with_options(
                FIRST_FIELD,
                "email",
                0,
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                true,
                true,
                None,
                None,
            )],
        );
        let active = active(vec![installed.clone()], 1);
        let replay = candidate(&active, vec![installed], 2);
        assert_eq!(
            plan_physical_changes(&active, &replay),
            Ok(PhysicalPlan {
                create_objects: Vec::new(),
                add_field: None,
            })
        );
        let renamed = candidate(
            &active,
            vec![object(
                FIRST_TYPE,
                "renamed_person",
                vec![field_with_options(
                    FIRST_FIELD,
                    "renamed_email",
                    0,
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    true,
                    true,
                    None,
                    None,
                )],
            )],
            3,
        );
        assert_eq!(
            plan_physical_changes(&active, &renamed),
            Ok(PhysicalPlan {
                create_objects: Vec::new(),
                add_field: None,
            })
        );
    }

    #[test]
    fn unique_text_keeps_required_unique_references_admitted() {
        let owner = object(
            FIRST_TYPE,
            "owner",
            vec![field_with_options(
                FIRST_FIELD,
                "target",
                0,
                ResolvedType::reference(SECOND_TYPE),
                false,
                true,
                None,
                Some(OnDeleteAction::Restrict),
            )],
        );
        let target = object(SECOND_TYPE, "target", Vec::new());
        let empty = active(Vec::new(), 1);
        let candidate = candidate(&empty, vec![owner, target], 2);

        let plan = plan_physical_changes(&empty, &candidate)
            .expect("ADR 0051 must retain the required unique Reference form");
        assert_eq!(
            plan.create_objects()[0].fields(),
            [CreateField {
                field_id: FIRST_FIELD,
                field_type: PhysicalFieldType::Reference {
                    target: SECOND_TYPE,
                    on_delete: Some(OnDeleteAction::Restrict),
                },
                nullable: false,
                unique: true,
            }]
        );
    }

    #[test]
    fn unique_text_keeps_other_unique_shapes_and_existing_changes_closed() {
        let empty = active(Vec::new(), 1);
        for (resolved_type, seed) in [
            (ResolvedType::scalar(StandardScalar::Integer), 1),
            (ResolvedType::Named(SECOND_TYPE), 2),
        ] {
            let candidate = candidate(
                &empty,
                vec![object(
                    FIRST_TYPE,
                    "closed",
                    vec![field_with_options(
                        FIRST_FIELD,
                        "value",
                        0,
                        resolved_type,
                        true,
                        true,
                        None,
                        None,
                    )],
                )],
                10 + seed,
            );
            assert_eq!(
                plan_physical_changes(&empty, &candidate),
                Err(PhysicalPlanError::UnsupportedUniqueField {
                    object_type: FIRST_TYPE,
                    field: FIRST_FIELD,
                })
            );
        }

        let text = TypeId::from_bytes([0xda; 16]);
        let other = TypeId::from_bytes([0xdb; 16]);
        let standard = verified_standard(vec![
            standard_value_type(
                text,
                "orna.kernel.value.character-large-object@1",
                ValueTypePersistence::Persistable,
            ),
            standard_value_type(
                other,
                "orna.kernel.value.integer@1",
                ValueTypePersistence::Persistable,
            ),
        ]);
        let value_empty = active_version_two(Vec::new(), standard.clone(), 20);
        let other_value = candidate_version_two(
            &value_empty,
            vec![object(
                FIRST_TYPE,
                "closed_value",
                vec![field_with_options(
                    FIRST_FIELD,
                    "value",
                    0,
                    ResolvedType::Value(other),
                    true,
                    true,
                    None,
                    None,
                )],
            )],
            standard.clone(),
            21,
        );
        assert_eq!(
            plan_physical_changes(&value_empty, &other_value),
            Err(PhysicalPlanError::UnsupportedUniqueField {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
            })
        );

        let base = object(
            FIRST_TYPE,
            "person",
            vec![field_with_options(
                FIRST_FIELD,
                "email",
                0,
                ResolvedType::Value(text),
                false,
                true,
                None,
                None,
            )],
        );
        let active = active_version_two(vec![base.clone()], standard.clone(), 22);
        for (candidate_object, seed) in [
            (
                object(
                    FIRST_TYPE,
                    "person",
                    vec![field_with_options(
                        FIRST_FIELD,
                        "email",
                        0,
                        ResolvedType::Value(text),
                        false,
                        false,
                        None,
                        None,
                    )],
                ),
                23,
            ),
            (
                object(
                    FIRST_TYPE,
                    "person",
                    vec![
                        base.fields()[0].clone(),
                        field_with_options(
                            SECOND_FIELD,
                            "other_email",
                            1,
                            ResolvedType::Value(text),
                            true,
                            true,
                            None,
                            None,
                        ),
                    ],
                ),
                24,
            ),
        ] {
            let candidate =
                candidate_version_two(&active, vec![candidate_object], standard.clone(), seed);
            assert_eq!(
                plan_physical_changes(&active, &candidate),
                Err(PhysicalPlanError::UnsupportedExistingObjectChange {
                    object_type: FIRST_TYPE,
                })
            );
        }
    }

    #[test]
    fn version_two_unique_text_requires_verified_value_not_legacy_or_impostor_contracts() {
        let text = TypeId::from_bytes([0xdc; 16]);
        let transient_text = TypeId::from_bytes([0xdd; 16]);
        let other_text_contract = TypeId::from_bytes([0xde; 16]);
        let standard = verified_standard(vec![
            standard_value_type(
                text,
                "orna.kernel.value.character-large-object@1",
                ValueTypePersistence::Persistable,
            ),
            standard_value_type(
                transient_text,
                "orna.kernel.value.character-large-object@1",
                ValueTypePersistence::Transient,
            ),
            standard_value_type(
                other_text_contract,
                "orna.kernel.value.character-large-object@2",
                ValueTypePersistence::Persistable,
            ),
        ]);
        let legacy = field_with_options(
            FIRST_FIELD,
            "email",
            0,
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            true,
            true,
            None,
            None,
        );
        // A version-two revision cannot retain a legacy scalar field. This
        // constructor boundary closes the hostile input before physical
        // planning; therefore it cannot reach the Text UNIQUE branch.
        let legacy_source = source(1, None);
        let legacy_catalogue = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([2; 16]),
            vec![schema()],
            vec![object(FIRST_TYPE, "person", vec![legacy])],
        )
        .unwrap();
        let legacy_origins = origins(&legacy_source, &legacy_catalogue);
        let legacy = ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                RevisionPair::new(legacy_source.id(), legacy_catalogue.revision()),
                legacy_source,
                legacy_catalogue,
                digest(1),
                ActiveRevisionContent::new(Vec::new(), Vec::new(), legacy_origins, Vec::new()),
            ),
            CatalogueHashContext::version_two(standard.clone()),
        );
        assert!(
            legacy.is_err(),
            "version-two legacy Scalar(Text) must be closed before physical planning"
        );

        let empty = active_version_two(Vec::new(), standard.clone(), 3);
        for (value_type, seed) in [(transient_text, 4), (other_text_contract, 5)] {
            let candidate = candidate_version_two(
                &empty,
                vec![object(
                    FIRST_TYPE,
                    "person",
                    vec![field_with_options(
                        FIRST_FIELD,
                        "email",
                        0,
                        ResolvedType::Value(value_type),
                        true,
                        true,
                        None,
                        None,
                    )],
                )],
                standard.clone(),
                seed,
            );
            assert_eq!(
                plan_physical_changes(&empty, &candidate),
                Err(PhysicalPlanError::UnsupportedUniqueField {
                    object_type: FIRST_TYPE,
                    field: FIRST_FIELD,
                }),
                "only the persistable exact version-two Text contract may be unique"
            );
        }
    }

    #[test]
    fn plans_required_unique_references_against_the_complete_candidate() {
        let owner = object(
            FIRST_TYPE,
            "owner",
            vec![field_with_options(
                FIRST_FIELD,
                "target",
                0,
                ResolvedType::reference(SECOND_TYPE),
                false,
                true,
                None,
                Some(OnDeleteAction::Restrict),
            )],
        );
        let target = object(
            SECOND_TYPE,
            "target",
            vec![reference_field(
                SECOND_FIELD,
                "owner",
                0,
                FIRST_TYPE,
                true,
                None,
            )],
        );
        let active = active(Vec::new(), 1);
        let candidate = candidate(&active, vec![owner, target], 2);

        let plan = plan_physical_changes(&active, &candidate).unwrap();
        let unique = &plan.create_objects()[0].fields()[0];
        assert_eq!(unique.field_id(), FIRST_FIELD);
        assert_eq!(
            unique.field_type(),
            PhysicalFieldType::Reference {
                target: SECOND_TYPE,
                on_delete: Some(OnDeleteAction::Restrict),
            }
        );
        assert!(!unique.nullable());
        assert!(unique.unique());
    }

    #[test]
    fn preserves_supported_scalar_field_order_types_and_nullability() {
        let active = active(Vec::new(), 1);
        let candidate = candidate(
            &active,
            vec![object(
                FIRST_TYPE,
                "first",
                vec![
                    field(
                        FIRST_FIELD,
                        "count",
                        0,
                        ResolvedType::scalar(StandardScalar::Integer),
                        false,
                    ),
                    field(
                        SECOND_FIELD,
                        "title",
                        1,
                        ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                        true,
                    ),
                ],
            )],
            2,
        );

        let plan = plan_physical_changes(&active, &candidate).unwrap();
        let fields = plan.create_objects()[0].fields();

        assert_eq!(
            fields,
            [
                CreateField {
                    field_id: FIRST_FIELD,
                    field_type: PhysicalFieldType::Scalar(StandardScalar::Integer),
                    nullable: false,
                    unique: false,
                },
                CreateField {
                    field_id: SECOND_FIELD,
                    field_type: PhysicalFieldType::Scalar(StandardScalar::CharacterLargeObject,),
                    nullable: true,
                    unique: false,
                },
            ]
        );
        assert_eq!(fields[0].field_id(), FIRST_FIELD);
        assert_eq!(
            fields[1].field_type(),
            PhysicalFieldType::Scalar(StandardScalar::CharacterLargeObject)
        );
        assert!(!fields[0].nullable());
        assert!(fields[1].nullable());
    }

    #[test]
    fn rejects_stale_bases_and_object_drops() {
        let object = object(FIRST_TYPE, "first", Vec::new());
        let active_revision = active(vec![object.clone()], 1);
        let deployable = candidate(&active_revision, vec![object], 2);
        let other_active = active(Vec::new(), 3);

        assert!(matches!(
            plan_physical_changes(&other_active, &deployable),
            Err(PhysicalPlanError::ExpectedBaseMismatch { .. })
        ));

        let dropped = candidate(&active_revision, Vec::new(), 4);
        assert_eq!(
            plan_physical_changes(&active_revision, &dropped),
            Err(PhysicalPlanError::UnsupportedObjectDrop {
                object_type: FIRST_TYPE,
            })
        );
    }

    #[test]
    fn semantic_names_do_not_change_existing_object_storage() {
        let baseline = object(
            FIRST_TYPE,
            "first",
            vec![
                field(
                    FIRST_FIELD,
                    "first",
                    0,
                    ResolvedType::scalar(StandardScalar::Integer),
                    true,
                ),
                field(
                    SECOND_FIELD,
                    "second",
                    1,
                    ResolvedType::scalar(StandardScalar::Integer),
                    true,
                ),
            ],
        );
        let active = active(vec![baseline.clone()], 1);
        let renamed = object(
            FIRST_TYPE,
            "renamed",
            vec![
                field(
                    FIRST_FIELD,
                    "renamed_first",
                    0,
                    ResolvedType::scalar(StandardScalar::Integer),
                    true,
                ),
                field(
                    SECOND_FIELD,
                    "renamed_second",
                    1,
                    ResolvedType::scalar(StandardScalar::Integer),
                    true,
                ),
            ],
        );
        let candidate = candidate(&active, vec![renamed], 2);

        assert_eq!(
            plan_physical_changes(&active, &candidate),
            Ok(PhysicalPlan {
                create_objects: Vec::new(),
                add_field: None,
            })
        );
    }

    #[test]
    fn semantic_field_names_do_not_change_required_unique_reference_storage() {
        let target = object(SECOND_TYPE, "target", Vec::new());
        let baseline = object(
            FIRST_TYPE,
            "owner",
            vec![field_with_options(
                FIRST_FIELD,
                "target",
                0,
                ResolvedType::reference(SECOND_TYPE),
                false,
                true,
                None,
                Some(OnDeleteAction::Cascade),
            )],
        );
        let active = active(vec![baseline, target.clone()], 1);
        let renamed = object(
            FIRST_TYPE,
            "owner",
            vec![field_with_options(
                FIRST_FIELD,
                "renamed_target",
                0,
                ResolvedType::reference(SECOND_TYPE),
                false,
                true,
                None,
                Some(OnDeleteAction::Cascade),
            )],
        );
        let candidate = candidate(&active, vec![renamed, target], 2);

        assert_eq!(
            plan_physical_changes(&active, &candidate),
            Ok(PhysicalPlan {
                create_objects: Vec::new(),
                add_field: None,
            })
        );
    }

    #[test]
    fn rejects_adding_or_removing_existing_reference_uniqueness() {
        let target = object(SECOND_TYPE, "target", Vec::new());
        let owner = |unique| {
            object(
                FIRST_TYPE,
                "owner",
                vec![field_with_options(
                    FIRST_FIELD,
                    "target",
                    0,
                    ResolvedType::reference(SECOND_TYPE),
                    false,
                    unique,
                    None,
                    Some(OnDeleteAction::Restrict),
                )],
            )
        };

        let unique_active = active(vec![owner(true), target.clone()], 1);
        let remove_unique = candidate(&unique_active, vec![owner(false), target.clone()], 2);
        assert_eq!(
            plan_physical_changes(&unique_active, &remove_unique),
            Err(PhysicalPlanError::UnsupportedExistingObjectChange {
                object_type: FIRST_TYPE,
            })
        );

        let plain_active = active(vec![owner(false), target.clone()], 3);
        let add_unique = candidate(&plain_active, vec![owner(true), target], 4);
        assert_eq!(
            plan_physical_changes(&plain_active, &add_unique),
            Err(PhysicalPlanError::UnsupportedExistingObjectChange {
                object_type: FIRST_TYPE,
            })
        );
    }

    #[test]
    fn rejects_every_existing_object_storage_change_category() {
        let baseline = object(
            FIRST_TYPE,
            "first",
            vec![
                field(
                    FIRST_FIELD,
                    "first",
                    0,
                    ResolvedType::scalar(StandardScalar::Integer),
                    true,
                ),
                reference_field(
                    SECOND_FIELD,
                    "second",
                    1,
                    SECOND_TYPE,
                    true,
                    Some(OnDeleteAction::Restrict),
                ),
            ],
        );
        let target = object(SECOND_TYPE, "target", Vec::new());
        let active = active(vec![baseline.clone(), target.clone()], 1);
        let variants = vec![
            object(FIRST_TYPE, "first", vec![baseline.fields()[0].clone()]),
            object(
                FIRST_TYPE,
                "first",
                vec![
                    baseline.fields()[0].clone(),
                    baseline.fields()[1].clone(),
                    field(
                        FieldId::from_bytes([22; 16]),
                        "third",
                        2,
                        ResolvedType::reference(SECOND_TYPE),
                        true,
                    ),
                ],
            ),
            object(
                FIRST_TYPE,
                "first",
                vec![
                    field(
                        FieldId::from_bytes([22; 16]),
                        "first",
                        0,
                        ResolvedType::scalar(StandardScalar::Integer),
                        true,
                    ),
                    baseline.fields()[1].clone(),
                ],
            ),
            object(
                FIRST_TYPE,
                "first",
                vec![
                    field(
                        FIRST_FIELD,
                        "first",
                        0,
                        ResolvedType::scalar(StandardScalar::BigInt),
                        true,
                    ),
                    baseline.fields()[1].clone(),
                ],
            ),
            object(
                FIRST_TYPE,
                "first",
                vec![
                    field(
                        FIRST_FIELD,
                        "first",
                        0,
                        ResolvedType::scalar(StandardScalar::Integer),
                        false,
                    ),
                    baseline.fields()[1].clone(),
                ],
            ),
            object(
                FIRST_TYPE,
                "first",
                vec![
                    field(
                        FIRST_FIELD,
                        "first",
                        0,
                        ResolvedType::scalar(StandardScalar::Integer),
                        true,
                    ),
                    reference_field(
                        SECOND_FIELD,
                        "second",
                        1,
                        SECOND_TYPE,
                        true,
                        Some(OnDeleteAction::Cascade),
                    ),
                ],
            ),
            object(
                FIRST_TYPE,
                "first",
                vec![
                    field_with_options(
                        FIRST_FIELD,
                        "first",
                        0,
                        ResolvedType::scalar(StandardScalar::Integer),
                        true,
                        true,
                        None,
                        None,
                    ),
                    baseline.fields()[1].clone(),
                ],
            ),
            object(
                FIRST_TYPE,
                "first",
                vec![
                    field_with_options(
                        FIRST_FIELD,
                        "first",
                        0,
                        ResolvedType::scalar(StandardScalar::Integer),
                        true,
                        false,
                        Some(ExpressionId::from_bytes([30; 16])),
                        None,
                    ),
                    baseline.fields()[1].clone(),
                ],
            ),
            object(
                FIRST_TYPE,
                "first",
                vec![
                    field(
                        SECOND_FIELD,
                        "second",
                        0,
                        ResolvedType::scalar(StandardScalar::Integer),
                        true,
                    ),
                    reference_field(
                        FIRST_FIELD,
                        "first",
                        1,
                        SECOND_TYPE,
                        true,
                        Some(OnDeleteAction::Restrict),
                    ),
                ],
            ),
        ];

        for (index, variant) in variants.into_iter().enumerate() {
            let candidate = candidate(&active, vec![variant, target.clone()], 10 + index as u8);
            let expected = match index {
                6 => PhysicalPlanError::UnsupportedUniqueField {
                    object_type: FIRST_TYPE,
                    field: FIRST_FIELD,
                },
                7 => PhysicalPlanError::UnsupportedFieldDefault {
                    object_type: FIRST_TYPE,
                    field: FIRST_FIELD,
                },
                _ => PhysicalPlanError::UnsupportedExistingObjectChange {
                    object_type: FIRST_TYPE,
                },
            };
            assert_eq!(plan_physical_changes(&active, &candidate), Err(expected));
        }
    }

    #[test]
    fn active_physical_catalogue_preserves_object_and_field_order() {
        let baseline = object(
            FIRST_TYPE,
            "first",
            vec![field(
                FIRST_FIELD,
                "first",
                0,
                ResolvedType::scalar(StandardScalar::Integer),
                true,
            )],
        );
        let active = active(vec![baseline], 1);

        assert_eq!(
            active_physical_catalogue(&active).unwrap().objects(),
            [CreateObject {
                type_id: FIRST_TYPE,
                fields: vec![CreateField {
                    field_id: FIRST_FIELD,
                    field_type: PhysicalFieldType::Scalar(StandardScalar::Integer),
                    nullable: true,
                    unique: false,
                }],
            }]
        );
    }

    #[test]
    fn rejects_unsupported_new_field_semantics_fail_closed() {
        let active = active(Vec::new(), 1);
        let cases = [
            (
                field(
                    FIRST_FIELD,
                    "value",
                    0,
                    ResolvedType::Named(SECOND_TYPE),
                    true,
                ),
                PhysicalPlanError::UnsupportedNamedFieldType {
                    object_type: FIRST_TYPE,
                    field: FIRST_FIELD,
                },
            ),
            (
                field(
                    FIRST_FIELD,
                    "value",
                    0,
                    ResolvedType::scalar(StandardScalar::Void),
                    true,
                ),
                PhysicalPlanError::UnsupportedVoidField {
                    object_type: FIRST_TYPE,
                    field: FIRST_FIELD,
                },
            ),
        ];
        for (index, (field, expected)) in cases.into_iter().enumerate() {
            let candidate = candidate(
                &active,
                vec![object(FIRST_TYPE, "first", vec![field])],
                20 + index as u8,
            );
            assert_eq!(plan_physical_changes(&active, &candidate), Err(expected));
        }

        assert_new_field_error(
            &active,
            field_with_options(
                FIRST_FIELD,
                "value",
                0,
                ResolvedType::scalar(StandardScalar::Integer),
                true,
                true,
                None,
                None,
            ),
            PhysicalPlanError::UnsupportedUniqueField {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
            },
        );

        let target = object(SECOND_TYPE, "second", Vec::new());
        let nullable_unique_reference = field_with_options(
            FIRST_FIELD,
            "second",
            0,
            ResolvedType::reference(SECOND_TYPE),
            true,
            true,
            None,
            None,
        );
        let nullable_candidate = candidate(
            &active,
            vec![
                object(FIRST_TYPE, "first", vec![nullable_unique_reference]),
                target.clone(),
            ],
            29,
        );
        assert_eq!(
            plan_physical_changes(&active, &nullable_candidate),
            Err(PhysicalPlanError::UnsupportedUniqueField {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
            })
        );

        assert_new_field_error(
            &active,
            field_with_options(
                FIRST_FIELD,
                "value",
                0,
                ResolvedType::scalar(StandardScalar::Integer),
                true,
                false,
                Some(ExpressionId::from_bytes([31; 16])),
                None,
            ),
            PhysicalPlanError::UnsupportedFieldDefault {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
            },
        );

        assert_new_field_error(
            &active,
            field_with_options(
                FIRST_FIELD,
                "value",
                0,
                ResolvedType::scalar(StandardScalar::Integer),
                true,
                false,
                None,
                Some(OnDeleteAction::Cascade),
            ),
            PhysicalPlanError::InvalidDeleteAction {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
            },
        );

        assert_new_field_error(
            &active,
            reference_field(FIRST_FIELD, "missing", 0, SECOND_TYPE, true, None),
            PhysicalPlanError::UnknownReferenceTarget {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
                target: SECOND_TYPE,
            },
        );

        let invalid_set_null = reference_field(
            FIRST_FIELD,
            "second",
            0,
            SECOND_TYPE,
            false,
            Some(OnDeleteAction::SetNull),
        );
        let candidate = candidate(
            &active,
            vec![object(FIRST_TYPE, "first", vec![invalid_set_null]), target],
            30,
        );
        assert_eq!(
            plan_physical_changes(&active, &candidate),
            Err(PhysicalPlanError::InvalidDeleteAction {
                object_type: FIRST_TYPE,
                field: FIRST_FIELD,
            })
        );
    }

    #[test]
    fn migration_artifact_bytes_and_digest_are_deterministic() {
        let active = active(Vec::new(), 1);
        let candidate = candidate(
            &active,
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "value",
                    0,
                    ResolvedType::scalar(StandardScalar::Integer),
                    true,
                )],
            )],
            2,
        );

        let first = PhysicalMigrationArtifact::from_revisions(&active, &candidate).unwrap();
        let second = PhysicalMigrationArtifact::from_revisions(&active, &candidate).unwrap();

        assert_eq!(first.canonical_bytes(), second.canonical_bytes());
        assert_eq!(first.digest(), second.digest());
        assert_eq!(
            first.digest(),
            crate::canonical_hash::artifact_payload_digest(first.canonical_bytes()).unwrap()
        );
        assert_eq!(&first.canonical_bytes()[..MAGIC.len()], &MAGIC);
        assert_eq!(
            u32::from_be_bytes(
                first.canonical_bytes()[MAGIC.len()..MAGIC.len() + 4]
                    .try_into()
                    .unwrap()
            ),
            FORMAT_VERSION
        );
    }

    fn recovery_artifact() -> PhysicalMigrationArtifact {
        let active = active(Vec::new(), 1);
        let candidate = candidate(
            &active,
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "value",
                    0,
                    ResolvedType::scalar(StandardScalar::Integer),
                    true,
                )],
            )],
            2,
        );
        PhysicalMigrationArtifact::from_revisions(&active, &candidate)
            .expect("recovery fixture must produce an artifact")
    }

    #[test]
    fn migration_artifact_recovers_valid_canonical_bytes() {
        let artifact = recovery_artifact();

        let recovered = PhysicalMigrationArtifact::from_canonical_bytes(
            artifact.expected_base(),
            artifact.candidate_pair(),
            artifact.canonical_bytes(),
            artifact.digest(),
        )
        .expect("canonical artifact must recover");

        assert_eq!(recovered, artifact);
    }

    #[test]
    fn migration_artifact_recovery_rejects_mismatched_pairs() {
        let artifact = recovery_artifact();
        let different_base = RevisionPair::new(
            SourceRevisionId::from_bytes([0xf1; 16]),
            artifact.expected_base().catalogue(),
        );
        let different_candidate = RevisionPair::new(
            SourceRevisionId::from_bytes([0xf2; 16]),
            artifact.candidate_pair().catalogue(),
        );

        assert_eq!(
            PhysicalMigrationArtifact::from_canonical_bytes(
                different_base,
                artifact.candidate_pair(),
                artifact.canonical_bytes(),
                artifact.digest(),
            ),
            Err(PhysicalMigrationArtifactError::ExpectedBaseMismatch {
                expected: different_base,
                actual: artifact.expected_base(),
            })
        );
        assert_eq!(
            PhysicalMigrationArtifact::from_canonical_bytes(
                artifact.expected_base(),
                different_candidate,
                artifact.canonical_bytes(),
                artifact.digest(),
            ),
            Err(PhysicalMigrationArtifactError::CandidatePairMismatch {
                expected: different_candidate,
                actual: artifact.candidate_pair(),
            })
        );
    }

    #[test]
    fn migration_artifact_recovery_rejects_bad_header_tags_and_trailing_bytes() {
        let artifact = recovery_artifact();
        let expected_base = artifact.expected_base();
        let candidate_pair = artifact.candidate_pair();
        let decode = |bytes: Vec<u8>| {
            let digest = crate::canonical_hash::artifact_payload_digest(&bytes)
                .expect("test artifact digest");
            PhysicalMigrationArtifact::from_canonical_bytes(
                expected_base,
                candidate_pair,
                &bytes,
                digest,
            )
        };
        let operation_offset = MAGIC.len() + 4 + 32 + 32 + 4;
        let field_type_offset = operation_offset + 1 + 16 + 4 + 16;

        let mut bad_magic = artifact.canonical_bytes().to_vec();
        bad_magic[0] ^= 0xff;
        assert_eq!(
            decode(bad_magic),
            Err(PhysicalMigrationArtifactError::InvalidMagic)
        );

        let mut bad_version = artifact.canonical_bytes().to_vec();
        bad_version[MAGIC.len()..MAGIC.len() + 4].copy_from_slice(&2_u32.to_be_bytes());
        assert_eq!(
            decode(bad_version),
            Err(PhysicalMigrationArtifactError::UnsupportedVersion(2))
        );

        let mut bad_operation = artifact.canonical_bytes().to_vec();
        bad_operation[operation_offset] = 0xff;
        assert_eq!(
            decode(bad_operation),
            Err(PhysicalMigrationArtifactError::InvalidOperationTag(0xff))
        );

        let mut bad_field_type = artifact.canonical_bytes().to_vec();
        bad_field_type[field_type_offset] = 0xff;
        assert_eq!(
            decode(bad_field_type),
            Err(PhysicalMigrationArtifactError::InvalidFieldTypeTag(0xff))
        );

        let mut bad_scalar = artifact.canonical_bytes().to_vec();
        bad_scalar[field_type_offset + 1] = 0xff;
        assert_eq!(
            decode(bad_scalar),
            Err(PhysicalMigrationArtifactError::InvalidScalarTag(0xff))
        );

        let mut bad_boolean = artifact.canonical_bytes().to_vec();
        bad_boolean[field_type_offset + 2] = 2;
        assert_eq!(
            decode(bad_boolean),
            Err(PhysicalMigrationArtifactError::InvalidBoolean {
                context: "physical field nullable",
                value: 2,
            })
        );

        let mut trailing = artifact.canonical_bytes().to_vec();
        trailing.push(0);
        assert_eq!(
            decode(trailing),
            Err(PhysicalMigrationArtifactError::TrailingBytes)
        );
    }

    #[test]
    fn migration_artifact_recovery_rejects_oversized_operation_count() {
        let artifact = recovery_artifact();
        let mut bytes = artifact.canonical_bytes().to_vec();
        let operation_count_offset = MAGIC.len() + 4 + 32 + 32;
        bytes[operation_count_offset..operation_count_offset + 4]
            .copy_from_slice(&(MAX_PHYSICAL_OPERATIONS + 1).to_be_bytes());
        let digest = crate::canonical_hash::artifact_payload_digest(&bytes).unwrap();

        assert_eq!(
            PhysicalMigrationArtifact::from_canonical_bytes(
                artifact.expected_base(),
                artifact.candidate_pair(),
                &bytes,
                digest,
            ),
            Err(PhysicalMigrationArtifactError::CollectionLimit {
                kind: "physical operations",
                count: MAX_PHYSICAL_OPERATIONS as usize + 1,
                maximum: MAX_PHYSICAL_OPERATIONS,
            })
        );

        let mut field_bytes = artifact.canonical_bytes().to_vec();
        let field_count_offset = operation_count_offset + 4 + 1 + 16;
        field_bytes[field_count_offset..field_count_offset + 4]
            .copy_from_slice(&(MAX_PHYSICAL_FIELDS + 1).to_be_bytes());
        let field_digest = crate::canonical_hash::artifact_payload_digest(&field_bytes).unwrap();

        assert_eq!(
            PhysicalMigrationArtifact::from_canonical_bytes(
                artifact.expected_base(),
                artifact.candidate_pair(),
                &field_bytes,
                field_digest,
            ),
            Err(PhysicalMigrationArtifactError::CollectionLimit {
                kind: "physical object fields",
                count: MAX_PHYSICAL_FIELDS as usize + 1,
                maximum: MAX_PHYSICAL_FIELDS,
            })
        );
    }

    #[test]
    fn migration_artifact_preserves_plan_operation_order() {
        let active = active(Vec::new(), 1);
        let candidate = candidate(
            &active,
            vec![
                object(
                    SECOND_TYPE,
                    "second",
                    vec![field(
                        SECOND_FIELD,
                        "value",
                        0,
                        ResolvedType::scalar(StandardScalar::Integer),
                        true,
                    )],
                ),
                object(
                    FIRST_TYPE,
                    "first",
                    vec![field(
                        FIRST_FIELD,
                        "value",
                        0,
                        ResolvedType::scalar(StandardScalar::Integer),
                        true,
                    )],
                ),
            ],
            2,
        );

        let artifact = PhysicalMigrationArtifact::from_revisions(&active, &candidate).unwrap();
        assert_eq!(artifact.operations().len(), 2);
        assert!(matches!(
            &artifact.operations()[0],
            PhysicalOperation::CreateObject(object) if object.type_id() == SECOND_TYPE
        ));
        assert!(matches!(
            &artifact.operations()[1],
            PhysicalOperation::CreateObject(object) if object.type_id() == FIRST_TYPE
        ));
    }

    #[test]
    fn migration_artifact_orders_new_objects_before_existing_field_addition() {
        let boolean = TypeId::from_bytes([0xd1; 16]);
        let standard = verified_standard(vec![standard_value_type(
            boolean,
            "orna.kernel.value.boolean@1",
            ValueTypePersistence::Persistable,
        )]);
        let existing = field(
            FIRST_FIELD,
            "first_value",
            0,
            ResolvedType::Value(boolean),
            false,
        );
        let appended = field(
            SECOND_FIELD,
            "second_value",
            1,
            ResolvedType::Value(boolean),
            true,
        );
        let active = active_version_two(
            vec![object(FIRST_TYPE, "first", vec![existing.clone()])],
            standard.clone(),
            1,
        );
        let candidate = candidate_version_two(
            &active,
            vec![
                object(FIRST_TYPE, "first", vec![existing, appended]),
                object(
                    SECOND_TYPE,
                    "second",
                    vec![field(
                        FIRST_FIELD,
                        "value",
                        0,
                        ResolvedType::Value(boolean),
                        true,
                    )],
                ),
            ],
            standard,
            2,
        );

        let artifact = PhysicalMigrationArtifact::from_revisions(&active, &candidate).unwrap();
        assert!(matches!(
            &artifact.operations()[0],
            PhysicalOperation::CreateObject(object) if object.type_id() == SECOND_TYPE
        ));
        assert!(matches!(
            &artifact.operations()[1],
            PhysicalOperation::AddField(add_field)
                if add_field.object_type() == FIRST_TYPE
                    && add_field.field().field_id() == SECOND_FIELD
        ));
    }

    #[test]
    fn migration_artifact_binds_expected_and_candidate_revision_pairs() {
        let active = active(Vec::new(), 1);
        let candidate = candidate(
            &active,
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "value",
                    0,
                    ResolvedType::scalar(StandardScalar::Integer),
                    true,
                )],
            )],
            2,
        );
        let plan = plan_physical_changes(&active, &candidate).unwrap();
        let expected_base = active.pair();
        let candidate_pair = candidate.candidate_pair();
        let artifact =
            PhysicalMigrationArtifact::from_plan(expected_base, candidate_pair, &plan).unwrap();

        assert_eq!(artifact.expected_base(), expected_base);
        assert_eq!(artifact.candidate_pair(), candidate_pair);

        let different_base = RevisionPair::new(
            SourceRevisionId::from_bytes([0xf1; 16]),
            expected_base.catalogue(),
        );
        let different =
            PhysicalMigrationArtifact::from_plan(different_base, candidate_pair, &plan).unwrap();
        assert_ne!(artifact.canonical_bytes(), different.canonical_bytes());
        assert_ne!(artifact.digest(), different.digest());
    }

    #[test]
    fn migration_artifact_propagates_unsupported_physical_planning() {
        let active = active(
            vec![object(
                FIRST_TYPE,
                "first",
                vec![field(
                    FIRST_FIELD,
                    "value",
                    0,
                    ResolvedType::scalar(StandardScalar::Integer),
                    true,
                )],
            )],
            1,
        );
        let candidate = candidate(&active, Vec::new(), 2);

        assert_eq!(
            PhysicalMigrationArtifact::from_revisions(&active, &candidate),
            Err(PhysicalMigrationArtifactError::Planning(
                PhysicalPlanError::UnsupportedObjectDrop {
                    object_type: FIRST_TYPE,
                }
            ))
        );
    }

    fn assert_new_field_error(
        active: &ActiveDatabaseRevision,
        field: FieldDefinition,
        expected: PhysicalPlanError,
    ) {
        let candidate = candidate(active, vec![object(FIRST_TYPE, "first", vec![field])], 40);
        assert_eq!(plan_physical_changes(active, &candidate), Err(expected));
    }

    fn active_version_two(
        objects: Vec<ObjectTypeDefinition>,
        standard: crate::revision::VerifiedStandardLibrarySnapshot,
        seed: u8,
    ) -> ActiveDatabaseRevision {
        let source = source(seed, None);
        let catalogue = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([seed.wrapping_add(1); 16]),
            vec![schema()],
            objects,
        )
        .unwrap();
        let origins = origins(&source, &catalogue);
        let pair = RevisionPair::new(source.id(), catalogue.revision());
        ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                pair,
                source,
                catalogue,
                digest(seed),
                ActiveRevisionContent::new(Vec::new(), Vec::new(), origins, Vec::new()),
            ),
            CatalogueHashContext::version_two(standard),
        )
        .unwrap()
    }

    fn candidate_version_two(
        active: &ActiveDatabaseRevision,
        objects: Vec<ObjectTypeDefinition>,
        standard: crate::revision::VerifiedStandardLibrarySnapshot,
        seed: u8,
    ) -> DeployableRevision {
        let source = source(seed, Some(active.pair().source()));
        let catalogue = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([seed.wrapping_add(1); 16]),
            vec![schema()],
            objects,
        )
        .unwrap();
        let origins = origins(&source, &catalogue);
        DeployableRevision::new_with_catalogue_hash_context(
            DeployableRevisionInput::new(
                active.pair(),
                source,
                active.pair().catalogue(),
                catalogue,
                digest(seed),
                DeployableRevisionContent::new(origins, Vec::new(), Vec::new(), Vec::new())
                    .with_current_function_revisions(Vec::new()),
            ),
            CatalogueHashContext::version_two(standard),
        )
        .unwrap()
    }

    fn verified_standard(
        value_types: Vec<ValueTypeDefinition>,
    ) -> crate::revision::VerifiedStandardLibrarySnapshot {
        let source = standard_source();
        let catalogue = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes([0xa2; 16]),
            vec![
                SchemaDefinition::new(STANDARD_SCHEMA_ID, name(&["std"])),
                SchemaDefinition::new(STANDARD_TYPES_SCHEMA_ID, name(&["std", "types"])),
            ],
            Vec::new(),
            value_types,
            Vec::new(),
        )
        .unwrap();
        let origins = standard_origins(&source, &catalogue);
        let provisional = StandardLibrarySnapshot::new(
            StandardLibraryRevisionId::from_bytes([0xa3; 16]),
            StandardLibraryDigestVersion::Version1,
            source.clone(),
            "orna.language/1",
            catalogue.clone(),
            origins.clone(),
            digest(0xa4),
        )
        .unwrap();
        let digest = calculate_standard_library_digest_for_test(&provisional).unwrap();
        verify_standard_library_snapshot(
            StandardLibrarySnapshot::new(
                provisional.revision(),
                provisional.digest_version(),
                source,
                provisional.language_version(),
                catalogue,
                origins,
                digest,
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn standard_value_type(
        id: TypeId,
        contract: &str,
        persistence: ValueTypePersistence,
    ) -> ValueTypeDefinition {
        ValueTypeDefinition::primitive(
            id,
            QualifiedSemanticName::new(vec![
                "std".to_owned(),
                "types".to_owned(),
                format!("value_{:02x}", id.to_bytes()[0]),
            ])
            .unwrap(),
            ValueTypeMutability::Immutable,
            persistence,
            contract,
        )
    }

    fn standard_source() -> StoredSourceRevision {
        let bundle = SourceBundleId::from_bytes([0xa5; 16]);
        let revision = SourceRevisionId::from_bytes([0xa6; 16]);
        let content = "CREATE SCHEMA std;";
        let unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes([0xa7; 16]),
            0,
            "std/types.orna",
            content,
            source_unit_content_digest(content).unwrap(),
        )
        .unwrap();
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
        let revision_hash = source_revision_record_digest(bundle, None, bundle_hash).unwrap();
        StoredSourceRevision::new(
            bundle,
            revision,
            None,
            vec![unit],
            bundle_hash,
            revision_hash,
        )
        .unwrap()
    }

    fn standard_origins(
        source: &StoredSourceRevision,
        catalogue: &CatalogueSnapshot,
    ) -> Vec<DefinitionOrigin> {
        let origin = SourceOrigin::new(
            source.units()[0].id(),
            0,
            u32::try_from(source.units()[0].content().len()).unwrap(),
        )
        .unwrap();
        catalogue
            .schemas()
            .iter()
            .map(|schema| DefinitionIdentity::Schema(schema.id()))
            .chain(
                catalogue
                    .value_types()
                    .iter()
                    .map(|value_type| DefinitionIdentity::ValueType(value_type.id())),
            )
            .map(|identity| DefinitionOrigin::new(identity, origin))
            .collect()
    }

    fn active(objects: Vec<ObjectTypeDefinition>, seed: u8) -> ActiveDatabaseRevision {
        let source = source(seed, None);
        let catalogue = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([seed.wrapping_add(1); 16]),
            vec![schema()],
            objects,
        )
        .unwrap();
        let origins = origins(&source, &catalogue);
        let pair = RevisionPair::new(source.id(), catalogue.revision());
        ActiveDatabaseRevision::new(
            pair,
            source,
            catalogue,
            digest(seed),
            Vec::new(),
            Vec::new(),
            origins,
            Vec::new(),
        )
        .unwrap()
    }

    fn candidate(
        active: &ActiveDatabaseRevision,
        objects: Vec<ObjectTypeDefinition>,
        seed: u8,
    ) -> DeployableRevision {
        let source = source(seed, Some(active.pair().source()));
        let catalogue = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([seed.wrapping_add(1); 16]),
            vec![schema()],
            objects,
        )
        .unwrap();
        let origins = origins(&source, &catalogue);
        DeployableRevision::new(
            active.pair(),
            source,
            active.pair().catalogue(),
            catalogue,
            digest(seed),
            origins,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap()
    }

    fn source(seed: u8, parent: Option<SourceRevisionId>) -> StoredSourceRevision {
        let bundle = SourceBundleId::from_bytes([seed.wrapping_add(2); 16]);
        let revision = SourceRevisionId::from_bytes([seed.wrapping_add(3); 16]);
        let unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes([seed.wrapping_add(4); 16]),
            0,
            "physical.orna",
            "source",
            digest(seed),
        )
        .unwrap();
        StoredSourceRevision::new(
            bundle,
            revision,
            parent,
            vec![unit],
            digest(seed),
            digest(seed),
        )
        .unwrap()
    }

    fn origins(
        source: &StoredSourceRevision,
        catalogue: &CatalogueSnapshot,
    ) -> Vec<DefinitionOrigin> {
        let source_origin = SourceOrigin::new(source.units()[0].id(), 0, 6).unwrap();
        let mut values = vec![DefinitionOrigin::new(
            DefinitionIdentity::Schema(SCHEMA_ID),
            source_origin,
        )];
        for object_type in catalogue.object_types() {
            values.push(DefinitionOrigin::new(
                DefinitionIdentity::ObjectType(object_type.id()),
                source_origin,
            ));
            values.extend(object_type.fields().iter().map(|field| {
                DefinitionOrigin::new(
                    DefinitionIdentity::Field {
                        owner: object_type.id(),
                        field: field.id(),
                    },
                    source_origin,
                )
            }));
        }
        values.extend(catalogue.enum_types().iter().map(|enum_type| {
            DefinitionOrigin::new(DefinitionIdentity::ValueType(enum_type.id()), source_origin)
        }));
        for record_type in catalogue.record_value_types() {
            values.push(DefinitionOrigin::new(
                DefinitionIdentity::ValueType(record_type.id()),
                source_origin,
            ));
            values.extend(record_type.fields().iter().map(|field| {
                DefinitionOrigin::new(
                    DefinitionIdentity::Field {
                        owner: record_type.id(),
                        field: field.id(),
                    },
                    source_origin,
                )
            }));
        }
        values
    }

    fn schema() -> SchemaDefinition {
        SchemaDefinition::new(SCHEMA_ID, name(&["demo"]))
    }

    fn object(id: TypeId, object_name: &str, fields: Vec<FieldDefinition>) -> ObjectTypeDefinition {
        ObjectTypeDefinition::new(id, name(&["demo", object_name]), fields)
    }

    fn field(
        id: FieldId,
        field_name: &str,
        ordinal: u32,
        resolved_type: ResolvedType,
        nullable: bool,
    ) -> FieldDefinition {
        field_with_options(
            id,
            field_name,
            ordinal,
            resolved_type,
            nullable,
            false,
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn field_with_options(
        id: FieldId,
        field_name: &str,
        ordinal: u32,
        resolved_type: ResolvedType,
        nullable: bool,
        unique: bool,
        default_expression: Option<ExpressionId>,
        on_delete: Option<OnDeleteAction>,
    ) -> FieldDefinition {
        FieldDefinition::new(
            id,
            field_name,
            ordinal,
            resolved_type,
            nullable,
            unique,
            default_expression,
            on_delete,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn reference_field(
        id: FieldId,
        field_name: &str,
        ordinal: u32,
        target: TypeId,
        nullable: bool,
        on_delete: Option<OnDeleteAction>,
    ) -> FieldDefinition {
        FieldDefinition::new(
            id,
            field_name,
            ordinal,
            ResolvedType::reference(target),
            nullable,
            false,
            None,
            on_delete,
        )
    }

    fn name(parts: &[&str]) -> QualifiedSemanticName {
        QualifiedSemanticName::new(parts.iter().copied()).unwrap()
    }

    const fn digest(seed: u8) -> Sha256Digest {
        Sha256Digest::from_bytes([seed; 32])
    }
}
