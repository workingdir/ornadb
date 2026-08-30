//! Canonical versioned hashes for durable OrnaDB revision data.
//!
//! Each function in this module first writes one complete, domain-separated
//! byte sequence and then calculates SHA-256 over that sequence. The encoding
//! is not a public wire protocol. It is a durable integrity contract between
//! revision preparation and recovery.

use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt,
};

use sha2::{Digest, Sha256};

use crate::{
    ExpressionId, FieldId, FunctionId, FunctionRevisionId, ParameterId, SchemaId, SourceBundleId,
    SourceRevisionId, SourceUnitId, StandardLibraryRevisionId, TypeBindingId, TypeId,
    catalogue::{
        CatalogueSnapshot, EnumTypeDefinition, FunctionDefinition, FunctionDomain, FunctionReturn,
        FunctionSecurity, FunctionTransaction, FunctionVolatility, ObjectTypeDefinition,
        OnDeleteAction, ParameterDefinition, PreludeTypeName, QualifiedSemanticName,
        SchemaDefinition, TypeBinding, TypeBindingKind, TypeLookupName, ValueTypeDefinition,
        ValueTypeKind, ValueTypeMutability, ValueTypePersistence,
    },
    revision::{
        CatalogueHashContext, CatalogueHashVersion, DefinitionIdentity, DefinitionOrigin,
        DefinitionReference, DefinitionReferenceKind, DefinitionReferenceTarget,
        ExecutableArtifact, ExecutableArtifactKind, ExpressionArtifact, FunctionRevisionRecord,
        FunctionSemanticHashVersion, RecordValueFieldDescriptorClass,
        RecordValueFieldDescriptorClassificationError, RecordValueFieldDescriptorValidationError,
        Sha256Digest, SourceOrigin, StandardLibraryDigestVersion, StandardLibrarySnapshot,
        StoredSourceRevision, StoredSourceUnit, VerifiedStandardLibrarySnapshot,
        classify_record_value_field_descriptor, function_accepts_opaque_client_return,
        reference_kind_accepts_target, validate_record_value_field_descriptors,
    },
    types::{ResolvedType, StandardScalar, TypeDescriptor},
};

const SOURCE_UNIT_CONTENT_DOMAIN: &[u8] = b"ornadb.hash/source-unit-content/v1\0";
const SOURCE_BUNDLE_DOMAIN: &[u8] = b"ornadb.hash/source-bundle/v1\0";
const SOURCE_REVISION_DOMAIN: &[u8] = b"ornadb.hash/source-revision/v1\0";
const CATALOGUE_DOMAIN: &[u8] = b"ornadb.hash/catalogue/v1\0";
const CATALOGUE_V2_DOMAIN: &[u8] = b"ornadb.hash/catalogue/v2\0";
const FUNCTION_DECLARATION_DOMAIN: &[u8] = b"ornadb.hash/function-declaration/v1\0";
const FUNCTION_SEMANTIC_DOMAIN: &[u8] = b"ornadb.hash/function-semantic/v1\0";
const FUNCTION_SEMANTIC_V2_DOMAIN: &[u8] = b"ornadb.hash/function-semantic/v2\0";
const STANDARD_LIBRARY_DOMAIN: &[u8] = b"ornadb.hash/standard-library/v1\0";
const STANDARD_LIBRARY_V2_DOMAIN: &[u8] = b"ornadb.hash/standard-library/v2\0";
const ARTIFACT_PAYLOAD_DOMAIN: &[u8] = b"ornadb.hash/artifact-payload/v1\0";

/// One typed fact that cannot be represented by the selected catalogue hash contract.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CatalogueHashFact {
    /// One value-type definition.
    ValueTypeDefinition(TypeId),
    /// One named record-value definition.
    RecordValueTypeDefinition(TypeId),
    /// One enum-type definition.
    EnumTypeDefinition(TypeId),
    /// One direct type-name binding.
    TypeBinding(TypeBindingId),
    /// One definition source origin.
    DefinitionOrigin(DefinitionIdentity),
    /// One resolved reference target.
    DefinitionReferenceTarget(DefinitionReferenceTarget),
    /// One immutable function revision's semantic-hash contract.
    FunctionSemanticHashVersion {
        /// The function owning the immutable revision.
        function: FunctionId,
        /// The immutable function revision.
        revision: FunctionRevisionId,
        /// The incompatible semantic-hash contract.
        version: FunctionSemanticHashVersion,
    },
}

/// One typed function fact that cannot be represented by an older semantic hash.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FunctionSemanticHashFact {
    /// One resolved value-type reference target.
    ValueTypeReference(TypeId),
}

/// An error returned when input cannot form the selected canonical hash bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CanonicalHashError {
    /// A text, blob, or sequence length cannot be represented by the codec.
    LengthExceedsU32 {
        /// The encoded value category.
        value: &'static str,
        /// The unrepresentable length.
        length: usize,
    },
    /// A durable source unit's retained content hash is not its exact content hash.
    SourceContentHashMismatch {
        /// The source unit with inconsistent retained content.
        source_unit: SourceUnitId,
    },
    /// A raw source bundle uses a duplicate source-unit identity.
    DuplicateSourceUnitId {
        /// The repeated source unit identity.
        source_unit: SourceUnitId,
    },
    /// A raw source bundle uses a duplicate logical path.
    DuplicateSourceLogicalPath {
        /// The repeated logical path.
        logical_path: String,
    },
    /// A raw source bundle's ordinals are not contiguous and zero-based.
    SourceOrdinalOutOfSequence {
        /// The source unit with the incorrect ordinal.
        source_unit: SourceUnitId,
        /// The required ordinal after canonical sorting.
        expected: u32,
        /// The retained ordinal.
        actual: u32,
    },
    /// A durable source revision's retained bundle hash is inconsistent.
    SourceBundleHashMismatch {
        /// The source bundle with an inconsistent aggregate hash.
        source_bundle: SourceBundleId,
    },
    /// A standard source revision's retained revision hash is inconsistent.
    StandardSourceRevisionHashMismatch {
        /// The inconsistent source revision.
        source_revision: SourceRevisionId,
    },
    /// A retained standard-library digest differs from its canonical facts.
    StandardLibraryDigestMismatch {
        /// The inconsistent standard-library revision.
        revision: StandardLibraryRevisionId,
    },
    /// A caller required a different standard-library digest contract.
    StandardLibraryDigestVersionMismatch {
        /// The selected standard-library revision.
        revision: StandardLibraryRevisionId,
        /// The required digest contract.
        expected: StandardLibraryDigestVersion,
        /// The retained digest contract.
        actual: StandardLibraryDigestVersion,
    },
    /// A resolved value type requires the version-2 catalogue hash contract.
    ResolvedValueRequiresCatalogueHashVersionTwo {
        /// The resolved catalogue slot that contains the value type.
        identity: DefinitionIdentity,
        /// The resolved standard value type identity.
        value_type: TypeId,
    },
    /// A legacy scalar is not valid in the version-2 catalogue hash contract.
    LegacyScalarRequiresCatalogueHashVersionOne {
        /// The resolved catalogue slot that contains the scalar.
        identity: DefinitionIdentity,
        /// The legacy scalar representation.
        scalar: StandardScalar,
    },
    /// A resolved value type is absent from the pinned standard library.
    ResolvedValueTypeNotInPinnedStandard {
        /// The resolved catalogue slot that contains the value type.
        identity: DefinitionIdentity,
        /// The missing resolved standard value type identity.
        value_type: TypeId,
    },
    /// A resolved catalogue slot names a transient opaque value type.
    OpaqueValueTypeNotAcceptedInSlot {
        /// The resolved catalogue slot that contains the opaque type.
        identity: DefinitionIdentity,
        /// The rejected opaque value type identity.
        value_type: TypeId,
    },
    /// A record field uses a resolved type outside the initial record family.
    UnsupportedRecordValueFieldType {
        /// The record value type owning the field.
        record_value_type: TypeId,
        /// The rejected record field.
        field: FieldId,
        /// The unsupported descriptor.
        descriptor: TypeDescriptor,
    },
    /// A record field identity has conflicting application and standard meanings.
    AmbiguousRecordValueFieldType {
        /// The record value type owning the field.
        record_value_type: TypeId,
        /// The ambiguous record field.
        field: FieldId,
        /// The conflicting type identity.
        type_id: TypeId,
    },
    /// Record fields form a recursive by-value dependency.
    RecursiveRecordValueField {
        /// The record value type owning the rejected field.
        record_value_type: TypeId,
        /// The edge that closes the cycle.
        field: FieldId,
        /// The active record value type already on the dependency path.
        nested_record_value_type: TypeId,
    },
    /// Record fields exceed the maximum by-value nesting depth.
    RecordValueNestingTooDeep {
        /// The record value type owning the rejected field.
        record_value_type: TypeId,
        /// The edge that exceeds the maximum depth.
        field: FieldId,
        /// The record value type selected by the rejected edge.
        nested_record_value_type: TypeId,
        /// The accepted maximum number of record-valued edges.
        maximum: u32,
        /// The first depth outside the accepted maximum.
        actual: u32,
    },
    /// A typed catalogue fact cannot be represented by the selected hash contract.
    CatalogueFactUnsupportedByHashVersion {
        /// The selected catalogue hash contract.
        version: CatalogueHashVersion,
        /// The incompatible catalogue fact.
        fact: CatalogueHashFact,
    },
    /// A typed function fact cannot be represented by the selected semantic hash contract.
    FunctionFactUnsupportedBySemanticHashVersion {
        /// The selected function semantic hash contract.
        version: FunctionSemanticHashVersion,
        /// The affected function.
        function: FunctionId,
        /// The incompatible semantic fact.
        fact: FunctionSemanticHashFact,
    },
    /// An artifact record's retained payload hash is inconsistent.
    ArtifactPayloadHashMismatch {
        /// A stable label for the inconsistent artifact role.
        artifact: &'static str,
    },
    /// A function name does not resolve to one exact declared schema.
    FunctionOwnerSchemaNotFound {
        /// The function whose namespace has no exact schema owner.
        function: FunctionId,
    },
    /// An object type name does not resolve to one exact declared schema.
    ObjectTypeOwnerSchemaNotFound {
        /// The object type whose namespace has no exact schema owner.
        object_type: TypeId,
    },
    /// A declared default expression has no artifact in the supplied view.
    DefaultExpressionArtifactNotFound {
        /// The missing expression artifact identity.
        expression: ExpressionId,
    },
    /// The supplied expression artifacts contain one identity more than once.
    DuplicateExpressionArtifact {
        /// The repeated expression identity.
        expression: ExpressionId,
    },
    /// A supplied current function revision names no function in the catalogue.
    FunctionRevisionFunctionNotFound {
        /// The revision's function identity.
        function: FunctionId,
        /// The supplied revision identity.
        revision: FunctionRevisionId,
    },
    /// More than one supplied current revision belongs to one function.
    DuplicateFunctionRevision {
        /// The function with more than one supplied current revision.
        function: FunctionId,
    },
    /// More than one supplied current record uses one revision identity.
    DuplicateFunctionRevisionId {
        /// The repeated function revision identity.
        revision: FunctionRevisionId,
    },
    /// A catalogue function does not have its exact current revision record.
    MissingCurrentFunctionRevision {
        /// The function without a supplied current revision record.
        function: FunctionId,
        /// The current revision recorded by the catalogue.
        revision: FunctionRevisionId,
    },
    /// A supplied function revision differs from the catalogue current revision.
    FunctionRevisionIsNotCurrent {
        /// The affected function.
        function: FunctionId,
        /// The revision recorded by the catalogue.
        expected: FunctionRevisionId,
        /// The supplied revision identity.
        actual: FunctionRevisionId,
    },
    /// A function artifact kind differs from the function domain.
    FunctionArtifactDomainMismatch {
        /// The affected function.
        function: FunctionId,
    },
    /// More than one reference has one source function and ordinal.
    DuplicateReferenceOrdinal {
        /// The function that owns the repeated ordinal.
        function: FunctionId,
        /// The repeated ordinal.
        ordinal: u32,
    },
    /// A semantic-hash reference names a different source function.
    ReferenceSourceFunctionMismatch {
        /// The function passed to the semantic hash operation.
        expected: FunctionId,
        /// The reference source function.
        actual: FunctionId,
    },
    /// A catalogue reference does not belong to a current function revision.
    ReferenceRevisionIsNotCurrent {
        /// The source function.
        function: FunctionId,
        /// The reference source revision.
        revision: FunctionRevisionId,
    },
    /// A catalogue reference targets no member of the supplied complete view.
    ReferenceTargetNotFound {
        /// The missing reference target.
        target: DefinitionReferenceTarget,
    },
    /// A reference kind cannot target the supplied definition kind.
    ReferenceKindTargetMismatch {
        /// The invalid reference kind.
        kind: DefinitionReferenceKind,
        /// The incompatible target.
        target: DefinitionReferenceTarget,
    },
    /// A catalogue origin names no member of the supplied complete view.
    OriginDefinitionNotFound {
        /// The missing origin identity.
        identity: DefinitionIdentity,
    },
    /// More than one catalogue origin names one definition.
    DuplicateDefinitionOrigin {
        /// The repeated definition identity.
        identity: DefinitionIdentity,
    },
    /// A complete catalogue view has no origin for one definition.
    MissingDefinitionOrigin {
        /// The definition without a source origin.
        identity: DefinitionIdentity,
    },
    /// A supplied semantic hash differs from the exact canonical semantic input.
    FunctionSemanticHashMismatch {
        /// The affected function.
        function: FunctionId,
        /// The current revision record with the inconsistent hash.
        revision: FunctionRevisionId,
    },
}

