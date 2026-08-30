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
mod tests;