impl fmt::Display for CanonicalHashError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        use CanonicalHashError::*;

        match self {
            LengthExceedsU32 { value, .. } => write!(formatter, "{value} length exceeds u32"),
            SourceContentHashMismatch { .. } => {
                formatter.write_str("stored source content hash differs from exact content")
            }
            DuplicateSourceUnitId { .. } => formatter.write_str("duplicate source unit identity"),
            DuplicateSourceLogicalPath { .. } => {
                formatter.write_str("duplicate source logical path")
            }
            SourceOrdinalOutOfSequence { .. } => {
                formatter.write_str("source ordinals are not contiguous and zero-based")
            }
            SourceBundleHashMismatch { .. } => {
                formatter.write_str("stored source bundle hash differs from exact bundle")
            }
            StandardSourceRevisionHashMismatch { .. } => formatter
                .write_str("stored standard source revision hash differs from exact revision"),
            StandardLibraryDigestMismatch { .. } => {
                formatter.write_str("stored standard library digest differs from canonical facts")
            }
            StandardLibraryDigestVersionMismatch { .. } => formatter
                .write_str("standard library digest contract differs from the required version"),
            ResolvedValueRequiresCatalogueHashVersionTwo { .. } => {
                formatter.write_str("resolved value type requires catalogue hash version 2")
            }
            LegacyScalarRequiresCatalogueHashVersionOne { .. } => {
                formatter.write_str("legacy scalar resolved type requires catalogue hash version 1")
            }
            ResolvedValueTypeNotInPinnedStandard { .. } => formatter
                .write_str("resolved value type is absent from the pinned standard library"),
            OpaqueValueTypeNotAcceptedInSlot { .. } => {
                formatter.write_str("opaque value type is not accepted in a catalogue slot")
            }
            UnsupportedRecordValueFieldType { .. } => {
                formatter.write_str("record value field has an unsupported resolved type")
            }
            AmbiguousRecordValueFieldType { .. } => formatter.write_str(
                "record field type is present in both application and standard catalogues",
            ),
            RecursiveRecordValueField { .. } => {
                formatter.write_str("record value fields must not form a recursive cycle")
            }
            RecordValueNestingTooDeep { .. } => {
                formatter.write_str("record value nesting exceeds the maximum depth")
            }
            CatalogueFactUnsupportedByHashVersion { version, fact } => match fact {
                CatalogueHashFact::ValueTypeDefinition(_) => write!(
                    formatter,
                    "catalogue hash version {} cannot include value types; use catalogue hash version 2",
                    version.to_u32()
                ),
                CatalogueHashFact::RecordValueTypeDefinition(_) => write!(
                    formatter,
                    "catalogue hash version {} cannot include record value types; use catalogue hash version 2",
                    version.to_u32()
                ),
                CatalogueHashFact::EnumTypeDefinition(_) => write!(
                    formatter,
                    "catalogue hash version {} cannot include enum types; use catalogue hash version 2",
                    version.to_u32()
                ),
                CatalogueHashFact::TypeBinding(_) => write!(
                    formatter,
                    "catalogue hash version {} cannot include type-name bindings; use catalogue hash version 2",
                    version.to_u32()
                ),
                CatalogueHashFact::DefinitionOrigin(_) => write!(
                    formatter,
                    "catalogue hash version {} cannot include value-type or binding origins; use catalogue hash version 2",
                    version.to_u32()
                ),
                CatalogueHashFact::DefinitionReferenceTarget(_) => write!(
                    formatter,
                    "catalogue hash version {} cannot include value-type references; use catalogue hash version 2",
                    version.to_u32()
                ),
                CatalogueHashFact::FunctionSemanticHashVersion {
                    version: semantic_version,
                    ..
                } => write!(
                    formatter,
                    "catalogue hash version {} cannot include function semantic hash version {}; use catalogue hash version 2",
                    version.to_u32(),
                    semantic_version.to_u32()
                ),
            },
            FunctionFactUnsupportedBySemanticHashVersion { version, fact, .. } => match fact {
                FunctionSemanticHashFact::ValueTypeReference(_) => write!(
                    formatter,
                    "function semantic hash version {} cannot include value-type references; use function semantic hash version 2",
                    version.to_u32()
                ),
            },
            ArtifactPayloadHashMismatch { artifact } => {
                write!(
                    formatter,
                    "{artifact} payload hash differs from exact payload"
                )
            }
            FunctionOwnerSchemaNotFound { .. } => {
                formatter.write_str("function namespace has no exact schema owner")
            }
            ObjectTypeOwnerSchemaNotFound { .. } => {
                formatter.write_str("object type namespace has no exact schema owner")
            }
            DefaultExpressionArtifactNotFound { .. } => {
                formatter.write_str("default expression artifact is absent from complete view")
            }
            DuplicateExpressionArtifact { .. } => {
                formatter.write_str("duplicate expression artifact identity")
            }
            FunctionRevisionFunctionNotFound { .. } => {
                formatter.write_str("function revision function is absent from catalogue")
            }
            DuplicateFunctionRevision { .. } => formatter
                .write_str("more than one supplied function revision belongs to a function"),
            DuplicateFunctionRevisionId { .. } => {
                formatter.write_str("duplicate supplied function revision identity")
            }
            MissingCurrentFunctionRevision { .. } => {
                formatter.write_str("catalogue current function revision is absent")
            }
            FunctionRevisionIsNotCurrent { .. } => {
                formatter.write_str("supplied function revision is not current")
            }
            FunctionArtifactDomainMismatch { .. } => {
                formatter.write_str("function artifact kind differs from function domain")
            }
            DuplicateReferenceOrdinal { .. } => {
                formatter.write_str("duplicate reference ordinal for source function")
            }
            ReferenceSourceFunctionMismatch { .. } => {
                formatter.write_str("reference source function differs from semantic function")
            }
            ReferenceRevisionIsNotCurrent { .. } => formatter
                .write_str("reference source revision is not the current function revision"),
            ReferenceTargetNotFound { .. } => {
                formatter.write_str("reference target is absent from complete catalogue view")
            }
            ReferenceKindTargetMismatch { .. } => {
                formatter.write_str("reference kind cannot target the supplied definition kind")
            }
            OriginDefinitionNotFound { .. } => {
                formatter.write_str("definition origin is absent from complete catalogue view")
            }
            DuplicateDefinitionOrigin { .. } => formatter.write_str("duplicate definition origin"),
            MissingDefinitionOrigin { .. } => {
                formatter.write_str("definition has no source origin")
            }
            FunctionSemanticHashMismatch { .. } => {
                formatter.write_str("function semantic hash differs from canonical semantic input")
            }
        }
    }
}

impl Error for CanonicalHashError {}

/// Hashes exact UTF-8 source content.
pub fn source_unit_content_digest(content: &str) -> Result<Sha256Digest, CanonicalHashError> {
    let mut encoder = Encoder::new(SOURCE_UNIT_CONTENT_DOMAIN);
    encoder.blob(content.as_bytes(), "source content")?;
    Ok(encoder.digest())
}

/// Hashes exact binary artifact payload bytes.
pub fn artifact_payload_digest(payload: &[u8]) -> Result<Sha256Digest, CanonicalHashError> {
    let mut encoder = Encoder::new(ARTIFACT_PAYLOAD_DOMAIN);
    encoder.blob(payload, "artifact payload")?;
    Ok(encoder.digest())
}

/// Hashes the exact byte slice of one function declaration.
pub fn function_declaration_digest(declaration: &[u8]) -> Result<Sha256Digest, CanonicalHashError> {
    let mut encoder = Encoder::new(FUNCTION_DECLARATION_DOMAIN);
    encoder.blob(declaration, "function declaration")?;
    Ok(encoder.digest())
}

/// Hashes one ordered durable source bundle.
///
/// The result includes each source-unit identity, ordinal, logical path, and
/// verified exact content hash. It does not include the source-bundle identity.
pub fn source_bundle_digest(
    units: &[StoredSourceUnit],
) -> Result<Sha256Digest, CanonicalHashError> {
    let units = sorted_by_key(units, |unit| unit.ordinal());
    let mut source_unit_ids = HashSet::with_capacity(units.len());
    let mut logical_paths = HashSet::with_capacity(units.len());
    for (index, unit) in units.iter().enumerate() {
        let expected = length_to_u32(index, "source unit ordinal")?;
        if unit.ordinal() != expected {
            return Err(CanonicalHashError::SourceOrdinalOutOfSequence {
                source_unit: unit.id(),
                expected,
                actual: unit.ordinal(),
            });
        }
        if !source_unit_ids.insert(unit.id()) {
            return Err(CanonicalHashError::DuplicateSourceUnitId {
                source_unit: unit.id(),
            });
        }
        if !logical_paths.insert(unit.logical_path()) {
            return Err(CanonicalHashError::DuplicateSourceLogicalPath {
                logical_path: unit.logical_path().to_owned(),
            });
        }
    }
    let mut encoder = Encoder::new(SOURCE_BUNDLE_DOMAIN);
    encoder.sequence_len(units.len(), "source units")?;

    for unit in units {
        let exact_content_hash = source_unit_content_digest(unit.content())?;
        if exact_content_hash != unit.content_hash() {
            return Err(CanonicalHashError::SourceContentHashMismatch {
                source_unit: unit.id(),
            });
        }

        encoder.source_unit_id(unit.id());
        encoder.u32(unit.ordinal());
        encoder.text(unit.logical_path(), "source logical path")?;
        encoder.digest_value(unit.content_hash());
    }

    Ok(encoder.digest())
}

/// Hashes one durable source revision record.
///
/// The source revision identity is deliberately not hashed. The content hash
/// records the complete source bundle, its durable bundle identity, and its
/// parent source revision identity.
pub fn source_revision_digest(
    revision: &StoredSourceRevision,
) -> Result<Sha256Digest, CanonicalHashError> {
    let bundle_hash = source_bundle_digest(revision.units())?;
    if bundle_hash != revision.bundle_hash() {
        return Err(CanonicalHashError::SourceBundleHashMismatch {
            source_bundle: revision.bundle(),
        });
    }

    source_revision_record_digest(revision.bundle(), revision.parent(), revision.bundle_hash())
}

/// Hashes the durable fields of one source revision record.
///
/// This form lets revision preparation calculate the record hash before it
/// constructs the immutable [`StoredSourceRevision`] value.
pub fn source_revision_record_digest(
    bundle: SourceBundleId,
    parent: Option<SourceRevisionId>,
    bundle_hash: Sha256Digest,
) -> Result<Sha256Digest, CanonicalHashError> {
    let mut encoder = Encoder::new(SOURCE_REVISION_DOMAIN);
    encoder.source_bundle_id(bundle);
    encoder.option_source_revision_id(parent);
    encoder.digest_value(bundle_hash);
    Ok(encoder.digest())
}

/// Verifies and returns the canonical digest of one standard-library snapshot.
pub fn standard_library_digest(
    standard: &StandardLibrarySnapshot,
) -> Result<Sha256Digest, CanonicalHashError> {
    let digest = calculate_standard_library_digest(standard)?;
    if digest != standard.digest() {
        return Err(CanonicalHashError::StandardLibraryDigestMismatch {
            revision: standard.revision(),
        });
    }
    Ok(digest)
}

/// Verifies a retained version-1 standard snapshot and returns its trust capability.
pub fn verify_standard_library_snapshot(
    standard: StandardLibrarySnapshot,
) -> Result<VerifiedStandardLibrarySnapshot, CanonicalHashError> {
    verify_standard_library_snapshot_version(standard, StandardLibraryDigestVersion::Version1)
}

/// Verifies a retained executable standard snapshot under the V2 contract.
pub fn verify_standard_library_v2_snapshot(
    standard: StandardLibrarySnapshot,
) -> Result<VerifiedStandardLibrarySnapshot, CanonicalHashError> {
    verify_standard_library_snapshot_version(standard, StandardLibraryDigestVersion::Version2)
}

fn verify_standard_library_snapshot_version(
    standard: StandardLibrarySnapshot,
    expected: StandardLibraryDigestVersion,
) -> Result<VerifiedStandardLibrarySnapshot, CanonicalHashError> {
    if standard.digest_version() != expected {
        return Err(CanonicalHashError::StandardLibraryDigestVersionMismatch {
            revision: standard.revision(),
            expected,
            actual: standard.digest_version(),
        });
    }
    standard_library_digest(&standard)?;
    Ok(VerifiedStandardLibrarySnapshot::new(standard))
}

/// Calculates the canonical digest for a standard-library snapshot.
///
/// This validates the source revision and executable facts, then returns the
/// digest calculated from the snapshot. It does not compare the result with
/// the digest stored in the snapshot. Call `standard_library_digest` when the
/// embedded digest must also be verified.
pub fn calculate_standard_library_digest(
    standard: &StandardLibrarySnapshot,
) -> Result<Sha256Digest, CanonicalHashError> {
    let source_hash = source_revision_digest(standard.source())?;
    if source_hash != standard.source().revision_hash() {
        return Err(CanonicalHashError::StandardSourceRevisionHashMismatch {
            source_revision: standard.source().id(),
        });
    }

    let expressions = HashMap::new();
    let origins = complete_origins(standard.catalogue(), &expressions, standard.origins())?;
    match standard.digest_version() {
        StandardLibraryDigestVersion::Version1 => {
            let mut encoder = Encoder::new(STANDARD_LIBRARY_DOMAIN);
            encoder.u32(StandardLibraryDigestVersion::Version1.to_u32());
            encoder.standard_library_revision_id(standard.revision());
            encoder.source_revision_id(standard.source().id());
            encoder.digest_value(standard.source().revision_hash());
            encoder.text(standard.language_version(), "standard language version")?;
            encode_standard_schemas(&mut encoder, standard.catalogue(), &origins)?;
            encode_value_types(
                &mut encoder,
                standard.catalogue().value_types(),
                Some(&origins),
            )?;
            if !standard.catalogue().enum_types().is_empty() {
                encode_enum_types(
                    &mut encoder,
                    standard.catalogue().enum_types(),
                    Some(&origins),
                )?;
            }
            encode_type_bindings(
                &mut encoder,
                standard.catalogue().type_bindings(),
                Some(&origins),
            )?;
            Ok(encoder.digest())
        }
        StandardLibraryDigestVersion::Version2 => {
            let (revisions, references) = standard_executable_facts(standard)?;
            let mut encoder = Encoder::new(STANDARD_LIBRARY_V2_DOMAIN);
            encoder.u32(StandardLibraryDigestVersion::Version2.to_u32());
            encoder.standard_library_revision_id(standard.revision());
            encoder.source_revision_id(standard.source().id());
            encoder.digest_value(standard.source().revision_hash());
            encoder.text(standard.language_version(), "standard language version")?;
            encode_catalogue_schemas(&mut encoder, standard.catalogue().schemas())?;
            encode_value_types(&mut encoder, standard.catalogue().value_types(), None)?;
            encode_enum_types(&mut encoder, standard.catalogue().enum_types(), None)?;
            encode_type_bindings(&mut encoder, standard.catalogue().type_bindings(), None)?;
            encode_catalogue_functions(&mut encoder, standard.catalogue())?;
            encode_current_function_revisions_with_contract(
                &mut encoder,
                &revisions,
                CurrentFunctionRevisionEncoding::Version2,
            )?;
            encode_definition_references(&mut encoder, &references)?;
            encode_definition_origins(&mut encoder, standard.origins())?;
            Ok(encoder.digest())
        }
    }
}

fn standard_executable_facts(
    standard: &StandardLibrarySnapshot,
) -> Result<(Vec<FunctionRevisionRecord>, Vec<DefinitionReference>), CanonicalHashError> {
    let revisions = standard
        .executables()
        .iter()
        .map(|executable| executable.revision().clone())
        .collect::<Vec<_>>();
    let references = standard
        .executables()
        .iter()
        .flat_map(|executable| executable.references().iter().cloned())
        .collect::<Vec<_>>();
    let expressions = HashMap::new();
    let revisions_by_function = current_function_revisions(standard.catalogue(), &revisions)?;
    validate_catalogue_references(
        standard.catalogue(),
        None,
        None,
        &expressions,
        &revisions_by_function,
        &references,
    )?;
    for function in standard.catalogue().functions() {
        let revision = revisions_by_function.get(&function.id()).ok_or(
            CanonicalHashError::MissingCurrentFunctionRevision {
                function: function.id(),
                revision: function.current_revision(),
            },
        )?;
        let function_references = references
            .iter()
            .filter(|reference| reference.source_function() == function.id())
            .cloned()
            .collect::<Vec<_>>();
        let semantic_hash = function_semantic_digest_with_version(
            revision.semantic_hash_version(),
            function,
            revision.language_version(),
            revision.artifact(),
            &[],
            &function_references,
        )?;
        if semantic_hash != revision.semantic_hash() {
            return Err(CanonicalHashError::FunctionSemanticHashMismatch {
                function: function.id(),
                revision: revision.id(),
            });
        }
    }
    Ok((revisions, references))
}

#[cfg(test)]
pub(crate) fn calculate_standard_library_digest_for_test(
    standard: &StandardLibrarySnapshot,
) -> Result<Sha256Digest, CanonicalHashError> {
    calculate_standard_library_digest(standard)
}

/// Hashes the resolved semantics of one function revision.
///
/// This intentionally excludes semantic names, source origins, and function
/// revision identities. It includes resolved identities, execution properties,
/// exact artifact descriptors, and resolved definition references.
pub fn function_semantic_digest(
    function: &FunctionDefinition,
    language_version: &str,
    artifact: &ExecutableArtifact,
    expressions: &[ExpressionArtifact],
    references: &[DefinitionReference],
) -> Result<Sha256Digest, CanonicalHashError> {
    function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version1,
        function,
        language_version,
        artifact,
        expressions,
        references,
    )
}

/// Hashes one function under an explicit semantic-hash contract.
pub fn function_semantic_digest_with_version(
    version: FunctionSemanticHashVersion,
    function: &FunctionDefinition,
    language_version: &str,
    artifact: &ExecutableArtifact,
    expressions: &[ExpressionArtifact],
    references: &[DefinitionReference],
) -> Result<Sha256Digest, CanonicalHashError> {
    let expressions = expression_artifacts_by_id(expressions)?;
    validate_executable_artifact(artifact, "function artifact")?;
    if executable_kind_for_domain(function.domain()) != artifact.kind() {
        return Err(CanonicalHashError::FunctionArtifactDomainMismatch {
            function: function.id(),
        });
    }

    let references = semantic_references(function, references)?;
    match version {
        FunctionSemanticHashVersion::Version1 => {
            for reference in &references {
                match reference.target() {
                    DefinitionReferenceTarget::ValueType(target) => {
                        return Err(
                            CanonicalHashError::FunctionFactUnsupportedBySemanticHashVersion {
                                version,
                                function: function.id(),
                                fact: FunctionSemanticHashFact::ValueTypeReference(target),
                            },
                        );
                    }
                    DefinitionReferenceTarget::ObjectType(_)
                    | DefinitionReferenceTarget::Field { .. }
                    | DefinitionReferenceTarget::Function(_)
                    | DefinitionReferenceTarget::Parameter { .. }
                    | DefinitionReferenceTarget::Expression(_) => {}
                }
            }
        }
        FunctionSemanticHashVersion::Version2 => {}
    }
    let domain = match version {
        FunctionSemanticHashVersion::Version1 => FUNCTION_SEMANTIC_DOMAIN,
        FunctionSemanticHashVersion::Version2 => FUNCTION_SEMANTIC_V2_DOMAIN,
    };
    let mut encoder = Encoder::new(domain);
    encoder.function_id(function.id());
    encoder.function_domain(function.domain());

    let parameters = sorted_by_key(function.parameters(), |parameter| parameter.ordinal());
    encoder.sequence_len(parameters.len(), "function parameters")?;
    for parameter in parameters {
        encode_parameter_semantics(&mut encoder, parameter, &expressions)?;
    }

    encode_function_return_semantics(&mut encoder, function.return_type())?;
    encoder.function_security(function.security());
    encoder.function_transaction(function.transaction());
    encoder.function_volatility(function.volatility());
    encoder.text(language_version, "function language version")?;
    encode_executable_artifact_descriptor(&mut encoder, artifact)?;

    encoder.sequence_len(references.len(), "function references")?;
    for reference in references {
        if !reference_kind_accepts_target(reference.kind(), reference.target()) {
            return Err(CanonicalHashError::ReferenceKindTargetMismatch {
                kind: reference.kind(),
                target: reference.target(),
            });
        }
        encoder.u32(reference.ordinal());
        encoder.reference_kind(reference.kind());
        encoder.reference_target(reference.target());
    }

    Ok(encoder.digest())
}

/// Hashes one complete current semantic catalogue view.
///
/// Callers must provide every current function revision, expression artifact,
/// source origin, and semantic reference. The function validates this coverage
/// before it writes canonical bytes.
pub fn catalogue_digest(
    catalogue: &CatalogueSnapshot,
    function_revisions: &[FunctionRevisionRecord],
    expressions: &[ExpressionArtifact],
    origins: &[DefinitionOrigin],
    references: &[DefinitionReference],
) -> Result<Sha256Digest, CanonicalHashError> {
    catalogue_digest_with_context(
        &CatalogueHashContext::version_one(),
        catalogue,
        function_revisions,
        expressions,
        origins,
        references,
    )
}

/// Hashes one complete catalogue under its closed version and standard context.
pub fn catalogue_digest_with_context(
    context: &CatalogueHashContext,
    catalogue: &CatalogueSnapshot,
    function_revisions: &[FunctionRevisionRecord],
    expressions: &[ExpressionArtifact],
    origins: &[DefinitionOrigin],
    references: &[DefinitionReference],
) -> Result<Sha256Digest, CanonicalHashError> {
    catalogue_digest_with_context_and_parent(
        context,
        catalogue,
        function_revisions,
        expressions,
        origins,
        references,
        None,
    )
}

/// Hashes one catalogue while using an existing parent catalogue only as a
/// validation view for references. The parent is deliberately not encoded in
/// the digest, so the candidate hash remains a function of candidate records.
pub fn catalogue_digest_with_context_and_parent(
    context: &CatalogueHashContext,
    catalogue: &CatalogueSnapshot,
    function_revisions: &[FunctionRevisionRecord],
    expressions: &[ExpressionArtifact],
    origins: &[DefinitionOrigin],
    references: &[DefinitionReference],
    parent: Option<&CatalogueSnapshot>,
) -> Result<Sha256Digest, CanonicalHashError> {
    if context.version() == CatalogueHashVersion::Version1 {
        reject_version_one_value_definitions(catalogue)?;
    }
    validate_resolved_type_slots(context, catalogue)?;
    validate_catalogue_version_facts(
        context.version(),
        catalogue,
        function_revisions,
        origins,
        references,
    )?;
    validate_record_value_field_types(context, catalogue)?;
    let expressions_by_id = expression_artifacts_by_id(expressions)?;
    validate_default_expression_coverage(catalogue, &expressions_by_id)?;
    let revisions_by_function = current_function_revisions(catalogue, function_revisions)?;
    validate_catalogue_references(
        catalogue,
        context
            .standard()
            .map(VerifiedStandardLibrarySnapshot::catalogue),
        parent,
        &expressions_by_id,
        &revisions_by_function,
        references,
    )?;
    complete_origins(catalogue, &expressions_by_id, origins)?;

    for function in catalogue.functions() {
        let revision = revisions_by_function.get(&function.id()).ok_or(
            CanonicalHashError::MissingCurrentFunctionRevision {
                function: function.id(),
                revision: function.current_revision(),
            },
        )?;
        let function_references = references
            .iter()
            .filter(|reference| reference.source_function() == function.id())
            .cloned()
            .collect::<Vec<_>>();
        let semantic_hash = function_semantic_digest_with_version(
            revision.semantic_hash_version(),
            function,
            revision.language_version(),
            revision.artifact(),
            expressions,
            &function_references,
        )?;
        if semantic_hash != revision.semantic_hash() {
            return Err(CanonicalHashError::FunctionSemanticHashMismatch {
                function: function.id(),
                revision: revision.id(),
            });
        }
    }

    match context {
        CatalogueHashContext::Version1 => {
            let mut encoder = Encoder::new(CATALOGUE_DOMAIN);
            encode_catalogue_schemas(&mut encoder, catalogue.schemas())?;
            encode_catalogue_object_types(&mut encoder, catalogue)?;
            encode_catalogue_functions(&mut encoder, catalogue)?;
            encode_expression_artifacts(&mut encoder, expressions)?;
            encode_current_function_revisions_with_contract(
                &mut encoder,
                function_revisions,
                CurrentFunctionRevisionEncoding::Version1,
            )?;
            encode_definition_origins(&mut encoder, origins)?;
            encode_definition_references(&mut encoder, references)?;
            Ok(encoder.digest())
        }
        CatalogueHashContext::Version2 { standard } => {
            let mut encoder = Encoder::new(CATALOGUE_V2_DOMAIN);
            encoder.u32(CatalogueHashVersion::Version2.to_u32());
            encoder.standard_library_revision_id(standard.revision());
            encoder.digest_value(standard.digest());
            encode_catalogue_schemas(&mut encoder, catalogue.schemas())?;
            encode_catalogue_object_types(&mut encoder, catalogue)?;
            encode_value_types(&mut encoder, catalogue.value_types(), None)?;
            if !catalogue.enum_types().is_empty() {
                encode_enum_types(&mut encoder, catalogue.enum_types(), None)?;
            }
            if !catalogue.record_value_types().is_empty() {
                encode_record_value_types(&mut encoder, catalogue, standard.catalogue())?;
            }
            encode_type_bindings(&mut encoder, catalogue.type_bindings(), None)?;
            encode_catalogue_functions(&mut encoder, catalogue)?;
            encode_expression_artifacts(&mut encoder, expressions)?;
            encode_current_function_revisions_with_contract(
                &mut encoder,
                function_revisions,
                CurrentFunctionRevisionEncoding::Version2,
            )?;
            encode_definition_origins(&mut encoder, origins)?;
            encode_definition_references(&mut encoder, references)?;
            Ok(encoder.digest())
        }
    }
}

fn validate_resolved_type_slots(
    context: &CatalogueHashContext,
    catalogue: &CatalogueSnapshot,
) -> Result<(), CanonicalHashError> {
    for object_type in catalogue.object_types() {
        for field in object_type.fields() {
            validate_resolved_type_slot(
                context,
                DefinitionIdentity::Field {
                    owner: object_type.id(),
                    field: field.id(),
                },
                field.resolved_type(),
                false,
            )?;
        }
    }
    for function in catalogue.functions() {
        for parameter in function.parameters() {
            validate_resolved_type_slot(
                context,
                DefinitionIdentity::Parameter {
                    owner: function.id(),
                    parameter: parameter.id(),
                },
                parameter.resolved_type(),
                false,
            )?;
        }
        match function.return_type() {
            FunctionReturn::Single(resolved_type) | FunctionReturn::Stream(resolved_type) => {
                validate_resolved_type_slot(
                    context,
                    DefinitionIdentity::Function(function.id()),
                    *resolved_type,
                    function_accepts_opaque_client_return(function),
                )?
            }
            FunctionReturn::Rows(columns) => {
                for column in columns {
                    validate_resolved_type_slot(
                        context,
                        DefinitionIdentity::FunctionReturnColumn {
                            owner: function.id(),
                            ordinal: column.ordinal(),
                        },
                        column.resolved_type(),
                        false,
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn validate_resolved_type_slot(
    context: &CatalogueHashContext,
    identity: DefinitionIdentity,
    resolved_type: ResolvedType,
    opaque_accepted: bool,
) -> Result<(), CanonicalHashError> {
    if let Some(scalar) = resolved_type.legacy_scalar() {
        if context.version() == CatalogueHashVersion::Version2 {
            return Err(
                CanonicalHashError::LegacyScalarRequiresCatalogueHashVersionOne {
                    identity,
                    scalar,
                },
            );
        }
        return Ok(());
    }
    let Some(value_type) = resolved_type.value_type() else {
        return Ok(());
    };
    if context.version() == CatalogueHashVersion::Version1 {
        return Err(
            CanonicalHashError::ResolvedValueRequiresCatalogueHashVersionTwo {
                identity,
                value_type,
            },
        );
    }
    if is_sealed_inspect_type_id(value_type) {
        return Ok(());
    }
    let Some(value_type_definition) = context
        .standard()
        .and_then(|standard| standard.catalogue().value_type_by_id(value_type))
    else {
        return Err(CanonicalHashError::ResolvedValueTypeNotInPinnedStandard {
            identity,
            value_type,
        });
    };
    if value_type_definition.kind() == ValueTypeKind::Opaque && !opaque_accepted {
        return Err(CanonicalHashError::OpaqueValueTypeNotAcceptedInSlot {
            identity,
            value_type,
        });
    }
    Ok(())
}

fn reject_version_one_value_definitions(
    catalogue: &CatalogueSnapshot,
) -> Result<(), CanonicalHashError> {
    if let Some(record_value_type) = catalogue.record_value_types().first() {
        return Err(CanonicalHashError::CatalogueFactUnsupportedByHashVersion {
            version: CatalogueHashVersion::Version1,
            fact: CatalogueHashFact::RecordValueTypeDefinition(record_value_type.id()),
        });
    }
    if let Some(opaque) = catalogue
        .value_types()
        .iter()
        .find(|value_type| value_type.kind() == ValueTypeKind::Opaque)
    {
        return Err(CanonicalHashError::CatalogueFactUnsupportedByHashVersion {
            version: CatalogueHashVersion::Version1,
            fact: CatalogueHashFact::ValueTypeDefinition(opaque.id()),
        });
    }
    Ok(())
}

fn validate_record_value_field_types(
    context: &CatalogueHashContext,
    catalogue: &CatalogueSnapshot,
) -> Result<(), CanonicalHashError> {
    let CatalogueHashContext::Version2 { standard } = context else {
        return Ok(());
    };
    validate_record_value_field_descriptors(catalogue, standard.catalogue()).map_err(|error| {
        match error {
            RecordValueFieldDescriptorValidationError::Unsupported {
                record_value_type,
                field,
                descriptor,
            } => CanonicalHashError::UnsupportedRecordValueFieldType {
                record_value_type,
                field,
                descriptor,
            },
            RecordValueFieldDescriptorValidationError::Ambiguous {
                record_value_type,
                field,
                type_id,
            } => CanonicalHashError::AmbiguousRecordValueFieldType {
                record_value_type,
                field,
                type_id,
            },
            RecordValueFieldDescriptorValidationError::RecursiveRecordValueField {
                record_value_type,
                field,
                nested_record_value_type,
            } => CanonicalHashError::RecursiveRecordValueField {
                record_value_type,
                field,
                nested_record_value_type,
            },
            RecordValueFieldDescriptorValidationError::RecordValueNestingTooDeep {
                record_value_type,
                field,
                nested_record_value_type,
                maximum,
                actual,
            } => CanonicalHashError::RecordValueNestingTooDeep {
                record_value_type,
                field,
                nested_record_value_type,
                maximum,
                actual,
            },
        }
    })
}

fn canonical_record_value_field_type(
    catalogue: &CatalogueSnapshot,
    standard: &CatalogueSnapshot,
    record_value_type: TypeId,
    field: FieldId,
    descriptor: &TypeDescriptor,
) -> Result<RecordValueFieldDescriptorClass, CanonicalHashError> {
    classify_record_value_field_descriptor(catalogue, standard, descriptor).map_err(|error| {
        match error {
            RecordValueFieldDescriptorClassificationError::Unsupported => {
                CanonicalHashError::UnsupportedRecordValueFieldType {
                    record_value_type,
                    field,
                    descriptor: descriptor.clone(),
                }
            }
            RecordValueFieldDescriptorClassificationError::Ambiguous { type_id } => {
                CanonicalHashError::AmbiguousRecordValueFieldType {
                    record_value_type,
                    field,
                    type_id,
                }
            }
        }
    })
}

fn validate_catalogue_version_facts(
    version: CatalogueHashVersion,
    catalogue: &CatalogueSnapshot,
    function_revisions: &[FunctionRevisionRecord],
    origins: &[DefinitionOrigin],
    references: &[DefinitionReference],
) -> Result<(), CanonicalHashError> {
    match version {
        CatalogueHashVersion::Version1 => {
            if let Some(enum_type) = catalogue.enum_types().first() {
                return Err(CanonicalHashError::CatalogueFactUnsupportedByHashVersion {
                    version,
                    fact: CatalogueHashFact::EnumTypeDefinition(enum_type.id()),
                });
            }
            if let Some(value_type) = catalogue.value_types().first() {
                return Err(CanonicalHashError::CatalogueFactUnsupportedByHashVersion {
                    version,
                    fact: CatalogueHashFact::ValueTypeDefinition(value_type.id()),
                });
            }
            if let Some(binding) = catalogue.type_bindings().first() {
                return Err(CanonicalHashError::CatalogueFactUnsupportedByHashVersion {
                    version,
                    fact: CatalogueHashFact::TypeBinding(binding.id()),
                });
            }
            for origin in origins {
                match origin.identity() {
                    DefinitionIdentity::ValueType(_) | DefinitionIdentity::TypeBinding(_) => {
                        return Err(CanonicalHashError::CatalogueFactUnsupportedByHashVersion {
                            version,
                            fact: CatalogueHashFact::DefinitionOrigin(origin.identity()),
                        });
                    }
                    DefinitionIdentity::Schema(_)
                    | DefinitionIdentity::ObjectType(_)
                    | DefinitionIdentity::Field { .. }
                    | DefinitionIdentity::Function(_)
                    | DefinitionIdentity::Parameter { .. }
                    | DefinitionIdentity::FunctionReturnColumn { .. }
                    | DefinitionIdentity::Expression(_) => {}
                }
            }
            for reference in references {
                match reference.target() {
                    DefinitionReferenceTarget::ValueType(_) => {
                        return Err(CanonicalHashError::CatalogueFactUnsupportedByHashVersion {
                            version,
                            fact: CatalogueHashFact::DefinitionReferenceTarget(reference.target()),
                        });
                    }
                    DefinitionReferenceTarget::ObjectType(_)
                    | DefinitionReferenceTarget::Field { .. }
                    | DefinitionReferenceTarget::Function(_)
                    | DefinitionReferenceTarget::Parameter { .. }
                    | DefinitionReferenceTarget::Expression(_) => {}
                }
            }
            for revision in function_revisions {
                match revision.semantic_hash_version() {
                    FunctionSemanticHashVersion::Version1 => {}
                    FunctionSemanticHashVersion::Version2 => {
                        return Err(CanonicalHashError::CatalogueFactUnsupportedByHashVersion {
                            version,
                            fact: CatalogueHashFact::FunctionSemanticHashVersion {
                                function: revision.function(),
                                revision: revision.id(),
                                version: revision.semantic_hash_version(),
                            },
                        });
                    }
                }
            }
        }
        CatalogueHashVersion::Version2 => {}
    }
    Ok(())
}

fn encode_catalogue_schemas(
    encoder: &mut Encoder,
    schemas: &[SchemaDefinition],
) -> Result<(), CanonicalHashError> {
    let schemas = sorted_by_key(schemas, |schema| schema.id().to_bytes());
    encoder.sequence_len(schemas.len(), "catalogue schemas")?;
    for schema in schemas {
        encoder.schema_id(schema.id());
        encoder.semantic_name(schema.name())?;
    }
    Ok(())
}

fn encode_standard_schemas(
    encoder: &mut Encoder,
    catalogue: &CatalogueSnapshot,
    origins: &HashMap<DefinitionIdentity, &DefinitionOrigin>,
) -> Result<(), CanonicalHashError> {
    let schemas = sorted_by_key(catalogue.schemas(), |schema| schema.id().to_bytes());
    encoder.sequence_len(schemas.len(), "standard schemas")?;
    for schema in schemas {
        encoder.schema_id(schema.id());
        encoder.semantic_name(schema.name())?;
        encode_required_origin(encoder, origins, DefinitionIdentity::Schema(schema.id()))?;
    }
    Ok(())
}

fn encode_value_types(
    encoder: &mut Encoder,
    value_types: &[ValueTypeDefinition],
    origins: Option<&HashMap<DefinitionIdentity, &DefinitionOrigin>>,
) -> Result<(), CanonicalHashError> {
    let value_types = sorted_by_key(value_types, |value_type| value_type.id().to_bytes());
    encoder.sequence_len(value_types.len(), "catalogue value types")?;
    for value_type in value_types {
        encoder.type_id(value_type.id());
        encoder.semantic_name(value_type.name())?;
        encoder.u8(match value_type.kind() {
            ValueTypeKind::Primitive => 1,
            ValueTypeKind::Opaque => 2,
        });
        encoder.u8(match value_type.mutability() {
            ValueTypeMutability::Immutable => 1,
        });
        encoder.u8(match value_type.persistence() {
            ValueTypePersistence::Persistable => 1,
            ValueTypePersistence::Transient => 2,
        });
        encoder.text(
            value_type.representation_contract(),
            "value type representation contract",
        )?;
        if let Some(origins) = origins {
            encode_required_origin(
                encoder,
                origins,
                DefinitionIdentity::ValueType(value_type.id()),
            )?;
        }
    }
    Ok(())
}

fn encode_enum_types(
    encoder: &mut Encoder,
    enum_types: &[EnumTypeDefinition],
    origins: Option<&HashMap<DefinitionIdentity, &DefinitionOrigin>>,
) -> Result<(), CanonicalHashError> {
    let enum_types = sorted_by_key(enum_types, |enum_type| enum_type.id().to_bytes());
    encoder.sequence_len(enum_types.len(), "catalogue enum types")?;
    for enum_type in enum_types {
        encoder.type_id(enum_type.id());
        encoder.semantic_name(enum_type.name())?;
        encoder.sequence_len(enum_type.labels().len(), "enum labels")?;
        for label in enum_type.labels() {
            encoder.text(label, "enum label")?;
        }
        if let Some(origins) = origins {
            encode_required_origin(
                encoder,
                origins,
                DefinitionIdentity::ValueType(enum_type.id()),
            )?;
        }
    }
    Ok(())
}

fn encode_record_value_types(
    encoder: &mut Encoder,
    catalogue: &CatalogueSnapshot,
    standard: &CatalogueSnapshot,
) -> Result<(), CanonicalHashError> {
    let record_value_types = sorted_by_key(catalogue.record_value_types(), |record_value_type| {
        record_value_type.id().to_bytes()
    });
    encoder.sequence_len(record_value_types.len(), "catalogue record value types")?;
    for record_value_type in record_value_types {
        encoder.type_id(record_value_type.id());
        encoder.semantic_name(record_value_type.name())?;
        encoder.u8(2);
        encoder.u8(1);
        encoder.u8(1);
        encoder.sequence_len(record_value_type.fields().len(), "record value fields")?;
        let fields = sorted_by_key(record_value_type.fields(), |field| field.ordinal());
        for field in fields {
            encoder.field_id(field.id());
            encoder.text(field.name(), "record value field name")?;
            encoder.u32(field.ordinal());
            match canonical_record_value_field_type(
                catalogue,
                standard,
                record_value_type.id(),
                field.id(),
                field.descriptor(),
            )? {
                RecordValueFieldDescriptorClass::ApplicationEnum(type_id)
                | RecordValueFieldDescriptorClass::ApplicationRecord(type_id)
                | RecordValueFieldDescriptorClass::StandardEnum(type_id) => {
                    encoder.u8(2);
                    encoder.type_id(type_id);
                }
                RecordValueFieldDescriptorClass::StandardPrimitive(type_id) => {
                    encoder.u8(4);
                    encoder.type_id(type_id);
                }
                RecordValueFieldDescriptorClass::SealedSourceMetadata => {
                    return Err(CanonicalHashError::UnsupportedRecordValueFieldType {
                        record_value_type: record_value_type.id(),
                        field: field.id(),
                        descriptor: field.descriptor().clone(),
                    });
                }
            }
        }
    }
    Ok(())
}

fn encode_type_bindings(
    encoder: &mut Encoder,
    bindings: &[TypeBinding],
    origins: Option<&HashMap<DefinitionIdentity, &DefinitionOrigin>>,
) -> Result<(), CanonicalHashError> {
    let bindings = sorted_by_key(bindings, |binding| binding.id().to_bytes());
    encoder.sequence_len(bindings.len(), "catalogue type bindings")?;
    for binding in bindings {
        encoder.type_binding_id(binding.id());
        encoder.u8(match binding.kind() {
            TypeBindingKind::Qualified => 1,
            TypeBindingKind::Prelude => 2,
        });
        match binding.name() {
            TypeLookupName::Qualified(name) => encoder.semantic_name(name)?,
            TypeLookupName::Prelude(name) => encoder.prelude_name(name)?,
        }
        encoder.type_id(binding.target());
        if let Some(origins) = origins {
            encode_required_origin(
                encoder,
                origins,
                DefinitionIdentity::TypeBinding(binding.id()),
            )?;
        }
    }
    Ok(())
}

fn encode_required_origin(
    encoder: &mut Encoder,
    origins: &HashMap<DefinitionIdentity, &DefinitionOrigin>,
    identity: DefinitionIdentity,
) -> Result<(), CanonicalHashError> {
    let origin = origins
        .get(&identity)
        .ok_or(CanonicalHashError::MissingDefinitionOrigin { identity })?;
    encoder.source_origin(origin.source());
    Ok(())
}

fn encode_catalogue_object_types(
    encoder: &mut Encoder,
    catalogue: &CatalogueSnapshot,
) -> Result<(), CanonicalHashError> {
    let object_types = sorted_by_key(catalogue.object_types(), |object_type| {
        object_type.id().to_bytes()
    });
    encoder.sequence_len(object_types.len(), "catalogue object types")?;
    for object_type in object_types {
        encoder.type_id(object_type.id());
        encoder.schema_id(object_type_owner_schema(catalogue, object_type)?);
        encoder.semantic_name(object_type.name())?;

        let fields = sorted_by_key(object_type.fields(), |field| field.ordinal());
        encoder.sequence_len(fields.len(), "object fields")?;
        for field in fields {
            encoder.field_id(field.id());
            encoder.text(field.name(), "field name")?;
            encoder.u32(field.ordinal());
            encode_resolved_type(encoder, field.resolved_type());
            encoder.boolean(field.nullable());
            encoder.boolean(field.unique());
            encoder.option_expression_id(field.default_expression());
            encoder.on_delete(field.on_delete());
        }
    }
    Ok(())
}

fn encode_catalogue_functions(
    encoder: &mut Encoder,
    catalogue: &CatalogueSnapshot,
) -> Result<(), CanonicalHashError> {
    let functions = sorted_by_key(catalogue.functions(), |function| function.id().to_bytes());
    encoder.sequence_len(functions.len(), "catalogue functions")?;
    for function in functions {
        encoder.function_id(function.id());
        encoder.schema_id(function_owner_schema(catalogue, function)?);
        encoder.semantic_name(function.name())?;
        encoder.function_domain(function.domain());
        encoder.function_security(function.security());
        encoder.function_transaction(function.transaction());
        encoder.function_volatility(function.volatility());

        let parameters = sorted_by_key(function.parameters(), |parameter| parameter.ordinal());
        encoder.sequence_len(parameters.len(), "function parameters")?;
        for parameter in parameters {
            encoder.parameter_id(parameter.id());
            encoder.text(parameter.name(), "parameter name")?;
            encoder.u32(parameter.ordinal());
            encode_resolved_type(encoder, parameter.resolved_type());
            encoder.option_expression_id(parameter.default_expression());
        }

        encode_function_return_catalogue(encoder, function.return_type())?;
        encoder.function_revision_id(function.current_revision());
    }
    Ok(())
}

fn encode_function_return_catalogue(
    encoder: &mut Encoder,
    function_return: &FunctionReturn,
) -> Result<(), CanonicalHashError> {
    encode_function_return(encoder, function_return, ReturnColumnNames::Include)
}

#[derive(Clone, Copy)]
enum CurrentFunctionRevisionEncoding {
    Version1,
    Version2,
}

fn encode_current_function_revisions_with_contract(
    encoder: &mut Encoder,
    function_revisions: &[FunctionRevisionRecord],
    encoding: CurrentFunctionRevisionEncoding,
) -> Result<(), CanonicalHashError> {
    let revisions = sorted_by_key(function_revisions, |revision| {
        revision.function().to_bytes()
    });
    encoder.sequence_len(revisions.len(), "current function revisions")?;
    for revision in revisions {
        encoder.function_id(revision.function());
        encoder.function_revision_id(revision.id());
        encoder.u64(revision.revision_number());
        encoder.source_origin(revision.declaration_origin());
        encoder.digest_value(revision.declaration_content_hash());
        encoder.digest_value(revision.semantic_hash());
        match encoding {
            CurrentFunctionRevisionEncoding::Version1 => {}
            CurrentFunctionRevisionEncoding::Version2 => {
                encoder.u32(revision.semantic_hash_version().to_u32());
            }
        }
        encoder.text(revision.language_version(), "function language version")?;
        encode_executable_artifact_descriptor(encoder, revision.artifact())?;
    }
    Ok(())
}

fn encode_expression_artifacts(
    encoder: &mut Encoder,
    expressions: &[ExpressionArtifact],
) -> Result<(), CanonicalHashError> {
    let expressions = sorted_by_key(expressions, |expression| expression.id().to_bytes());
    encoder.sequence_len(expressions.len(), "expression artifacts")?;
    for expression in expressions {
        encoder.expression_id(expression.id());
        encode_expression_artifact_descriptor(encoder, expression)?;
    }
    Ok(())
}

fn encode_definition_origins(
    encoder: &mut Encoder,
    origins: &[DefinitionOrigin],
) -> Result<(), CanonicalHashError> {
    let origins = sorted_by_key(origins, |origin| {
        definition_identity_sort_key(origin.identity())
    });
    encoder.sequence_len(origins.len(), "definition origins")?;
    for origin in origins {
        encoder.definition_identity(origin.identity());
        encoder.source_origin(origin.source());
    }
    Ok(())
}

fn encode_definition_references(
    encoder: &mut Encoder,
    references: &[DefinitionReference],
) -> Result<(), CanonicalHashError> {
    let references = sorted_by_key(references, reference_sort_key);
    encoder.sequence_len(references.len(), "definition references")?;
    for reference in references {
        encoder.function_id(reference.source_function());
        encoder.function_revision_id(reference.source_revision());
        encoder.u32(reference.ordinal());
        encoder.reference_kind(reference.kind());
        encoder.reference_target(reference.target());
        encoder.source_origin(reference.source_origin());
    }
    Ok(())
}

fn encode_parameter_semantics(
    encoder: &mut Encoder,
    parameter: &ParameterDefinition,
    expressions: &HashMap<ExpressionId, &ExpressionArtifact>,
) -> Result<(), CanonicalHashError> {
    encoder.parameter_id(parameter.id());
    encoder.u32(parameter.ordinal());
    encode_resolved_type(encoder, parameter.resolved_type());
    encode_optional_expression_artifact_descriptor(
        encoder,
        parameter.default_expression(),
        expressions,
    )
}

fn encode_function_return_semantics(
    encoder: &mut Encoder,
    function_return: &FunctionReturn,
) -> Result<(), CanonicalHashError> {
    encode_function_return(encoder, function_return, ReturnColumnNames::Exclude)
}

#[derive(Clone, Copy)]
enum ReturnColumnNames {
    Include,
    Exclude,
}

fn encode_function_return(
    encoder: &mut Encoder,
    function_return: &FunctionReturn,
    column_names: ReturnColumnNames,
) -> Result<(), CanonicalHashError> {
    match function_return {
        FunctionReturn::Single(resolved_type) => {
            encoder.u8(1);
            encode_resolved_type(encoder, *resolved_type);
        }
        FunctionReturn::Stream(resolved_type) => {
            encoder.u8(3);
            encode_resolved_type(encoder, *resolved_type);
        }
        FunctionReturn::Rows(columns) => {
            encoder.u8(2);
            let columns = sorted_by_key(columns, |column| column.ordinal());
            encoder.sequence_len(columns.len(), "function return columns")?;
            for column in columns {
                if matches!(column_names, ReturnColumnNames::Include) {
                    encoder.text(column.name(), "function return column name")?;
                }
                encoder.u32(column.ordinal());
                encode_resolved_type(encoder, column.resolved_type());
            }
        }
    }
    Ok(())
}

fn encode_optional_expression_artifact_descriptor(
    encoder: &mut Encoder,
    expression_id: Option<ExpressionId>,
    expressions: &HashMap<ExpressionId, &ExpressionArtifact>,
) -> Result<(), CanonicalHashError> {
    match expression_id {
        None => encoder.option_none(),
        Some(expression_id) => {
            let expression = expressions.get(&expression_id).ok_or(
                CanonicalHashError::DefaultExpressionArtifactNotFound {
                    expression: expression_id,
                },
            )?;
            encoder.option_some();
            encoder.expression_id(expression_id);
            encode_expression_artifact_descriptor(encoder, expression)?;
        }
    }
    Ok(())
}

fn encode_expression_artifact_descriptor(
    encoder: &mut Encoder,
    artifact: &ExpressionArtifact,
) -> Result<(), CanonicalHashError> {
    validate_expression_artifact(artifact)?;
    encoder.text(artifact.format(), "expression artifact format")?;
    encoder.u32(artifact.version());
    encoder.digest_value(artifact.content_hash());
    Ok(())
}

fn encode_executable_artifact_descriptor(
    encoder: &mut Encoder,
    artifact: &ExecutableArtifact,
) -> Result<(), CanonicalHashError> {
    validate_executable_artifact(artifact, "function artifact")?;
    encoder.executable_artifact_kind(artifact.kind());
    encoder.text(artifact.format(), "function artifact format")?;
    encoder.u32(artifact.version());
    encoder.digest_value(artifact.content_hash());
    Ok(())
}

fn validate_expression_artifact(artifact: &ExpressionArtifact) -> Result<(), CanonicalHashError> {
    if artifact_payload_digest(artifact.payload())? != artifact.content_hash() {
        return Err(CanonicalHashError::ArtifactPayloadHashMismatch {
            artifact: "expression artifact",
        });
    }
    Ok(())
}

fn validate_executable_artifact(
    artifact: &ExecutableArtifact,
    label: &'static str,
) -> Result<(), CanonicalHashError> {
    if artifact_payload_digest(artifact.payload())? != artifact.content_hash() {
        return Err(CanonicalHashError::ArtifactPayloadHashMismatch { artifact: label });
    }
    Ok(())
}

fn expression_artifacts_by_id(
    expressions: &[ExpressionArtifact],
) -> Result<HashMap<ExpressionId, &ExpressionArtifact>, CanonicalHashError> {
    let mut by_id = HashMap::with_capacity(expressions.len());
    for expression in expressions {
        validate_expression_artifact(expression)?;
        if by_id.insert(expression.id(), expression).is_some() {
            return Err(CanonicalHashError::DuplicateExpressionArtifact {
                expression: expression.id(),
            });
        }
    }
    Ok(by_id)
}

fn validate_default_expression_coverage(
    catalogue: &CatalogueSnapshot,
    expressions: &HashMap<ExpressionId, &ExpressionArtifact>,
) -> Result<(), CanonicalHashError> {
    for object_type in catalogue.object_types() {
        for field in object_type.fields() {
            if let Some(expression) = field.default_expression()
                && !expressions.contains_key(&expression)
            {
                return Err(CanonicalHashError::DefaultExpressionArtifactNotFound { expression });
            }
        }
    }
    for function in catalogue.functions() {
        for parameter in function.parameters() {
            if let Some(expression) = parameter.default_expression()
                && !expressions.contains_key(&expression)
            {
                return Err(CanonicalHashError::DefaultExpressionArtifactNotFound { expression });
            }
        }
    }
    Ok(())
}

fn current_function_revisions<'a>(
    catalogue: &CatalogueSnapshot,
    function_revisions: &'a [FunctionRevisionRecord],
) -> Result<HashMap<FunctionId, &'a FunctionRevisionRecord>, CanonicalHashError> {
    let mut by_function = HashMap::with_capacity(function_revisions.len());
    let mut revision_ids = HashSet::with_capacity(function_revisions.len());
    for revision in function_revisions {
        if !revision_ids.insert(revision.id()) {
            return Err(CanonicalHashError::DuplicateFunctionRevisionId {
                revision: revision.id(),
            });
        }
        let function = catalogue.function_by_id(revision.function()).ok_or(
            CanonicalHashError::FunctionRevisionFunctionNotFound {
                function: revision.function(),
                revision: revision.id(),
            },
        )?;
        if by_function.insert(revision.function(), revision).is_some() {
            return Err(CanonicalHashError::DuplicateFunctionRevision {
                function: revision.function(),
            });
        }
        if revision.id() != function.current_revision() {
            return Err(CanonicalHashError::FunctionRevisionIsNotCurrent {
                function: function.id(),
                expected: function.current_revision(),
                actual: revision.id(),
            });
        }
        if executable_kind_for_domain(function.domain()) != revision.artifact().kind() {
            return Err(CanonicalHashError::FunctionArtifactDomainMismatch {
                function: function.id(),
            });
        }
        validate_executable_artifact(revision.artifact(), "function artifact")?;
    }

    for function in catalogue.functions() {
        if !by_function.contains_key(&function.id()) {
            return Err(CanonicalHashError::MissingCurrentFunctionRevision {
                function: function.id(),
                revision: function.current_revision(),
            });
        }
    }
    Ok(by_function)
}

fn validate_catalogue_references(
    catalogue: &CatalogueSnapshot,
    standard: Option<&CatalogueSnapshot>,
    parent: Option<&CatalogueSnapshot>,
    expressions: &HashMap<ExpressionId, &ExpressionArtifact>,
    revisions: &HashMap<FunctionId, &FunctionRevisionRecord>,
    references: &[DefinitionReference],
) -> Result<(), CanonicalHashError> {
    let mut ordinals = HashMap::with_capacity(references.len());
    for reference in references {
        let revision = revisions.get(&reference.source_function()).ok_or(
            CanonicalHashError::ReferenceRevisionIsNotCurrent {
                function: reference.source_function(),
                revision: reference.source_revision(),
            },
        )?;
        if revision.id() != reference.source_revision() {
            return Err(CanonicalHashError::ReferenceRevisionIsNotCurrent {
                function: reference.source_function(),
                revision: reference.source_revision(),
            });
        }
        if ordinals
            .insert((reference.source_function(), reference.ordinal()), ())
            .is_some()
        {
            return Err(CanonicalHashError::DuplicateReferenceOrdinal {
                function: reference.source_function(),
                ordinal: reference.ordinal(),
            });
        }
        if !reference_target_exists(catalogue, standard, parent, expressions, reference.target()) {
            return Err(CanonicalHashError::ReferenceTargetNotFound {
                target: reference.target(),
            });
        }
        if !reference_kind_accepts_target(reference.kind(), reference.target()) {
            return Err(CanonicalHashError::ReferenceKindTargetMismatch {
                kind: reference.kind(),
                target: reference.target(),
            });
        }
    }
    Ok(())
}

fn complete_origins<'a>(
    catalogue: &CatalogueSnapshot,
    expressions: &HashMap<ExpressionId, &ExpressionArtifact>,
    origins: &'a [DefinitionOrigin],
) -> Result<HashMap<DefinitionIdentity, &'a DefinitionOrigin>, CanonicalHashError> {
    let mut by_identity = HashMap::with_capacity(origins.len());
    for origin in origins {
        if !definition_identity_exists(catalogue, expressions, origin.identity()) {
            return Err(CanonicalHashError::OriginDefinitionNotFound {
                identity: origin.identity(),
            });
        }
        if by_identity.insert(origin.identity(), origin).is_some() {
            return Err(CanonicalHashError::DuplicateDefinitionOrigin {
                identity: origin.identity(),
            });
        }
    }

    for identity in catalogue_definition_identities(catalogue, expressions) {
        if !by_identity.contains_key(&identity) {
            return Err(CanonicalHashError::MissingDefinitionOrigin { identity });
        }
    }
    Ok(by_identity)
}

fn semantic_references<'a>(
    function: &FunctionDefinition,
    references: &'a [DefinitionReference],
) -> Result<Vec<&'a DefinitionReference>, CanonicalHashError> {
    let mut sorted: Vec<&DefinitionReference> = Vec::with_capacity(references.len());
    let mut ordinals = HashMap::new();
    for reference in references.iter() {
        if reference.source_function() != function.id() {
            return Err(CanonicalHashError::ReferenceSourceFunctionMismatch {
                expected: function.id(),
                actual: reference.source_function(),
            });
        }
        if ordinals.insert(reference.ordinal(), ()).is_some() {
            return Err(CanonicalHashError::DuplicateReferenceOrdinal {
                function: function.id(),
                ordinal: reference.ordinal(),
            });
        }
        sorted.push(reference);
    }
    sorted.sort_by_key(|reference| reference.ordinal());
    Ok(sorted)
}

fn is_sealed_inspect_type_id(type_id: TypeId) -> bool {
    matches!(
        type_id,
        crate::system::SYS_INSPECT_INVOCATION_TYPE_ID
            | crate::system::SYS_INSPECT_SNAPSHOT_TYPE_ID
            | crate::system::SYS_INSPECT_SNAPSHOT_OPTIONS_TYPE_ID
            | crate::system::SYS_INSPECT_TRACE_EVENT_TYPE_ID
            | crate::system::SYS_INSPECT_INVOCATION_NODES_TYPE_ID
            | crate::system::SYS_INSPECT_CALLS_TYPE_ID
            | crate::system::SYS_INSPECT_RESOURCES_TYPE_ID
            | crate::system::SYS_INSPECT_STATE_CELLS_TYPE_ID
            | crate::system::SYS_INSPECT_UI_NODES_TYPE_ID
            | crate::system::SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID
            | crate::system::SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID
            | crate::system::SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID
    )
}

fn reference_target_exists(
    catalogue: &CatalogueSnapshot,
    standard: Option<&CatalogueSnapshot>,
    parent: Option<&CatalogueSnapshot>,
    expressions: &HashMap<ExpressionId, &ExpressionArtifact>,
    target: DefinitionReferenceTarget,
) -> bool {
    if matches!(
        target,
        DefinitionReferenceTarget::ValueType(value_type)
            | DefinitionReferenceTarget::ObjectType(value_type)
            if is_sealed_inspect_type_id(value_type)
                || value_type == crate::system::SYS_SOURCE_FUNCTION_TYPE_ID
    ) {
        return true;
    }
    if let DefinitionReferenceTarget::Field { owner, field } = target {
        return catalogue
            .object_type_by_id(owner)
            .is_some_and(|object_type| object_type.field_by_id(field).is_some())
            || catalogue
                .record_value_type_by_id(owner)
                .is_some_and(|record_value_type| record_value_type.field_by_id(field).is_some())
            || standard.is_some_and(|standard| {
                standard
                    .object_type_by_id(owner)
                    .is_some_and(|object_type| object_type.field_by_id(field).is_some())
                    || standard
                        .record_value_type_by_id(owner)
                        .is_some_and(|record_value_type| {
                            record_value_type.field_by_id(field).is_some()
                        })
            })
            || parent.is_some_and(|parent| {
                parent
                    .object_type_by_id(owner)
                    .is_some_and(|object_type| object_type.field_by_id(field).is_some())
                    || parent
                        .record_value_type_by_id(owner)
                        .is_some_and(|record_value_type| {
                            record_value_type.field_by_id(field).is_some()
                        })
            });
    }
    let identity = target.into();
    definition_identity_exists(catalogue, expressions, identity)
        || standard
            .is_some_and(|standard| definition_identity_exists(standard, expressions, identity))
        || parent.is_some_and(|parent| definition_identity_exists(parent, expressions, identity))
}

fn definition_identity_exists(
    catalogue: &CatalogueSnapshot,
    expressions: &HashMap<ExpressionId, &ExpressionArtifact>,
    identity: DefinitionIdentity,
) -> bool {
    match identity {
        DefinitionIdentity::Schema(schema) => catalogue.schema_by_id(schema).is_some(),
        DefinitionIdentity::ObjectType(object_type) => {
            catalogue.object_type_by_id(object_type).is_some()
        }
        DefinitionIdentity::ValueType(value_type) => {
            catalogue.value_type_by_id(value_type).is_some()
                || catalogue.enum_type_by_id(value_type).is_some()
                || catalogue.record_value_type_by_id(value_type).is_some()
        }
        DefinitionIdentity::TypeBinding(binding) => catalogue.type_binding_by_id(binding).is_some(),
        DefinitionIdentity::Field { owner, field } => {
            catalogue
                .object_type_by_id(owner)
                .is_some_and(|object_type| object_type.field_by_id(field).is_some())
                || catalogue
                    .record_value_type_by_id(owner)
                    .is_some_and(|record_value_type| record_value_type.field_by_id(field).is_some())
        }
        DefinitionIdentity::Function(function) => catalogue.function_by_id(function).is_some(),
        DefinitionIdentity::Parameter { owner, parameter } => catalogue
            .function_by_id(owner)
            .is_some_and(|function| function.parameter_by_id(parameter).is_some()),
        DefinitionIdentity::FunctionReturnColumn { owner, ordinal } => catalogue
            .function_by_id(owner)
            .is_some_and(|function| match function.return_type() {
                FunctionReturn::Single(_) | FunctionReturn::Stream(_) => false,
                FunctionReturn::Rows(columns) => {
                    columns.iter().any(|column| column.ordinal() == ordinal)
                }
            }),
        DefinitionIdentity::Expression(expression) => expressions.contains_key(&expression),
    }
}

fn catalogue_definition_identities(
    catalogue: &CatalogueSnapshot,
    expressions: &HashMap<ExpressionId, &ExpressionArtifact>,
) -> Vec<DefinitionIdentity> {
    let mut identities = Vec::new();
    for schema in catalogue.schemas() {
        identities.push(DefinitionIdentity::Schema(schema.id()));
    }
    for object_type in catalogue.object_types() {
        identities.push(DefinitionIdentity::ObjectType(object_type.id()));
        for field in object_type.fields() {
            identities.push(DefinitionIdentity::Field {
                owner: object_type.id(),
                field: field.id(),
            });
        }
    }
    for value_type in catalogue.value_types() {
        identities.push(DefinitionIdentity::ValueType(value_type.id()));
    }
    for enum_type in catalogue.enum_types() {
        identities.push(DefinitionIdentity::ValueType(enum_type.id()));
    }
    for record_value_type in catalogue.record_value_types() {
        identities.push(DefinitionIdentity::ValueType(record_value_type.id()));
        for field in record_value_type.fields() {
            identities.push(DefinitionIdentity::Field {
                owner: record_value_type.id(),
                field: field.id(),
            });
        }
    }
    for binding in catalogue.type_bindings() {
        identities.push(DefinitionIdentity::TypeBinding(binding.id()));
    }
    for function in catalogue.functions() {
        identities.push(DefinitionIdentity::Function(function.id()));
        for parameter in function.parameters() {
            identities.push(DefinitionIdentity::Parameter {
                owner: function.id(),
                parameter: parameter.id(),
            });
        }
        if let FunctionReturn::Rows(columns) = function.return_type() {
            for column in columns {
                identities.push(DefinitionIdentity::FunctionReturnColumn {
                    owner: function.id(),
                    ordinal: column.ordinal(),
                });
            }
        }
    }
    for expression in expressions.keys() {
        identities.push(DefinitionIdentity::Expression(*expression));
    }
    identities
}

fn object_type_owner_schema(
    catalogue: &CatalogueSnapshot,
    object_type: &ObjectTypeDefinition,
) -> Result<SchemaId, CanonicalHashError> {
    owner_schema(catalogue, object_type.name()).ok_or(
        CanonicalHashError::ObjectTypeOwnerSchemaNotFound {
            object_type: object_type.id(),
        },
    )
}

fn function_owner_schema(
    catalogue: &CatalogueSnapshot,
    function: &FunctionDefinition,
) -> Result<SchemaId, CanonicalHashError> {
    owner_schema(catalogue, function.name()).ok_or(
        CanonicalHashError::FunctionOwnerSchemaNotFound {
            function: function.id(),
        },
    )
}

fn owner_schema(catalogue: &CatalogueSnapshot, name: &QualifiedSemanticName) -> Option<SchemaId> {
    let parts = name.parts();
    let namespace_parts = parts.get(..parts.len().checked_sub(1)?)?;
    let namespace = QualifiedSemanticName::new(namespace_parts.iter().cloned()).ok()?;
    catalogue
        .schema_by_name(&namespace)
        .map(SchemaDefinition::id)
}

fn executable_kind_for_domain(domain: FunctionDomain) -> ExecutableArtifactKind {
    match domain {
        FunctionDomain::Server => ExecutableArtifactKind::Server,
        FunctionDomain::Client => ExecutableArtifactKind::Client,
    }
}

fn sorted_by_key<T, K: Ord>(items: &[T], key: impl Fn(&T) -> K) -> Vec<&T> {
    let mut sorted = items.iter().collect::<Vec<_>>();
    sorted.sort_by_key(|item| key(item));
    sorted
}

fn definition_identity_sort_key(identity: DefinitionIdentity) -> Vec<u8> {
    let mut encoder = Encoder::new(&[]);
    encoder.definition_identity(identity);
    encoder.bytes
}

fn reference_sort_key(reference: &DefinitionReference) -> Vec<u8> {
    let mut encoder = Encoder::new(&[]);
    encoder.function_id(reference.source_function());
    encoder.function_revision_id(reference.source_revision());
    encoder.u32(reference.ordinal());
    encoder.reference_kind(reference.kind());
    encoder.reference_target(reference.target());
    encoder.source_origin(reference.source_origin());
    encoder.bytes
}

/// Encodes every current resolved type with its canonical type tag and payload.
///
/// The catalogue slot scan rejects value types before any version-1 encoder
/// can write bytes. Version 2 uses tag 4 for the exact resolved value identity.
fn encode_resolved_type(encoder: &mut Encoder, resolved_type: ResolvedType) {
    match resolved_type {
        ResolvedType::Scalar(scalar) => {
            encoder.u8(1);
            encoder.standard_scalar(scalar);
        }
        ResolvedType::Named(id) => {
            encoder.u8(2);
            encoder.type_id(id);
        }
        ResolvedType::Reference { target } => {
            encoder.u8(3);
            encoder.type_id(target);
        }
        ResolvedType::Value(id) => {
            encoder.u8(4);
            encoder.type_id(id);
        }
    }
}

struct Encoder {
    bytes: Vec<u8>,
}

impl Encoder {
    fn new(domain: &[u8]) -> Self {
        Self {
            bytes: domain.to_vec(),
        }
    }

    fn digest(self) -> Sha256Digest {
        Sha256Digest::from_bytes(Sha256::digest(self.bytes).into())
    }

    fn bytes(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn u32(&mut self, value: u32) {
        self.bytes(value.to_be_bytes().as_slice());
    }

    fn u64(&mut self, value: u64) {
        self.bytes(value.to_be_bytes().as_slice());
    }

    fn boolean(&mut self, value: bool) {
        self.u8(u8::from(value));
    }

    fn option_none(&mut self) {
        self.u8(0);
    }

    fn option_some(&mut self) {
        self.u8(1);
    }

    fn sequence_len(
        &mut self,
        length: usize,
        value: &'static str,
    ) -> Result<(), CanonicalHashError> {
        self.u32(length_to_u32(length, value)?);
        Ok(())
    }

    fn blob(&mut self, bytes: &[u8], value: &'static str) -> Result<(), CanonicalHashError> {
        self.sequence_len(bytes.len(), value)?;
        self.bytes(bytes);
        Ok(())
    }

    fn text(&mut self, text: &str, value: &'static str) -> Result<(), CanonicalHashError> {
        self.blob(text.as_bytes(), value)
    }

    fn semantic_name(&mut self, name: &QualifiedSemanticName) -> Result<(), CanonicalHashError> {
        self.sequence_len(name.parts().len(), "semantic name parts")?;
        for part in name.parts() {
            self.text(part, "semantic name part")?;
        }
        Ok(())
    }

    fn prelude_name(&mut self, name: &PreludeTypeName) -> Result<(), CanonicalHashError> {
        self.sequence_len(name.words().len(), "prelude type name words")?;
        for word in name.words() {
            self.text(word, "prelude type name word")?;
        }
        Ok(())
    }

    fn id(&mut self, id: [u8; 16]) {
        self.bytes(&id);
    }

    fn type_id(&mut self, id: TypeId) {
        self.id(id.to_bytes());
    }

    fn type_binding_id(&mut self, id: TypeBindingId) {
        self.id(id.to_bytes());
    }

    fn standard_library_revision_id(&mut self, id: StandardLibraryRevisionId) {
        self.id(id.to_bytes());
    }

    fn field_id(&mut self, id: FieldId) {
        self.id(id.to_bytes());
    }

    fn schema_id(&mut self, id: SchemaId) {
        self.id(id.to_bytes());
    }

    fn source_bundle_id(&mut self, id: SourceBundleId) {
        self.id(id.to_bytes());
    }

    fn source_unit_id(&mut self, id: SourceUnitId) {
        self.id(id.to_bytes());
    }

    fn source_revision_id(&mut self, id: SourceRevisionId) {
        self.id(id.to_bytes());
    }

    fn expression_id(&mut self, id: ExpressionId) {
        self.id(id.to_bytes());
    }

    fn function_id(&mut self, id: FunctionId) {
        self.id(id.to_bytes());
    }

    fn parameter_id(&mut self, id: ParameterId) {
        self.id(id.to_bytes());
    }

    fn function_revision_id(&mut self, id: FunctionRevisionId) {
        self.id(id.to_bytes());
    }

    fn digest_value(&mut self, value: Sha256Digest) {
        self.bytes(&value.to_bytes());
    }

    fn option_source_revision_id(&mut self, id: Option<SourceRevisionId>) {
        match id {
            None => self.option_none(),
            Some(id) => {
                self.option_some();
                self.source_revision_id(id);
            }
        }
    }

    fn option_expression_id(&mut self, id: Option<ExpressionId>) {
        match id {
            None => self.option_none(),
            Some(id) => {
                self.option_some();
                self.expression_id(id);
            }
        }
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

    fn function_domain(&mut self, domain: FunctionDomain) {
        self.u8(match domain {
            FunctionDomain::Server => 1,
            FunctionDomain::Client => 2,
        });
    }

    fn function_security(&mut self, security: FunctionSecurity) {
        self.u8(match security {
            FunctionSecurity::Invoker => 1,
            FunctionSecurity::Definer => 2,
        });
    }

    fn function_transaction(&mut self, transaction: Option<FunctionTransaction>) {
        self.u8(match transaction {
            None => 0,
            Some(FunctionTransaction::Atomic) => 1,
            Some(FunctionTransaction::ReadOnly) => 2,
            Some(FunctionTransaction::Manual) => 3,
        });
    }

    fn function_volatility(&mut self, volatility: FunctionVolatility) {
        self.u8(match volatility {
            FunctionVolatility::Immutable => 1,
            FunctionVolatility::Stable => 2,
            FunctionVolatility::Volatile => 3,
        });
    }

    fn on_delete(&mut self, action: Option<OnDeleteAction>) {
        self.u8(match action {
            None => 0,
            Some(OnDeleteAction::Restrict) => 1,
            Some(OnDeleteAction::SetNull) => 2,
            Some(OnDeleteAction::Cascade) => 3,
        });
    }

    fn executable_artifact_kind(&mut self, kind: ExecutableArtifactKind) {
        self.u8(match kind {
            ExecutableArtifactKind::Server => 1,
            ExecutableArtifactKind::Client => 2,
        });
    }

    fn definition_identity(&mut self, identity: DefinitionIdentity) {
        match identity {
            DefinitionIdentity::Schema(schema) => {
                self.u8(1);
                self.schema_id(schema);
            }
            DefinitionIdentity::ObjectType(object_type) => {
                self.u8(2);
                self.type_id(object_type);
            }
            DefinitionIdentity::ValueType(value_type) => {
                self.u8(8);
                self.type_id(value_type);
            }
            DefinitionIdentity::TypeBinding(binding) => {
                self.u8(9);
                self.type_binding_id(binding);
            }
            DefinitionIdentity::Field { owner, field } => {
                self.u8(3);
                self.type_id(owner);
                self.field_id(field);
            }
            DefinitionIdentity::Function(function) => {
                self.u8(4);
                self.function_id(function);
            }
            DefinitionIdentity::Parameter { owner, parameter } => {
                self.u8(5);
                self.function_id(owner);
                self.parameter_id(parameter);
            }
            DefinitionIdentity::FunctionReturnColumn { owner, ordinal } => {
                self.u8(6);
                self.function_id(owner);
                self.u32(ordinal);
            }
            DefinitionIdentity::Expression(expression) => {
                self.u8(7);
                self.expression_id(expression);
            }
        }
    }

    fn reference_target(&mut self, target: DefinitionReferenceTarget) {
        match target {
            DefinitionReferenceTarget::ObjectType(object_type) => {
                self.u8(1);
                self.type_id(object_type);
            }
            DefinitionReferenceTarget::ValueType(value_type) => {
                self.u8(6);
                self.type_id(value_type);
            }
            DefinitionReferenceTarget::Field { owner, field } => {
                self.u8(2);
                self.type_id(owner);
                self.field_id(field);
            }
            DefinitionReferenceTarget::Function(function) => {
                self.u8(3);
                self.function_id(function);
            }
            DefinitionReferenceTarget::Parameter { owner, parameter } => {
                self.u8(4);
                self.function_id(owner);
                self.parameter_id(parameter);
            }
            DefinitionReferenceTarget::Expression(expression) => {
                self.u8(5);
                self.expression_id(expression);
            }
        }
    }

    fn reference_kind(&mut self, kind: DefinitionReferenceKind) {
        self.u8(reference_kind_tag(kind));
    }

    fn source_origin(&mut self, origin: SourceOrigin) {
        self.source_unit_id(origin.source_unit());
        self.u32(origin.byte_start());
        self.u32(origin.byte_end());
    }
}

const fn reference_kind_tag(kind: DefinitionReferenceKind) -> u8 {
    match kind {
        DefinitionReferenceKind::FunctionCall => 1,
        DefinitionReferenceKind::NamedType => 2,
        DefinitionReferenceKind::ObjectReference => 3,
        DefinitionReferenceKind::ParameterRead => 4,
        DefinitionReferenceKind::QueryObject => 5,
        DefinitionReferenceKind::QueryField => 6,
        DefinitionReferenceKind::Expression => 7,
        DefinitionReferenceKind::WriteObject => 8,
        DefinitionReferenceKind::WriteField => 9,
    }
}

fn length_to_u32(length: usize, value: &'static str) -> Result<u32, CanonicalHashError> {
    u32::try_from(length).map_err(|_| CanonicalHashError::LengthExceedsU32 { value, length })
}

#[cfg(test)]
mod tests;
