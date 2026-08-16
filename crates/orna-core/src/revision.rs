//! Durable, backend-neutral database revision records.
//!
//! This module is the boundary between compiler preparation and kernel apply
//! or recovery. It retains source bytes, semantic catalogue data, generated
//! artifacts, declaration origins, and resolved definition references without
//! exposing compiler syntax, compiler IR, or backend storage details.
//!
//! Constructors validate facts that are complete inside one revision value.
//! `prepare` validates semantic changes against its base catalogue. `apply`
//! validates the locked active revision and supported physical changes.
//! `recover` validates durable rows, hashes, links, and physical naming. Those
//! checks need base or storage context and do not belong in this module.

use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt,
    sync::Arc,
};

use crate::{
    CatalogueRevisionId, ExpressionId, FieldId, FunctionId, FunctionRevisionId, ParameterId,
    SchemaId, SourceBundleId, SourceRevisionId, SourceUnitId, StandardLibraryRevisionId,
    TypeBindingId, TypeId,
    catalogue::{
        CatalogueSnapshot, FunctionDefinition, FunctionDomain, FunctionReturn, FunctionSecurity,
        FunctionVolatility, QualifiedSemanticName, TypeDefinition, TypeLookupName,
        ValueTypeDefinition, ValueTypeKind, ValueTypeMutability, ValueTypePersistence,
    },
    system::{INVOCATION_CARRIERS, SYSTEM_FUNCTIONS},
    types::{ResolvedType, StandardScalar, TypeDescriptor, TypeDescriptorKind},
};

/// The reserved catalogue identity for the ephemeral offline application check.
///
/// This identity must not enter an active, recovered, or deployable revision.
pub const EMPTY_APPLICATION_CATALOGUE_REVISION_ID: CatalogueRevisionId =
    CatalogueRevisionId::from_bytes([0; 16]);

/// The durable revision position that cannot use the offline-check sentinel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DurableCatalogueRevisionRole {
    /// An active or recovered application catalogue.
    ActiveOrRecoveredApplication,
    /// An active or recovered standard catalogue.
    ActiveOrRecoveredStandard,
    /// A deployable revision's expected base catalogue.
    DeployableExpectedBase,
    /// A deployable revision's explicit parent catalogue.
    DeployableParent,
    /// A deployable revision's candidate catalogue.
    DeployableCandidate,
}

/// A durable catalogue canonical-hash contract version.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CatalogueHashVersion {
    /// The original application-only catalogue hash.
    Version1,
    /// The standard-backed catalogue hash.
    Version2,
}

impl CatalogueHashVersion {
    /// Returns the exact positive durable numeric value.
    pub const fn to_u32(self) -> u32 {
        match self {
            Self::Version1 => 1,
            Self::Version2 => 2,
        }
    }
}

impl TryFrom<u32> for CatalogueHashVersion {
    type Error = HashVersionError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Version1),
            2 => Ok(Self::Version2),
            _ => Err(HashVersionError::UnsupportedCatalogue { value }),
        }
    }
}

/// A durable function semantic-hash contract version.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FunctionSemanticHashVersion {
    /// The original semantic hash.
    Version1,
    /// The semantic hash that can identify standard value types.
    Version2,
}

impl FunctionSemanticHashVersion {
    /// Returns the exact positive durable numeric value.
    pub const fn to_u32(self) -> u32 {
        match self {
            Self::Version1 => 1,
            Self::Version2 => 2,
        }
    }
}

impl TryFrom<u32> for FunctionSemanticHashVersion {
    type Error = HashVersionError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Version1),
            2 => Ok(Self::Version2),
            _ => Err(HashVersionError::UnsupportedFunctionSemantic { value }),
        }
    }
}

/// A durable standard-library digest contract version.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum StandardLibraryDigestVersion {
    /// The initial standard-library digest.
    Version1,
    /// The executable standard-library digest.
    Version2,
}

impl StandardLibraryDigestVersion {
    /// Returns the exact positive durable numeric value.
    pub const fn to_u32(self) -> u32 {
        match self {
            Self::Version1 => 1,
            Self::Version2 => 2,
        }
    }
}

impl TryFrom<u32> for StandardLibraryDigestVersion {
    type Error = HashVersionError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Version1),
            2 => Ok(Self::Version2),
            _ => Err(HashVersionError::UnsupportedStandardLibraryDigest { value }),
        }
    }
}

/// An error returned when durable data selects an unsupported hash version.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HashVersionError {
    /// The catalogue hash version is unsupported.
    UnsupportedCatalogue {
        /// The rejected durable number.
        value: u32,
    },
    /// The function semantic hash version is unsupported.
    UnsupportedFunctionSemantic {
        /// The rejected durable number.
        value: u32,
    },
    /// The standard-library digest version is unsupported.
    UnsupportedStandardLibraryDigest {
        /// The rejected durable number.
        value: u32,
    },
}

impl fmt::Display for HashVersionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedCatalogue { value } => {
                write!(formatter, "unsupported catalogue hash version {value}")
            }
            Self::UnsupportedFunctionSemantic { value } => {
                write!(
                    formatter,
                    "unsupported function semantic hash version {value}"
                )
            }
            Self::UnsupportedStandardLibraryDigest { value } => {
                write!(
                    formatter,
                    "unsupported standard library digest version {value}"
                )
            }
        }
    }
}

impl Error for HashVersionError {}

/// A SHA-256 digest retained as exactly thirty-two bytes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Sha256Digest([u8; 32]);

impl Sha256Digest {
    /// Creates a digest from its exact durable bytes.
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the exact durable digest bytes.
    pub const fn to_bytes(self) -> [u8; 32] {
        self.0
    }
}

/// The source and catalogue revisions that become active together.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RevisionPair {
    source: SourceRevisionId,
    catalogue: CatalogueRevisionId,
}

impl RevisionPair {
    /// Creates a pair of revisions that must advance atomically.
    pub const fn new(source: SourceRevisionId, catalogue: CatalogueRevisionId) -> Self {
        Self { source, catalogue }
    }

    /// Returns the source revision identity.
    pub const fn source(self) -> SourceRevisionId {
        self.source
    }

    /// Returns the catalogue revision identity.
    pub const fn catalogue(self) -> CatalogueRevisionId {
        self.catalogue
    }
}

/// A half-open byte range in one stored source unit.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SourceOrigin {
    source_unit: SourceUnitId,
    byte_start: u32,
    byte_end: u32,
}

impl SourceOrigin {
    /// Creates an ordered byte range for one source unit.
    pub fn new(
        source_unit: SourceUnitId,
        byte_start: u32,
        byte_end: u32,
    ) -> Result<Self, RevisionInvariantError> {
        if byte_start > byte_end {
            return Err(RevisionInvariantError::SourceOriginReversed {
                source_unit,
                byte_start,
                byte_end,
            });
        }

        Ok(Self {
            source_unit,
            byte_start,
            byte_end,
        })
    }

    /// Returns the source unit that contains this range.
    pub const fn source_unit(self) -> SourceUnitId {
        self.source_unit
    }

    /// Returns the inclusive byte start of this half-open range.
    pub const fn byte_start(self) -> u32 {
        self.byte_start
    }

    /// Returns the exclusive byte end of this half-open range.
    pub const fn byte_end(self) -> u32 {
        self.byte_end
    }
}

/// One exact source file retained in a durable source revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredSourceUnit {
    id: SourceUnitId,
    ordinal: u32,
    logical_path: String,
    content: String,
    content_hash: Sha256Digest,
}

impl StoredSourceUnit {
    /// Creates one stored source unit.
    pub fn new(
        id: SourceUnitId,
        ordinal: u32,
        logical_path: impl Into<String>,
        content: impl Into<String>,
        content_hash: Sha256Digest,
    ) -> Result<Self, RevisionInvariantError> {
        let logical_path = logical_path.into();
        if logical_path.is_empty() {
            return Err(RevisionInvariantError::EmptyLogicalPath { source_unit: id });
        }
        let content = content.into();
        if u32::try_from(content.len()).is_err() {
            return Err(RevisionInvariantError::SourceContentTooLarge { source_unit: id });
        }

        Ok(Self {
            id,
            ordinal,
            logical_path,
            content,
            content_hash,
        })
    }

    /// Returns the stable source-unit identity.
    pub const fn id(&self) -> SourceUnitId {
        self.id
    }

    /// Returns the zero-based durable source order.
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    /// Returns the exact logical source path.
    pub fn logical_path(&self) -> &str {
        &self.logical_path
    }

    /// Returns the exact retained UTF-8 content.
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Returns the hash of this exact content.
    pub const fn content_hash(&self) -> Sha256Digest {
        self.content_hash
    }
}

/// One immutable, ordered durable source snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredSourceRevision {
    bundle: SourceBundleId,
    id: SourceRevisionId,
    parent: Option<SourceRevisionId>,
    units: Vec<StoredSourceUnit>,
    bundle_hash: Sha256Digest,
    revision_hash: Sha256Digest,
}

impl StoredSourceRevision {
    /// Creates a complete immutable source revision.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        bundle: SourceBundleId,
        id: SourceRevisionId,
        parent: Option<SourceRevisionId>,
        units: Vec<StoredSourceUnit>,
        bundle_hash: Sha256Digest,
        revision_hash: Sha256Digest,
    ) -> Result<Self, RevisionInvariantError> {
        if parent == Some(id) {
            return Err(RevisionInvariantError::SourceRevisionSelfParent { revision: id });
        }
        validate_source_units(&units)?;

        Ok(Self {
            bundle,
            id,
            parent,
            units,
            bundle_hash,
            revision_hash,
        })
    }

    /// Returns the durable source-bundle identity.
    pub const fn bundle(&self) -> SourceBundleId {
        self.bundle
    }

    /// Returns this source revision identity.
    pub const fn id(&self) -> SourceRevisionId {
        self.id
    }

    /// Returns the previous source revision, when this revision has one.
    pub const fn parent(&self) -> Option<SourceRevisionId> {
        self.parent
    }

    /// Returns source units in exact durable ordinal order.
    pub fn units(&self) -> &[StoredSourceUnit] {
        &self.units
    }

    /// Returns the aggregate hash for the ordered source bundle.
    pub const fn bundle_hash(&self) -> Sha256Digest {
        self.bundle_hash
    }

    /// Returns the aggregate hash for this source revision record.
    pub const fn revision_hash(&self) -> Sha256Digest {
        self.revision_hash
    }

    fn source_unit(&self, id: SourceUnitId) -> Option<&StoredSourceUnit> {
        self.units.iter().find(|unit| unit.id == id)
    }
}

/// One retained, structurally valid standard-library revision.
#[derive(Clone, Debug)]
pub struct StandardLibrarySnapshot {
    inner: Arc<StandardLibrarySnapshotData>,
}

#[derive(Debug)]
struct StandardLibrarySnapshotData {
    revision: StandardLibraryRevisionId,
    digest_version: StandardLibraryDigestVersion,
    source: StoredSourceRevision,
    language_version: String,
    catalogue: CatalogueSnapshot,
    executables: Vec<StandardExecutable>,
    origins: Vec<DefinitionOrigin>,
    digest: Sha256Digest,
}

impl StandardLibrarySnapshot {
    /// Creates a standard-library snapshot without calculating canonical hashes.
    pub fn new(
        revision: StandardLibraryRevisionId,
        digest_version: StandardLibraryDigestVersion,
        source: StoredSourceRevision,
        language_version: impl Into<String>,
        catalogue: CatalogueSnapshot,
        origins: Vec<DefinitionOrigin>,
        digest: Sha256Digest,
    ) -> Result<Self, RevisionInvariantError> {
        Self::new_with_executables(
            revision,
            digest_version,
            source,
            language_version,
            catalogue,
            Vec::new(),
            origins,
            digest,
        )
    }

    /// Creates a standard-library snapshot with complete executable evidence.
    ///
    /// Version 1 accepts no executable records. Version 2 records one current
    /// executable revision for every catalogue function in the same order.
    /// Canonical verification checks the retained digest and semantic hashes.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_executables(
        revision: StandardLibraryRevisionId,
        digest_version: StandardLibraryDigestVersion,
        source: StoredSourceRevision,
        language_version: impl Into<String>,
        catalogue: CatalogueSnapshot,
        executables: Vec<StandardExecutable>,
        origins: Vec<DefinitionOrigin>,
        digest: Sha256Digest,
    ) -> Result<Self, RevisionInvariantError> {
        reject_offline_check_catalogue_revision(
            catalogue.revision(),
            DurableCatalogueRevisionRole::ActiveOrRecoveredStandard,
        )?;
        reject_reserved_invocation_carrier(&catalogue)?;
        let language_version = language_version.into();
        if language_version.is_empty() {
            return Err(RevisionInvariantError::EmptyStandardLibraryLanguageVersion { revision });
        }
        match digest_version {
            StandardLibraryDigestVersion::Version1 => {
                if source.parent().is_some() {
                    return Err(RevisionInvariantError::StandardLibrarySourceHasParent {
                        source: source.id(),
                        parent: source.parent(),
                    });
                }
                if !catalogue.object_types().is_empty()
                    || !catalogue.record_value_types().is_empty()
                    || !catalogue.functions().is_empty()
                {
                    return Err(
                        RevisionInvariantError::UnsupportedStandardLibraryDefinition { revision },
                    );
                }
                if !executables.is_empty() {
                    return Err(
                        RevisionInvariantError::VersionOneStandardLibraryHasExecutable { revision },
                    );
                }
                validate_origins(&source, &catalogue, &[], &origins)?;
            }
            StandardLibraryDigestVersion::Version2 => {
                if source.parent().is_none() {
                    return Err(
                        RevisionInvariantError::VersionTwoStandardLibrarySourceHasNoParent {
                            source: source.id(),
                        },
                    );
                }
                if !catalogue.object_types().is_empty()
                    || !catalogue.record_value_types().is_empty()
                {
                    return Err(
                        RevisionInvariantError::UnsupportedStandardLibraryDefinition { revision },
                    );
                }
                validate_origins(&source, &catalogue, &[], &origins)?;
                validate_standard_executables(&source, &catalogue, &origins, &executables)?;
            }
        }

        Ok(Self {
            inner: Arc::new(StandardLibrarySnapshotData {
                revision,
                digest_version,
                source,
                language_version,
                catalogue,
                executables,
                origins,
                digest,
            }),
        })
    }

    /// Returns this immutable standard-library revision identity.
    pub fn revision(&self) -> StandardLibraryRevisionId {
        self.inner.revision
    }

    /// Returns the durable standard-library digest contract version.
    pub fn digest_version(&self) -> StandardLibraryDigestVersion {
        self.inner.digest_version
    }

    /// Returns the exact retained standard source revision.
    pub fn source(&self) -> &StoredSourceRevision {
        &self.inner.source
    }

    /// Returns the nonempty compatible language label.
    pub fn language_version(&self) -> &str {
        &self.inner.language_version
    }

    /// Returns the standard-library catalogue definitions.
    pub fn catalogue(&self) -> &CatalogueSnapshot {
        &self.inner.catalogue
    }

    /// Returns complete ordered executable evidence for version 2.
    pub fn executables(&self) -> &[StandardExecutable] {
        &self.inner.executables
    }

    /// Returns source origins for standard definitions and bindings.
    pub fn origins(&self) -> &[DefinitionOrigin] {
        &self.inner.origins
    }

    /// Returns the retained canonical standard-library digest.
    pub fn digest(&self) -> Sha256Digest {
        self.inner.digest
    }
}

/// A standard-library snapshot whose retained source and digest were verified canonically.
#[derive(Clone, Debug)]
pub struct VerifiedStandardLibrarySnapshot {
    snapshot: StandardLibrarySnapshot,
}

impl VerifiedStandardLibrarySnapshot {
    pub(crate) const fn new(snapshot: StandardLibrarySnapshot) -> Self {
        Self { snapshot }
    }

    /// Returns the verified immutable standard-library revision identity.
    pub fn revision(&self) -> StandardLibraryRevisionId {
        self.snapshot.revision()
    }

    /// Returns the verified standard-library digest contract version.
    pub fn digest_version(&self) -> StandardLibraryDigestVersion {
        self.snapshot.digest_version()
    }

    /// Returns the verified exact standard source revision.
    pub fn source(&self) -> &StoredSourceRevision {
        self.snapshot.source()
    }

    /// Returns the verified compatible language label.
    pub fn language_version(&self) -> &str {
        self.snapshot.language_version()
    }

    /// Returns the verified standard-library catalogue definitions.
    pub fn catalogue(&self) -> &CatalogueSnapshot {
        self.snapshot.catalogue()
    }

    /// Returns verified executable evidence for the standard functions.
    pub fn executables(&self) -> &[StandardExecutable] {
        self.snapshot.executables()
    }

    /// Returns the verified source origins for standard definitions and bindings.
    pub fn origins(&self) -> &[DefinitionOrigin] {
        self.snapshot.origins()
    }

    /// Returns the verified canonical standard-library digest.
    pub fn digest(&self) -> Sha256Digest {
        self.snapshot.digest()
    }
}

/// The closed canonical-hash context for one application catalogue.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub enum CatalogueHashContext {
    /// An application-only version-1 catalogue.
    Version1,
    /// A version-2 application catalogue pinned to one standard revision.
    Version2 {
        /// The complete verified standard-library snapshot.
        standard: VerifiedStandardLibrarySnapshot,
    },
}

impl CatalogueHashContext {
    /// Creates an application-only version-1 context.
    pub const fn version_one() -> Self {
        Self::Version1
    }

    /// Creates a version-2 context pinned to one standard-library snapshot.
    pub const fn version_two(standard: VerifiedStandardLibrarySnapshot) -> Self {
        Self::Version2 { standard }
    }

    /// Returns the exact catalogue hash contract version.
    pub const fn version(&self) -> CatalogueHashVersion {
        match self {
            Self::Version1 => CatalogueHashVersion::Version1,
            Self::Version2 { .. } => CatalogueHashVersion::Version2,
        }
    }

    /// Returns the pinned standard snapshot for version 2.
    pub const fn standard(&self) -> Option<&VerifiedStandardLibrarySnapshot> {
        match self {
            Self::Version1 => None,
            Self::Version2 { standard } => Some(standard),
        }
    }
}

/// The identity of a catalogue member that owns a source origin.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DefinitionIdentity {
    /// A declared logical schema.
    Schema(SchemaId),
    /// A durable object type.
    ObjectType(TypeId),
    /// A catalogue value type.
    ValueType(TypeId),
    /// A direct type-name binding.
    TypeBinding(TypeBindingId),
    /// A field owned by an object or record value type.
    Field {
        /// The stable owning type identity.
        owner: TypeId,
        /// The stable field identity.
        field: FieldId,
    },
    /// An executable function.
    Function(FunctionId),
    /// A parameter owned by a function.
    Parameter {
        /// The stable function identity.
        owner: FunctionId,
        /// The stable parameter identity.
        parameter: ParameterId,
    },
    /// One named column in a `ROWS (...)` function result.
    FunctionReturnColumn {
        /// The stable function identity.
        owner: FunctionId,
        /// The stable zero-based result-column ordinal.
        ordinal: u32,
    },
    /// A compiled expression artifact.
    Expression(ExpressionId),
}

/// A stable, 16-byte definition identity that can receive a semantic reference.
///
/// Result columns are catalogue subobjects identified by function and ordinal,
/// so they can own source origins but are not reference targets in this model.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum DefinitionReferenceTarget {
    /// A durable object type.
    ObjectType(TypeId),
    /// A catalogue value type.
    ValueType(TypeId),
    /// A stable field owned by an object or record value type.
    Field {
        /// The stable owning type identity.
        owner: TypeId,
        /// The stable field identity.
        field: FieldId,
    },
    /// An executable function.
    Function(FunctionId),
    /// A stable parameter owned by a function.
    Parameter {
        /// The stable function identity.
        owner: FunctionId,
        /// The stable parameter identity.
        parameter: ParameterId,
    },
    /// A compiled expression artifact.
    Expression(ExpressionId),
}

impl From<DefinitionReferenceTarget> for DefinitionIdentity {
    fn from(target: DefinitionReferenceTarget) -> Self {
        match target {
            DefinitionReferenceTarget::ObjectType(id) => Self::ObjectType(id),
            DefinitionReferenceTarget::ValueType(id) => Self::ValueType(id),
            DefinitionReferenceTarget::Field { owner, field } => Self::Field { owner, field },
            DefinitionReferenceTarget::Function(id) => Self::Function(id),
            DefinitionReferenceTarget::Parameter { owner, parameter } => {
                Self::Parameter { owner, parameter }
            }
            DefinitionReferenceTarget::Expression(id) => Self::Expression(id),
        }
    }
}

/// The source declaration range for one stable definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefinitionOrigin {
    identity: DefinitionIdentity,
    source: SourceOrigin,
}

impl DefinitionOrigin {
    /// Associates one stable definition with its retained source range.
    pub const fn new(identity: DefinitionIdentity, source: SourceOrigin) -> Self {
        Self { identity, source }
    }

    /// Returns the stable definition identity.
    pub const fn identity(&self) -> DefinitionIdentity {
        self.identity
    }

    /// Returns the exact source range.
    pub const fn source(&self) -> SourceOrigin {
        self.source
    }
}

/// The shared versioned bytes of a durable artifact.
#[derive(Clone, Debug, Eq, PartialEq)]
struct VersionedArtifactBytes {
    format: String,
    version: u32,
    payload: Vec<u8>,
    content_hash: Sha256Digest,
}

impl VersionedArtifactBytes {
    fn new(
        format: impl Into<String>,
        version: u32,
        payload: Vec<u8>,
        content_hash: Sha256Digest,
    ) -> Result<Self, RevisionInvariantError> {
        let format = format.into();
        validate_artifact_parts(&format, version, &payload)?;
        Ok(Self {
            format,
            version,
            payload,
            content_hash,
        })
    }
}

/// One versioned compiled expression artifact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpressionArtifact {
    id: ExpressionId,
    bytes: VersionedArtifactBytes,
}

impl ExpressionArtifact {
    /// Creates one versioned expression artifact.
    pub fn new(
        id: ExpressionId,
        format: impl Into<String>,
        version: u32,
        payload: Vec<u8>,
        content_hash: Sha256Digest,
    ) -> Result<Self, RevisionInvariantError> {
        Ok(Self {
            id,
            bytes: VersionedArtifactBytes::new(format, version, payload, content_hash)?,
        })
    }

    /// Returns the stable expression identity.
    pub const fn id(&self) -> ExpressionId {
        self.id
    }

    /// Returns the versioned artifact format identity.
    pub fn format(&self) -> &str {
        &self.bytes.format
    }

    /// Returns the positive format version.
    pub const fn version(&self) -> u32 {
        self.bytes.version
    }

    /// Returns the exact canonical artifact payload.
    pub fn payload(&self) -> &[u8] {
        &self.bytes.payload
    }

    /// Returns the payload content hash.
    pub const fn content_hash(&self) -> Sha256Digest {
        self.bytes.content_hash
    }
}

/// The execution domain encoded by a versioned executable artifact.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ExecutableArtifactKind {
    /// An artifact that executes in the database server runtime.
    Server,
    /// An artifact that executes in an installed client runtime.
    Client,
}

/// A complete versioned executable artifact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutableArtifact {
    kind: ExecutableArtifactKind,
    bytes: VersionedArtifactBytes,
}

impl ExecutableArtifact {
    /// Creates one complete executable artifact.
    pub fn new(
        kind: ExecutableArtifactKind,
        format: impl Into<String>,
        version: u32,
        payload: Vec<u8>,
        content_hash: Sha256Digest,
    ) -> Result<Self, RevisionInvariantError> {
        Ok(Self {
            kind,
            bytes: VersionedArtifactBytes::new(format, version, payload, content_hash)?,
        })
    }

    /// Returns the execution domain encoded by this artifact.
    pub const fn kind(&self) -> ExecutableArtifactKind {
        self.kind
    }

    /// Returns the versioned artifact format identity.
    pub fn format(&self) -> &str {
        &self.bytes.format
    }

    /// Returns the positive format version.
    pub const fn version(&self) -> u32 {
        self.bytes.version
    }

    /// Returns the exact canonical artifact payload.
    pub fn payload(&self) -> &[u8] {
        &self.bytes.payload
    }

    /// Returns the payload content hash.
    pub const fn content_hash(&self) -> Sha256Digest {
        self.bytes.content_hash
    }
}

/// One immutable revision of an executable function.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FunctionRevisionRecord {
    function: FunctionId,
    id: FunctionRevisionId,
    revision_number: u64,
    declaration_origin: SourceOrigin,
    declaration_content_hash: Sha256Digest,
    semantic_hash: Sha256Digest,
    semantic_hash_version: FunctionSemanticHashVersion,
    language_version: String,
    artifact: ExecutableArtifact,
}

impl FunctionRevisionRecord {
    /// Creates one immutable function revision record.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        function: FunctionId,
        id: FunctionRevisionId,
        revision_number: u64,
        declaration_origin: SourceOrigin,
        declaration_content_hash: Sha256Digest,
        semantic_hash: Sha256Digest,
        language_version: impl Into<String>,
        artifact: ExecutableArtifact,
    ) -> Result<Self, RevisionInvariantError> {
        if revision_number == 0 {
            return Err(RevisionInvariantError::ZeroFunctionRevisionNumber { function, id });
        }
        let language_version = language_version.into();
        if language_version.is_empty() {
            return Err(RevisionInvariantError::EmptyLanguageVersion { function, id });
        }

        Ok(Self {
            function,
            id,
            revision_number,
            declaration_origin,
            declaration_content_hash,
            semantic_hash,
            semantic_hash_version: FunctionSemanticHashVersion::Version1,
            language_version,
            artifact,
        })
    }

    /// Selects the semantic-hash contract for this newly built record.
    #[must_use]
    pub fn with_semantic_hash_version(
        mut self,
        semantic_hash_version: FunctionSemanticHashVersion,
    ) -> Self {
        self.semantic_hash_version = semantic_hash_version;
        self
    }

    /// Returns the stable function identity.
    pub const fn function(&self) -> FunctionId {
        self.function
    }

    /// Returns the immutable function-revision identity.
    pub const fn id(&self) -> FunctionRevisionId {
        self.id
    }

    /// Returns the positive, per-function revision number.
    pub const fn revision_number(&self) -> u64 {
        self.revision_number
    }

    /// Returns the declaration range in the source revision that introduced
    /// this immutable function revision.
    pub const fn declaration_origin(&self) -> SourceOrigin {
        self.declaration_origin
    }

    /// Returns the exact declaration content hash.
    pub const fn declaration_content_hash(&self) -> Sha256Digest {
        self.declaration_content_hash
    }

    /// Returns the compiler semantic hash.
    pub const fn semantic_hash(&self) -> Sha256Digest {
        self.semantic_hash
    }

    /// Returns the durable semantic-hash contract version.
    pub const fn semantic_hash_version(&self) -> FunctionSemanticHashVersion {
        self.semantic_hash_version
    }

    /// Returns the nonempty language version label.
    pub fn language_version(&self) -> &str {
        &self.language_version
    }

    /// Returns the immutable executable artifact.
    pub fn artifact(&self) -> &ExecutableArtifact {
        &self.artifact
    }
}

/// The explicit semantic relation recorded between durable definitions.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DefinitionReferenceKind {
    /// A function invokes another function.
    FunctionCall,
    /// A declaration uses a named object type.
    NamedType,
    /// A declaration uses a typed object reference.
    ObjectReference,
    /// Executable code reads or binds a function parameter.
    ParameterRead,
    /// A relational plan reads an object type.
    QueryObject,
    /// A relational plan reads a field.
    QueryField,
    /// A declaration or executable expression uses another expression.
    Expression,
    /// A mutation writes an object of the target object type.
    WriteObject,
    /// A mutation writes one owner-qualified object or record value field.
    WriteField,
}

/// One resolved definition reference from an immutable function revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefinitionReference {
    source_function: FunctionId,
    source_revision: FunctionRevisionId,
    ordinal: u32,
    target: DefinitionReferenceTarget,
    kind: DefinitionReferenceKind,
    source_origin: SourceOrigin,
}

impl DefinitionReference {
    /// Creates one ordered, resolved definition reference.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        source_function: FunctionId,
        source_revision: FunctionRevisionId,
        ordinal: u32,
        target: DefinitionReferenceTarget,
        kind: DefinitionReferenceKind,
        source_origin: SourceOrigin,
    ) -> Self {
        Self {
            source_function,
            source_revision,
            ordinal,
            target,
            kind,
            source_origin,
        }
    }

    /// Returns the function that contains this reference.
    pub const fn source_function(&self) -> FunctionId {
        self.source_function
    }

    /// Returns the immutable source function revision.
    pub const fn source_revision(&self) -> FunctionRevisionId {
        self.source_revision
    }

    /// Returns the deterministic zero-based reference ordinal.
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    /// Returns the stable target definition identity.
    pub const fn target(&self) -> DefinitionReferenceTarget {
        self.target
    }

    /// Returns the explicit resolved reference category.
    pub const fn kind(&self) -> DefinitionReferenceKind {
        self.kind
    }

    /// Returns the source range of this reference.
    pub const fn source_origin(&self) -> SourceOrigin {
        self.source_origin
    }
}

/// The immutable executable facts for one version-2 standard function.
///
/// This record links one catalogue function to its current immutable revision
/// and complete ordered reference sequence. It does not duplicate source text,
/// origins, or the catalogue function definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StandardExecutable {
    function: FunctionId,
    revision: FunctionRevisionRecord,
    references: Vec<DefinitionReference>,
}

impl StandardExecutable {
    /// Creates one standard executable record.
    pub fn new(
        function: FunctionId,
        revision: FunctionRevisionRecord,
        references: Vec<DefinitionReference>,
    ) -> Result<Self, RevisionInvariantError> {
        if revision.function() != function {
            return Err(RevisionInvariantError::StandardExecutableFunctionMismatch {
                function,
                revision_function: revision.function(),
                revision: revision.id(),
            });
        }
        Ok(Self {
            function,
            revision,
            references,
        })
    }

    /// Returns the linked standard catalogue function identity.
    pub const fn function(&self) -> FunctionId {
        self.function
    }

    /// Returns the complete immutable current function revision.
    pub fn revision(&self) -> &FunctionRevisionRecord {
        &self.revision
    }

    /// Returns complete ordered semantic reference evidence.
    pub fn references(&self) -> &[DefinitionReference] {
        &self.references
    }
}

/// The complete durable state reconstructed after database recovery.
#[derive(Clone, Debug)]
pub struct ActiveRevisionContent {
    expressions: Vec<ExpressionArtifact>,
    function_revisions: Vec<FunctionRevisionRecord>,
    historical_function_revisions: Vec<FunctionRevisionRecord>,
    origins: Vec<DefinitionOrigin>,
    references: Vec<DefinitionReference>,
}

impl ActiveRevisionContent {
    /// Groups the active semantic records that accompany one catalogue snapshot.
    pub const fn new(
        expressions: Vec<ExpressionArtifact>,
        function_revisions: Vec<FunctionRevisionRecord>,
        origins: Vec<DefinitionOrigin>,
        references: Vec<DefinitionReference>,
    ) -> Self {
        Self {
            expressions,
            function_revisions,
            historical_function_revisions: Vec::new(),
            origins,
            references,
        }
    }

    /// Adds immutable function revisions that are no longer current.
    #[must_use]
    pub fn with_history(
        mut self,
        historical_function_revisions: Vec<FunctionRevisionRecord>,
    ) -> Self {
        self.historical_function_revisions = historical_function_revisions;
        self
    }
}

/// The source, catalogue, and semantic records needed to build one active revision.
#[derive(Clone, Debug)]
pub struct ActiveDatabaseRevisionInput {
    pair: RevisionPair,
    source: StoredSourceRevision,
    catalogue: CatalogueSnapshot,
    catalogue_hash: Sha256Digest,
    content: ActiveRevisionContent,
}

impl ActiveDatabaseRevisionInput {
    /// Groups one complete active revision before its hash context is selected.
    pub const fn new(
        pair: RevisionPair,
        source: StoredSourceRevision,
        catalogue: CatalogueSnapshot,
        catalogue_hash: Sha256Digest,
        content: ActiveRevisionContent,
    ) -> Self {
        Self {
            pair,
            source,
            catalogue,
            catalogue_hash,
            content,
        }
    }
}

/// The complete durable state reconstructed after database recovery.
#[derive(Clone, Debug)]
pub struct ActiveDatabaseRevision {
    pair: RevisionPair,
    source: StoredSourceRevision,
    catalogue: CatalogueSnapshot,
    catalogue_hash: Sha256Digest,
    catalogue_hash_context: CatalogueHashContext,
    expressions: Vec<ExpressionArtifact>,
    function_revisions: Vec<FunctionRevisionRecord>,
    historical_function_revisions: Vec<FunctionRevisionRecord>,
    origins: Vec<DefinitionOrigin>,
    references: Vec<DefinitionReference>,
}

impl ActiveDatabaseRevision {
    /// Validates and creates one complete active database revision.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pair: RevisionPair,
        source: StoredSourceRevision,
        catalogue: CatalogueSnapshot,
        catalogue_hash: Sha256Digest,
        expressions: Vec<ExpressionArtifact>,
        function_revisions: Vec<FunctionRevisionRecord>,
        origins: Vec<DefinitionOrigin>,
        references: Vec<DefinitionReference>,
    ) -> Result<Self, RevisionInvariantError> {
        Self::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                pair,
                source,
                catalogue,
                catalogue_hash,
                ActiveRevisionContent::new(expressions, function_revisions, origins, references),
            ),
            CatalogueHashContext::version_one(),
        )
    }

    /// Creates one active revision with an explicit closed catalogue hash context.
    pub fn new_with_catalogue_hash_context(
        input: ActiveDatabaseRevisionInput,
        catalogue_hash_context: CatalogueHashContext,
    ) -> Result<Self, RevisionInvariantError> {
        let ActiveDatabaseRevisionInput {
            pair,
            source,
            catalogue,
            catalogue_hash,
            content:
                ActiveRevisionContent {
                    expressions,
                    function_revisions,
                    historical_function_revisions,
                    origins,
                    references,
                },
        } = input;
        reject_offline_check_catalogue_revision(
            catalogue.revision(),
            DurableCatalogueRevisionRole::ActiveOrRecoveredApplication,
        )?;
        if let Some(standard) = catalogue_hash_context.standard() {
            reject_offline_check_catalogue_revision(
                standard.catalogue().revision(),
                DurableCatalogueRevisionRole::ActiveOrRecoveredStandard,
            )?;
        }
        validate_pair(&pair, &source, &catalogue)?;
        validate_catalogue_hash_context_coherence(
            &catalogue_hash_context,
            &catalogue,
            &function_revisions,
            &origins,
            &references,
        )?;
        validate_expressions(&expressions)?;
        validate_origins(&source, &catalogue, &expressions, &origins)?;
        validate_function_revisions(
            &source,
            &catalogue,
            &origins,
            &function_revisions,
            FunctionRevisionSet::RecoveredActive,
        )?;
        validate_function_revision_history(&function_revisions, &historical_function_revisions)?;
        validate_references(
            &source,
            &catalogue,
            catalogue_hash_context
                .standard()
                .map(VerifiedStandardLibrarySnapshot::catalogue),
            &expressions,
            &function_revisions,
            &references,
        )?;

        Ok(Self {
            pair,
            source,
            catalogue,
            catalogue_hash,
            catalogue_hash_context,
            expressions,
            function_revisions,
            historical_function_revisions,
            origins,
            references,
        })
    }

    /// Validates and creates one complete active database revision together
    /// with immutable function revisions that are not current.
    ///
    /// Historical revisions can belong to functions outside the active
    /// catalogue. Their declaration origins can belong to earlier source
    /// revisions. Function-revision identities and per-function revision
    /// numbers must remain unique across the current and historical sets.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_history(
        pair: RevisionPair,
        source: StoredSourceRevision,
        catalogue: CatalogueSnapshot,
        catalogue_hash: Sha256Digest,
        expressions: Vec<ExpressionArtifact>,
        function_revisions: Vec<FunctionRevisionRecord>,
        historical_function_revisions: Vec<FunctionRevisionRecord>,
        origins: Vec<DefinitionOrigin>,
        references: Vec<DefinitionReference>,
    ) -> Result<Self, RevisionInvariantError> {
        Self::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                pair,
                source,
                catalogue,
                catalogue_hash,
                ActiveRevisionContent::new(expressions, function_revisions, origins, references)
                    .with_history(historical_function_revisions),
            ),
            CatalogueHashContext::version_one(),
        )
    }

    /// Returns the active source and catalogue revision pair.
    pub const fn pair(&self) -> RevisionPair {
        self.pair
    }

    /// Returns the exact active source snapshot.
    pub fn source(&self) -> &StoredSourceRevision {
        &self.source
    }

    /// Returns the active immutable semantic catalogue.
    pub fn catalogue(&self) -> &CatalogueSnapshot {
        &self.catalogue
    }

    /// Returns the canonical aggregate hash of the active catalogue revision.
    pub const fn catalogue_hash(&self) -> Sha256Digest {
        self.catalogue_hash
    }

    /// Returns the closed context that selects the catalogue hash contract.
    pub const fn catalogue_hash_context(&self) -> &CatalogueHashContext {
        &self.catalogue_hash_context
    }

    /// Converts one legacy flat resolved type into a catalogue-validated leaf descriptor.
    ///
    /// This does not admit the descriptor in any catalogue or execution
    /// position. Legacy compatibility scalars have no catalogue identity and
    /// must first be migrated to a resolved value identity.
    pub fn type_descriptor_for(
        &self,
        resolved_type: ResolvedType,
    ) -> Result<TypeDescriptor, FlatTypeDescriptorError> {
        match resolved_type {
            ResolvedType::Scalar(scalar) => Err(FlatTypeDescriptorError::LegacyScalar { scalar }),
            ResolvedType::Named(id) => {
                let application = self.catalogue.type_definition_by_id(id);
                let standard = self
                    .catalogue_hash_context
                    .standard()
                    .and_then(|standard| standard.catalogue().type_definition_by_id(id));
                if application.is_some() && standard.is_some() {
                    return Err(FlatTypeDescriptorError::AmbiguousNamedType { id });
                }
                match application.or(standard) {
                    None => Err(FlatTypeDescriptorError::UnknownNamedType { id }),
                    Some(TypeDefinition::Object(_)) => {
                        Err(FlatTypeDescriptorError::NamedObjectType { id })
                    }
                    Some(TypeDefinition::Value(_)) => {
                        Err(FlatTypeDescriptorError::NamedValueType { id })
                    }
                    Some(TypeDefinition::Enum(_) | TypeDefinition::RecordValue(_)) => {
                        Ok(TypeDescriptor::named(id))
                    }
                }
            }
            ResolvedType::Value(value_type) => {
                let standard = self
                    .catalogue_hash_context
                    .standard()
                    .ok_or(FlatTypeDescriptorError::StandardLibraryUnavailable { value_type })?;
                if !standard
                    .catalogue()
                    .value_type_by_id(value_type)
                    .is_some_and(|definition| {
                        matches!(
                            definition.kind(),
                            ValueTypeKind::Primitive | ValueTypeKind::Opaque
                        )
                    })
                {
                    return Err(FlatTypeDescriptorError::UnknownStandardValueType { value_type });
                }
                Ok(TypeDescriptor::named(value_type))
            }
            ResolvedType::Reference { target } => {
                if self.catalogue.object_type_by_id(target).is_none() {
                    return Err(FlatTypeDescriptorError::ReferenceTargetNotObject { target });
                }
                Ok(TypeDescriptor::reference(target))
            }
        }
    }

    /// Resolves one admitted record field to its executable runtime type.
    ///
    /// This uses the application catalogue and the exact verified standard
    /// snapshot pinned by this active revision.
    pub fn record_value_field_runtime_type(
        &self,
        resolved_type: ResolvedType,
    ) -> Option<ResolvedType> {
        let standard = self.catalogue_hash_context.standard()?.catalogue();
        record_value_field_runtime_type(&self.catalogue, standard, resolved_type)
    }

    /// Resolves one admitted record-field descriptor to its executable runtime type.
    ///
    /// The active application catalogue and its pinned verified standard
    /// snapshot are the only classification authority.
    pub fn record_value_field_descriptor_runtime_type(
        &self,
        descriptor: &TypeDescriptor,
    ) -> Option<ResolvedType> {
        let standard = self.catalogue_hash_context.standard()?.catalogue();
        match classify_record_value_field_descriptor(&self.catalogue, standard, descriptor).ok()? {
            RecordValueFieldDescriptorClass::ApplicationEnum(type_id)
            | RecordValueFieldDescriptorClass::ApplicationRecord(type_id)
            | RecordValueFieldDescriptorClass::StandardEnum(type_id) => {
                Some(ResolvedType::named(type_id))
            }
            RecordValueFieldDescriptorClass::StandardPrimitive(type_id) => standard
                .value_type_by_id(type_id)
                .and_then(accepted_record_scalar)
                .map(ResolvedType::scalar),
        }
    }

    /// Returns expression artifacts by durable record order.
    pub fn expressions(&self) -> &[ExpressionArtifact] {
        &self.expressions
    }

    /// Returns active function revisions by durable record order.
    pub fn function_revisions(&self) -> &[FunctionRevisionRecord] {
        &self.function_revisions
    }

    /// Returns immutable function revisions that are not current.
    pub fn historical_function_revisions(&self) -> &[FunctionRevisionRecord] {
        &self.historical_function_revisions
    }

    /// Returns known definition declaration origins.
    pub fn origins(&self) -> &[DefinitionOrigin] {
        &self.origins
    }

    /// Returns resolved definition references by durable record order.
    pub fn references(&self) -> &[DefinitionReference] {
        &self.references
    }
}

/// A failure to validate one flat resolved type against an active revision.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FlatTypeDescriptorError {
    /// A version-1 compatibility scalar has no durable catalogue identity.
    LegacyScalar {
        /// The rejected compatibility representation.
        scalar: StandardScalar,
    },
    /// The same named identity is present in both active type catalogues.
    AmbiguousNamedType {
        /// The colliding type identity.
        id: TypeId,
    },
    /// The named identity is absent from both active type catalogues.
    UnknownNamedType {
        /// The missing type identity.
        id: TypeId,
    },
    /// The named identity resolves to an object and must use `REF`.
    NamedObjectType {
        /// The rejected object identity.
        id: TypeId,
    },
    /// The named identity resolves to a primitive or opaque value definition.
    NamedValueType {
        /// The rejected value identity.
        id: TypeId,
    },
    /// A resolved value identity has no pinned standard-library catalogue.
    StandardLibraryUnavailable {
        /// The unresolved value identity.
        value_type: TypeId,
    },
    /// A resolved value identity is absent from the pinned standard library.
    UnknownStandardValueType {
        /// The missing standard value identity.
        value_type: TypeId,
    },
    /// A resolved reference target is not an active application object.
    ReferenceTargetNotObject {
        /// The rejected reference target.
        target: TypeId,
    },
}

impl fmt::Display for FlatTypeDescriptorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LegacyScalar { .. } => {
                formatter.write_str("legacy scalar type has no catalogue identity")
            }
            Self::AmbiguousNamedType { .. } => formatter.write_str(
                "resolved named type is present in both application and standard catalogues",
            ),
            Self::UnknownNamedType { .. } => {
                formatter.write_str("resolved named type is absent from the active catalogue")
            }
            Self::NamedObjectType { .. } => {
                formatter.write_str("resolved named type is an object and requires REF")
            }
            Self::NamedValueType { .. } => formatter.write_str(
                "resolved named type is a value definition and requires a value identity",
            ),
            Self::StandardLibraryUnavailable { .. } => formatter.write_str(
                "the active database has no standard library for the resolved value type",
            ),
            Self::UnknownStandardValueType { .. } => formatter
                .write_str("resolved value type is absent from the pinned standard library"),
            Self::ReferenceTargetNotObject { .. } => {
                formatter.write_str("resolved reference target is not an active application object")
            }
        }
    }
}

impl Error for FlatTypeDescriptorError {}

/// The semantic records produced for one deployable catalogue candidate.
#[derive(Clone, Debug)]
pub struct DeployableRevisionContent {
    origins: Vec<DefinitionOrigin>,
    expressions: Vec<ExpressionArtifact>,
    new_function_revisions: Vec<FunctionRevisionRecord>,
    current_function_revisions: Option<Vec<FunctionRevisionRecord>>,
    references: Vec<DefinitionReference>,
}

impl DeployableRevisionContent {
    /// Groups the new semantic records that accompany one candidate catalogue.
    pub const fn new(
        origins: Vec<DefinitionOrigin>,
        expressions: Vec<ExpressionArtifact>,
        new_function_revisions: Vec<FunctionRevisionRecord>,
        references: Vec<DefinitionReference>,
    ) -> Self {
        Self {
            origins,
            expressions,
            new_function_revisions,
            current_function_revisions: None,
            references,
        }
    }

    /// Supplies the complete current function-revision records for an explicit
    /// version-2 catalogue candidate.
    #[must_use]
    pub fn with_current_function_revisions(
        mut self,
        current_function_revisions: Vec<FunctionRevisionRecord>,
    ) -> Self {
        self.current_function_revisions = Some(current_function_revisions);
        self
    }
}

/// The source, catalogue, and semantic records needed to build a deployable revision.
#[derive(Clone, Debug)]
pub struct DeployableRevisionInput {
    expected_base: RevisionPair,
    source: StoredSourceRevision,
    parent_catalogue: CatalogueRevisionId,
    candidate: CatalogueSnapshot,
    catalogue_hash: Sha256Digest,
    content: DeployableRevisionContent,
}

impl DeployableRevisionInput {
    /// Groups one complete deployable candidate before its hash context is selected.
    pub const fn new(
        expected_base: RevisionPair,
        source: StoredSourceRevision,
        parent_catalogue: CatalogueRevisionId,
        candidate: CatalogueSnapshot,
        catalogue_hash: Sha256Digest,
        content: DeployableRevisionContent,
    ) -> Self {
        Self {
            expected_base,
            source,
            parent_catalogue,
            candidate,
            catalogue_hash,
            content,
        }
    }
}

/// A compiler-produced candidate that is ready for a kernel apply attempt.
#[derive(Clone, Debug)]
pub struct DeployableRevision {
    expected_base: RevisionPair,
    source: StoredSourceRevision,
    parent_catalogue: CatalogueRevisionId,
    candidate: CatalogueSnapshot,
    catalogue_hash: Sha256Digest,
    catalogue_hash_context: CatalogueHashContext,
    origins: Vec<DefinitionOrigin>,
    expressions: Vec<ExpressionArtifact>,
    new_function_revisions: Vec<FunctionRevisionRecord>,
    current_function_revisions: Option<Vec<FunctionRevisionRecord>>,
    references: Vec<DefinitionReference>,
}

impl DeployableRevision {
    /// Validates and creates a deployable candidate revision.
    ///
    /// This constructor cannot prove that omitted function revisions exist in
    /// the expected base. `prepare` and `apply` must make that check while
    /// they hold the base revision context.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        expected_base: RevisionPair,
        source: StoredSourceRevision,
        parent_catalogue: CatalogueRevisionId,
        candidate: CatalogueSnapshot,
        catalogue_hash: Sha256Digest,
        origins: Vec<DefinitionOrigin>,
        expressions: Vec<ExpressionArtifact>,
        new_function_revisions: Vec<FunctionRevisionRecord>,
        references: Vec<DefinitionReference>,
    ) -> Result<Self, RevisionInvariantError> {
        Self::new_with_catalogue_hash_context(
            DeployableRevisionInput::new(
                expected_base,
                source,
                parent_catalogue,
                candidate,
                catalogue_hash,
                DeployableRevisionContent::new(
                    origins,
                    expressions,
                    new_function_revisions,
                    references,
                ),
            ),
            CatalogueHashContext::version_one(),
        )
    }

    /// Creates a deployable revision with an explicit closed hash context.
    pub fn new_with_catalogue_hash_context(
        input: DeployableRevisionInput,
        catalogue_hash_context: CatalogueHashContext,
    ) -> Result<Self, RevisionInvariantError> {
        let DeployableRevisionInput {
            expected_base,
            source,
            parent_catalogue,
            candidate,
            catalogue_hash,
            content:
                DeployableRevisionContent {
                    origins,
                    expressions,
                    new_function_revisions,
                    current_function_revisions,
                    references,
                },
        } = input;
        reject_offline_check_catalogue_revision(
            expected_base.catalogue(),
            DurableCatalogueRevisionRole::DeployableExpectedBase,
        )?;
        reject_offline_check_catalogue_revision(
            parent_catalogue,
            DurableCatalogueRevisionRole::DeployableParent,
        )?;
        reject_offline_check_catalogue_revision(
            candidate.revision(),
            DurableCatalogueRevisionRole::DeployableCandidate,
        )?;
        if source.parent() != Some(expected_base.source()) {
            return Err(RevisionInvariantError::DeployableSourceParentMismatch {
                expected: expected_base.source(),
                actual: source.parent(),
            });
        }
        if parent_catalogue != expected_base.catalogue() {
            return Err(RevisionInvariantError::DeployableCatalogueParentMismatch {
                expected: expected_base.catalogue(),
                actual: parent_catalogue,
            });
        }
        if candidate.revision() == parent_catalogue {
            return Err(RevisionInvariantError::CatalogueRevisionSelfParent {
                revision: candidate.revision(),
            });
        }
        let revision_evidence = match (&catalogue_hash_context, &current_function_revisions) {
            (CatalogueHashContext::Version1, None) => new_function_revisions.as_slice(),
            (CatalogueHashContext::Version1, Some(current))
            | (CatalogueHashContext::Version2 { .. }, Some(current)) => {
                validate_function_revisions(
                    &source,
                    &candidate,
                    &origins,
                    current,
                    FunctionRevisionSet::DeployableCurrent,
                )?;
                validate_new_function_revision_subset(&new_function_revisions, current)?;
                current
            }
            (CatalogueHashContext::Version2 { .. }, None) => {
                return Err(RevisionInvariantError::DeployableCurrentFunctionRevisionsRequired);
            }
        };
        validate_catalogue_hash_context_coherence(
            &catalogue_hash_context,
            &candidate,
            revision_evidence,
            &origins,
            &references,
        )?;
        validate_expressions(&expressions)?;
        validate_origins(&source, &candidate, &expressions, &origins)?;
        validate_function_revisions(
            &source,
            &candidate,
            &origins,
            &new_function_revisions,
            FunctionRevisionSet::NewCandidate,
        )?;
        validate_references(
            &source,
            &candidate,
            catalogue_hash_context
                .standard()
                .map(VerifiedStandardLibrarySnapshot::catalogue),
            &expressions,
            revision_evidence,
            &references,
        )?;

        Ok(Self {
            expected_base,
            source,
            parent_catalogue,
            candidate,
            catalogue_hash,
            catalogue_hash_context,
            origins,
            expressions,
            new_function_revisions,
            current_function_revisions,
            references,
        })
    }

    /// Returns the source and catalogue pair that this apply expects.
    pub const fn expected_base(&self) -> RevisionPair {
        self.expected_base
    }

    /// Returns the source and catalogue identities created by this candidate.
    pub const fn candidate_pair(&self) -> RevisionPair {
        RevisionPair::new(self.source.id(), self.candidate.revision())
    }

    /// Returns the complete candidate durable source snapshot.
    pub fn source(&self) -> &StoredSourceRevision {
        &self.source
    }

    /// Returns the exact candidate catalogue parent identity.
    pub const fn parent_catalogue(&self) -> CatalogueRevisionId {
        self.parent_catalogue
    }

    /// Returns the candidate immutable semantic catalogue.
    pub fn candidate(&self) -> &CatalogueSnapshot {
        &self.candidate
    }

    /// Returns the canonical aggregate hash of the candidate catalogue.
    pub const fn catalogue_hash(&self) -> Sha256Digest {
        self.catalogue_hash
    }

    /// Returns the closed context that selects the catalogue hash contract.
    pub const fn catalogue_hash_context(&self) -> &CatalogueHashContext {
        &self.catalogue_hash_context
    }

    /// Classifies one record-field descriptor for durable storage.
    ///
    /// The candidate application catalogue and its pinned verified standard
    /// snapshot are the only classification authority.
    pub fn record_value_field_descriptor_class(
        &self,
        descriptor: &TypeDescriptor,
    ) -> Result<RecordValueFieldDescriptorClass, RecordValueFieldDescriptorError> {
        let standard = self
            .catalogue_hash_context
            .standard()
            .ok_or(RecordValueFieldDescriptorError::StandardLibraryUnavailable)?;
        classify_record_value_field_descriptor(&self.candidate, standard.catalogue(), descriptor)
            .map_err(|error| match error {
                RecordValueFieldDescriptorClassificationError::Unsupported => {
                    RecordValueFieldDescriptorError::Unsupported
                }
                RecordValueFieldDescriptorClassificationError::Ambiguous { type_id } => {
                    RecordValueFieldDescriptorError::Ambiguous { type_id }
                }
            })
    }

    /// Returns definition declaration origins.
    pub fn origins(&self) -> &[DefinitionOrigin] {
        &self.origins
    }

    /// Returns versioned compiled expression artifacts.
    pub fn expressions(&self) -> &[ExpressionArtifact] {
        &self.expressions
    }

    /// Returns only function revisions newly installed by this candidate.
    pub fn new_function_revisions(&self) -> &[FunctionRevisionRecord] {
        &self.new_function_revisions
    }

    /// Returns complete current function-revision evidence when it was
    /// supplied for explicit versioned construction.
    pub fn current_function_revisions(&self) -> Option<&[FunctionRevisionRecord]> {
        self.current_function_revisions.as_deref()
    }

    /// Returns resolved references for candidate current function revisions.
    pub fn references(&self) -> &[DefinitionReference] {
        &self.references
    }
}

/// Validates that a deployable catalogue can enter durable storage.
///
/// Revision construction rejects legacy scalar descriptors in a version-2
/// catalogue before the candidate can reach durable storage.
pub fn validate_persistable_catalogue(
    revision: &DeployableRevision,
) -> Result<(), RevisionInvariantError> {
    validate_resolved_type_slots(revision.catalogue_hash_context(), revision.candidate())?;
    validate_record_value_field_types(revision.catalogue_hash_context(), revision.candidate())
}

fn validate_source_units(units: &[StoredSourceUnit]) -> Result<(), RevisionInvariantError> {
    let mut ids = HashSet::with_capacity(units.len());
    let mut paths = HashMap::with_capacity(units.len());
    for (index, unit) in units.iter().enumerate() {
        let expected =
            u32::try_from(index).map_err(|_| RevisionInvariantError::SourceOrdinalOutOfRange {
                source_unit: unit.id,
            })?;
        if unit.ordinal != expected {
            return Err(RevisionInvariantError::SourceOrdinalOutOfSequence {
                source_unit: unit.id,
                expected,
                actual: unit.ordinal,
            });
        }
        if !ids.insert(unit.id) {
            return Err(RevisionInvariantError::DuplicateSourceUnitId {
                source_unit: unit.id,
            });
        }
        if let Some(first) = paths.insert(unit.logical_path.as_str(), unit.id) {
            return Err(RevisionInvariantError::DuplicateLogicalPath {
                logical_path: unit.logical_path.clone(),
                first,
                duplicate: unit.id,
            });
        }
    }
    Ok(())
}

fn validate_artifact_parts(
    format: &str,
    version: u32,
    payload: &[u8],
) -> Result<(), RevisionInvariantError> {
    if format.is_empty() {
        return Err(RevisionInvariantError::EmptyArtifactFormat);
    }
    if version == 0 {
        return Err(RevisionInvariantError::ZeroArtifactVersion {
            format: format.into(),
        });
    }
    if payload.is_empty() {
        return Err(RevisionInvariantError::EmptyArtifactPayload {
            format: format.into(),
        });
    }
    Ok(())
}

fn validate_pair(
    pair: &RevisionPair,
    source: &StoredSourceRevision,
    catalogue: &CatalogueSnapshot,
) -> Result<(), RevisionInvariantError> {
    if pair.source != source.id() {
        return Err(RevisionInvariantError::SourceRevisionPairMismatch {
            pair: pair.source,
            source: source.id(),
        });
    }
    if pair.catalogue != catalogue.revision() {
        return Err(RevisionInvariantError::CatalogueRevisionPairMismatch {
            pair: pair.catalogue,
            catalogue: catalogue.revision(),
        });
    }
    Ok(())
}

fn reject_offline_check_catalogue_revision(
    revision: CatalogueRevisionId,
    role: DurableCatalogueRevisionRole,
) -> Result<(), RevisionInvariantError> {
    if revision == EMPTY_APPLICATION_CATALOGUE_REVISION_ID {
        return Err(
            RevisionInvariantError::ReservedOfflineCheckCatalogueRevision { revision, role },
        );
    }
    Ok(())
}

fn validate_expressions(expressions: &[ExpressionArtifact]) -> Result<(), RevisionInvariantError> {
    let mut ids = HashSet::with_capacity(expressions.len());
    for expression in expressions {
        if !ids.insert(expression.id) {
            return Err(RevisionInvariantError::DuplicateExpressionId {
                expression: expression.id,
            });
        }
    }
    Ok(())
}

fn validate_origins(
    source: &StoredSourceRevision,
    catalogue: &CatalogueSnapshot,
    expressions: &[ExpressionArtifact],
    origins: &[DefinitionOrigin],
) -> Result<(), RevisionInvariantError> {
    let expression_ids = expressions
        .iter()
        .map(ExpressionArtifact::id)
        .collect::<HashSet<_>>();
    let mut identities = HashSet::with_capacity(origins.len());
    for origin in origins {
        validate_source_origin(source, origin.source)?;
        if !identities.insert(origin.identity) {
            return Err(RevisionInvariantError::DuplicateDefinitionOrigin {
                identity: origin.identity,
            });
        }
        if !definition_exists(catalogue, &expression_ids, origin.identity) {
            return Err(RevisionInvariantError::OriginDefinitionNotInRevision {
                identity: origin.identity,
            });
        }
    }
    for identity in expected_definition_identities(catalogue, expressions) {
        if !identities.contains(&identity) {
            return Err(RevisionInvariantError::MissingDefinitionOrigin { identity });
        }
    }
    Ok(())
}

fn validate_function_revisions(
    source: &StoredSourceRevision,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    revisions: &[FunctionRevisionRecord],
    set: FunctionRevisionSet,
) -> Result<(), RevisionInvariantError> {
    let mut revision_ids = HashSet::with_capacity(revisions.len());
    let mut function_numbers = HashSet::with_capacity(revisions.len());
    let mut function_ids = HashSet::with_capacity(revisions.len());

    for revision in revisions {
        if set == FunctionRevisionSet::NewCandidate {
            validate_source_origin(source, revision.declaration_origin)?;
        }
        if !revision_ids.insert(revision.id) {
            return Err(RevisionInvariantError::DuplicateFunctionRevisionId {
                revision: revision.id,
            });
        }
        if !function_numbers.insert((revision.function, revision.revision_number)) {
            return Err(RevisionInvariantError::DuplicateFunctionRevisionNumber {
                function: revision.function,
                revision_number: revision.revision_number,
            });
        }
        if !function_ids.insert(revision.function) {
            return Err(RevisionInvariantError::DuplicateFunctionRevisionFunction {
                function: revision.function,
            });
        }
        let function = catalogue.function_by_id(revision.function).ok_or(
            RevisionInvariantError::FunctionRevisionFunctionNotInCatalogue {
                function: revision.function,
                revision: revision.id,
            },
        )?;
        if function.current_revision() != revision.id {
            return Err(RevisionInvariantError::FunctionRevisionNotCurrent {
                function: revision.function,
                expected: function.current_revision(),
                actual: revision.id,
            });
        }
        let expected_artifact_kind = match function.domain() {
            FunctionDomain::Server => ExecutableArtifactKind::Server,
            FunctionDomain::Client => ExecutableArtifactKind::Client,
        };
        if revision.artifact.kind() != expected_artifact_kind {
            return Err(
                RevisionInvariantError::FunctionRevisionArtifactDomainMismatch {
                    function: revision.function,
                    revision: revision.id,
                    expected: expected_artifact_kind,
                    actual: revision.artifact.kind(),
                },
            );
        }
        if set == FunctionRevisionSet::NewCandidate {
            let identity = DefinitionIdentity::Function(revision.function);
            let origin = origins
                .iter()
                .find(|origin| origin.identity == identity)
                .ok_or(RevisionInvariantError::MissingDefinitionOrigin { identity })?;
            if origin.source != revision.declaration_origin {
                return Err(RevisionInvariantError::FunctionRevisionOriginMismatch {
                    function: revision.function,
                    revision: revision.id,
                    definition_origin: origin.source,
                    declaration_origin: revision.declaration_origin,
                });
            }
        }
    }

    for function in catalogue.functions() {
        if function_ids.contains(&function.id()) {
            continue;
        }
        match set {
            FunctionRevisionSet::RecoveredActive => {
                return Err(RevisionInvariantError::MissingActiveFunctionRevision {
                    function: function.id(),
                    revision: function.current_revision(),
                });
            }
            FunctionRevisionSet::DeployableCurrent => {
                return Err(
                    RevisionInvariantError::MissingDeployableCurrentFunctionRevision {
                        function: function.id(),
                        revision: function.current_revision(),
                    },
                );
            }
            FunctionRevisionSet::NewCandidate => {}
        }
    }
    Ok(())
}

fn validate_function_revision_history(
    current: &[FunctionRevisionRecord],
    historical: &[FunctionRevisionRecord],
) -> Result<(), RevisionInvariantError> {
    let mut revision_ids = current
        .iter()
        .map(FunctionRevisionRecord::id)
        .collect::<HashSet<_>>();
    let mut function_numbers = current
        .iter()
        .map(|revision| (revision.function(), revision.revision_number()))
        .collect::<HashSet<_>>();
    let mut function_hash_pairs = current
        .iter()
        .map(|revision| {
            (
                revision.function(),
                revision.declaration_content_hash(),
                revision.semantic_hash(),
            )
        })
        .collect::<HashSet<_>>();

    for revision in historical {
        if !revision_ids.insert(revision.id()) {
            return Err(RevisionInvariantError::DuplicateFunctionRevisionId {
                revision: revision.id(),
            });
        }
        if !function_numbers.insert((revision.function(), revision.revision_number())) {
            return Err(RevisionInvariantError::DuplicateFunctionRevisionNumber {
                function: revision.function(),
                revision_number: revision.revision_number(),
            });
        }
        if !function_hash_pairs.insert((
            revision.function(),
            revision.declaration_content_hash(),
            revision.semantic_hash(),
        )) {
            return Err(RevisionInvariantError::DuplicateFunctionRevisionHashPair {
                function: revision.function(),
                declaration_content_hash: revision.declaration_content_hash(),
                semantic_hash: revision.semantic_hash(),
            });
        }
    }

    Ok(())
}

fn validate_new_function_revision_subset(
    new_revisions: &[FunctionRevisionRecord],
    current_revisions: &[FunctionRevisionRecord],
) -> Result<(), RevisionInvariantError> {
    for new_revision in new_revisions {
        let matches_current = current_revisions
            .iter()
            .any(|current_revision| current_revision == new_revision);
        if !matches_current {
            return Err(
                RevisionInvariantError::NewFunctionRevisionCurrentEvidenceMismatch {
                    function: new_revision.function(),
                    revision: new_revision.id(),
                },
            );
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum FunctionRevisionSet {
    RecoveredActive,
    NewCandidate,
    DeployableCurrent,
}

fn validate_resolved_type_slots(
    context: &CatalogueHashContext,
    catalogue: &CatalogueSnapshot,
) -> Result<(), RevisionInvariantError> {
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
            FunctionReturn::Single(resolved_type) => validate_resolved_type_slot(
                context,
                DefinitionIdentity::Function(function.id()),
                *resolved_type,
                function_accepts_opaque_client_return(function),
            )?,
        }
    }

    Ok(())
}

fn validate_resolved_type_slot(
    context: &CatalogueHashContext,
    identity: DefinitionIdentity,
    resolved_type: ResolvedType,
    opaque_accepted: bool,
) -> Result<(), RevisionInvariantError> {
    match resolved_type {
        ResolvedType::Scalar(scalar) => {
            if matches!(context, CatalogueHashContext::Version2 { .. }) {
                return Err(
                    RevisionInvariantError::LegacyScalarRequiresCatalogueHashVersionOne {
                        identity,
                        scalar,
                    },
                );
            }
        }
        ResolvedType::Value(value_type) => match context {
            CatalogueHashContext::Version1 => {
                return Err(
                    RevisionInvariantError::ResolvedValueRequiresCatalogueHashVersionTwo {
                        identity,
                        value_type,
                    },
                );
            }
            CatalogueHashContext::Version2 { standard } => {
                let Some(value_type_definition) = standard.catalogue().value_type_by_id(value_type)
                else {
                    return Err(
                        RevisionInvariantError::ResolvedValueTypeNotInPinnedStandard {
                            identity,
                            value_type,
                        },
                    );
                };
                if value_type_definition.kind() == ValueTypeKind::Opaque && !opaque_accepted {
                    return Err(RevisionInvariantError::OpaqueValueTypeNotAcceptedInSlot {
                        identity,
                        value_type,
                    });
                }
            }
        },
        ResolvedType::Named(_) | ResolvedType::Reference { .. } => {}
    }

    Ok(())
}

pub(crate) fn function_accepts_opaque_client_return(function: &FunctionDefinition) -> bool {
    function.domain() == FunctionDomain::Client
        && function.parameters().is_empty()
        && matches!(
            function.return_type(),
            FunctionReturn::Single(ResolvedType::Value(_))
        )
        && function.security() == FunctionSecurity::Invoker
        && function.transaction().is_none()
        && function.volatility() == FunctionVolatility::Immutable
}

fn validate_catalogue_hash_context_coherence(
    context: &CatalogueHashContext,
    catalogue: &CatalogueSnapshot,
    revisions: &[FunctionRevisionRecord],
    origins: &[DefinitionOrigin],
    references: &[DefinitionReference],
) -> Result<(), RevisionInvariantError> {
    reject_reserved_invocation_carrier(catalogue)?;
    reject_reserved_system_function_identity(catalogue)?;
    if matches!(context, CatalogueHashContext::Version1) {
        reject_version_one_value_definitions(catalogue)?;
    }
    validate_resolved_type_slots(context, catalogue)?;
    validate_record_value_field_types(context, catalogue)?;
    match context {
        CatalogueHashContext::Version1 => {
            validate_catalogue_hash_context_version_one(catalogue, revisions, origins, references)
        }
        CatalogueHashContext::Version2 { .. } => {
            validate_catalogue_hash_context_version_two(revisions, references)
        }
    }
}

fn reject_reserved_invocation_carrier(
    catalogue: &CatalogueSnapshot,
) -> Result<(), RevisionInvariantError> {
    for carrier in INVOCATION_CARRIERS {
        if catalogue.type_definition_by_id(carrier.id()).is_some() {
            return Err(RevisionInvariantError::ReservedInvocationCarrierIdentity {
                carrier: carrier.id(),
            });
        }
    }
    for carrier in INVOCATION_CARRIERS {
        let name = QualifiedSemanticName::new(carrier.name_parts().iter().copied())
            .expect("the compiled invocation-carrier name must be valid");
        if let Some(type_id) = catalogue.type_id_by_name(&TypeLookupName::qualified(name)) {
            return Err(RevisionInvariantError::ReservedInvocationCarrierName { type_id });
        }
    }
    Ok(())
}

fn reject_reserved_system_function_identity(
    catalogue: &CatalogueSnapshot,
) -> Result<(), RevisionInvariantError> {
    for system_function in SYSTEM_FUNCTIONS {
        if catalogue.function_by_id(system_function.id()).is_some() {
            return Err(RevisionInvariantError::ReservedSystemFunctionIdentity {
                function: system_function.id(),
            });
        }
    }
    for system_function in SYSTEM_FUNCTIONS {
        if let Some(function) = catalogue
            .functions()
            .iter()
            .find(|function| system_function.has_name(function.name()))
        {
            return Err(RevisionInvariantError::ReservedSystemFunctionName {
                function: function.id(),
            });
        }
    }
    Ok(())
}

fn reject_version_one_value_definitions(
    catalogue: &CatalogueSnapshot,
) -> Result<(), RevisionInvariantError> {
    if let Some(record_value_type) = catalogue.record_value_types().first() {
        return Err(
            RevisionInvariantError::RecordValueTypeRequiresCatalogueHashVersionTwo {
                record_value_type: record_value_type.id(),
            },
        );
    }
    if let Some(opaque) = catalogue
        .value_types()
        .iter()
        .find(|value_type| value_type.kind() == ValueTypeKind::Opaque)
    {
        return Err(
            RevisionInvariantError::ValueTypeDefinitionRequiresCatalogueHashVersionTwo {
                value_type: opaque.id(),
            },
        );
    }
    Ok(())
}

fn validate_record_value_field_types(
    context: &CatalogueHashContext,
    catalogue: &CatalogueSnapshot,
) -> Result<(), RevisionInvariantError> {
    let CatalogueHashContext::Version2 { standard } = context else {
        return Ok(());
    };
    validate_record_value_field_descriptors(catalogue, standard.catalogue()).map_err(|error| {
        match error {
            RecordValueFieldDescriptorValidationError::Unsupported {
                record_value_type,
                field,
                descriptor,
            } => RevisionInvariantError::UnsupportedRecordValueFieldType {
                record_value_type,
                field,
                descriptor,
            },
            RecordValueFieldDescriptorValidationError::Ambiguous {
                record_value_type,
                field,
                type_id,
            } => RevisionInvariantError::AmbiguousRecordValueFieldType {
                record_value_type,
                field,
                type_id,
            },
            RecordValueFieldDescriptorValidationError::RecursiveRecordValueField {
                record_value_type,
                field,
                nested_record_value_type,
            } => RevisionInvariantError::RecursiveRecordValueField {
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
            } => RevisionInvariantError::RecordValueNestingTooDeep {
                record_value_type,
                field,
                nested_record_value_type,
                maximum,
                actual,
            },
        }
    })
}

/// The durable storage class of one admitted record-value field descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RecordValueFieldDescriptorClass {
    /// An application enum identity.
    ApplicationEnum(TypeId),
    /// An application record-value identity.
    ApplicationRecord(TypeId),
    /// A pinned-standard enum identity.
    StandardEnum(TypeId),
    /// An accepted immutable, persistable pinned-standard primitive identity.
    StandardPrimitive(TypeId),
}

/// An error classifying a record-value field descriptor for durable storage.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RecordValueFieldDescriptorError {
    /// The deployable revision does not pin a verified standard library.
    StandardLibraryUnavailable,
    /// The descriptor is outside the accepted record-field family.
    Unsupported,
    /// The identity selects incompatible application and standard definitions.
    Ambiguous { type_id: TypeId },
}

impl fmt::Display for RecordValueFieldDescriptorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StandardLibraryUnavailable => formatter.write_str(
                "deployable revision has no pinned standard library for record field classification",
            ),
            Self::Unsupported => formatter.write_str(
                "record field descriptor is not supported by the deployable revision",
            ),
            Self::Ambiguous { .. } => formatter.write_str(
                "record field type is present in both application and standard catalogues",
            ),
        }
    }
}

impl Error for RecordValueFieldDescriptorError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RecordValueFieldDescriptorClassificationError {
    Unsupported,
    Ambiguous { type_id: TypeId },
}

/// A failure that prevents a candidate from admitting record-value fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RecordValueFieldDescriptorValidationError {
    Unsupported {
        record_value_type: TypeId,
        field: FieldId,
        descriptor: TypeDescriptor,
    },
    Ambiguous {
        record_value_type: TypeId,
        field: FieldId,
        type_id: TypeId,
    },
    RecursiveRecordValueField {
        record_value_type: TypeId,
        field: FieldId,
        nested_record_value_type: TypeId,
    },
    RecordValueNestingTooDeep {
        record_value_type: TypeId,
        field: FieldId,
        nested_record_value_type: TypeId,
        maximum: u32,
        actual: u32,
    },
}

const MAXIMUM_RECORD_VALUE_NESTING: u32 = 32;

#[derive(Clone, Copy)]
struct RecordValueFieldGraphEdge {
    field: FieldId,
    nested_record_value_type: TypeId,
}

#[derive(Clone, Copy)]
struct RecordValueFieldCycleFrame {
    owner: TypeId,
    next_edge: usize,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum RecordValueFieldGraphVisitState {
    Grey,
    Black,
}

/// Classifies every record field before it checks the complete dependency graph.
///
/// The order and phase boundaries are part of the version-2 catalogue contract.
pub(crate) fn validate_record_value_field_descriptors(
    catalogue: &CatalogueSnapshot,
    standard: &CatalogueSnapshot,
) -> Result<(), RecordValueFieldDescriptorValidationError> {
    let mut record_value_types = catalogue.record_value_types().iter().collect::<Vec<_>>();
    record_value_types.sort_by_key(|record_value_type| record_value_type.id().to_bytes());

    let mut edges_by_owner = HashMap::with_capacity(record_value_types.len());
    for record_value_type in &record_value_types {
        let mut fields = record_value_type.fields().iter().collect::<Vec<_>>();
        fields.sort_by_key(|field| (field.ordinal(), field.id().to_bytes()));

        let mut outgoing = Vec::new();
        for field in fields {
            match classify_record_value_field_descriptor(catalogue, standard, field.descriptor()) {
                Ok(RecordValueFieldDescriptorClass::ApplicationRecord(
                    nested_record_value_type,
                )) => outgoing.push(RecordValueFieldGraphEdge {
                    field: field.id(),
                    nested_record_value_type,
                }),
                Ok(
                    RecordValueFieldDescriptorClass::ApplicationEnum(_)
                    | RecordValueFieldDescriptorClass::StandardEnum(_)
                    | RecordValueFieldDescriptorClass::StandardPrimitive(_),
                ) => {}
                Err(RecordValueFieldDescriptorClassificationError::Unsupported) => {
                    return Err(RecordValueFieldDescriptorValidationError::Unsupported {
                        record_value_type: record_value_type.id(),
                        field: field.id(),
                        descriptor: field.descriptor().clone(),
                    });
                }
                Err(RecordValueFieldDescriptorClassificationError::Ambiguous { type_id }) => {
                    return Err(RecordValueFieldDescriptorValidationError::Ambiguous {
                        record_value_type: record_value_type.id(),
                        field: field.id(),
                        type_id,
                    });
                }
            }
        }
        edges_by_owner.insert(record_value_type.id(), outgoing);
    }

    let roots = record_value_types
        .iter()
        .map(|record_value_type| record_value_type.id())
        .collect::<Vec<_>>();
    validate_record_value_field_cycles(&roots, &edges_by_owner)?;
    validate_record_value_field_nesting(&roots, &edges_by_owner)
}

fn validate_record_value_field_cycles(
    roots: &[TypeId],
    edges_by_owner: &HashMap<TypeId, Vec<RecordValueFieldGraphEdge>>,
) -> Result<(), RecordValueFieldDescriptorValidationError> {
    let mut states = HashMap::with_capacity(roots.len());
    for &root in roots {
        if states.contains_key(&root) {
            continue;
        }
        states.insert(root, RecordValueFieldGraphVisitState::Grey);
        let mut stack = vec![RecordValueFieldCycleFrame {
            owner: root,
            next_edge: 0,
        }];

        loop {
            let next = {
                let Some(frame) = stack.last_mut() else {
                    break;
                };
                let owner = frame.owner;
                let edge = edges_by_owner
                    .get(&owner)
                    .and_then(|edges| edges.get(frame.next_edge))
                    .copied();
                if edge.is_some() {
                    frame.next_edge += 1;
                }
                edge.map(|edge| (owner, edge))
            };

            let Some((owner, edge)) = next else {
                let completed = stack
                    .pop()
                    .expect("record cycle stack must retain the completed frame");
                states.insert(completed.owner, RecordValueFieldGraphVisitState::Black);
                continue;
            };

            match states.get(&edge.nested_record_value_type) {
                Some(RecordValueFieldGraphVisitState::Grey) => {
                    return Err(
                        RecordValueFieldDescriptorValidationError::RecursiveRecordValueField {
                            record_value_type: owner,
                            field: edge.field,
                            nested_record_value_type: edge.nested_record_value_type,
                        },
                    );
                }
                Some(RecordValueFieldGraphVisitState::Black) => {}
                None => {
                    states.insert(
                        edge.nested_record_value_type,
                        RecordValueFieldGraphVisitState::Grey,
                    );
                    stack.push(RecordValueFieldCycleFrame {
                        owner: edge.nested_record_value_type,
                        next_edge: 0,
                    });
                }
            }
        }
    }
    Ok(())
}

fn validate_record_value_field_nesting(
    roots: &[TypeId],
    edges_by_owner: &HashMap<TypeId, Vec<RecordValueFieldGraphEdge>>,
) -> Result<(), RecordValueFieldDescriptorValidationError> {
    let mut greatest_validated_depth = HashMap::with_capacity(roots.len());
    for &root in roots {
        visit_record_value_field_nesting(root, 0, edges_by_owner, &mut greatest_validated_depth)?;
    }
    Ok(())
}

fn visit_record_value_field_nesting(
    owner: TypeId,
    depth: u32,
    edges_by_owner: &HashMap<TypeId, Vec<RecordValueFieldGraphEdge>>,
    greatest_validated_depth: &mut HashMap<TypeId, u32>,
) -> Result<(), RecordValueFieldDescriptorValidationError> {
    if greatest_validated_depth
        .get(&owner)
        .is_some_and(|previous| *previous >= depth)
    {
        return Ok(());
    }
    greatest_validated_depth.insert(owner, depth);

    if let Some(edges) = edges_by_owner.get(&owner) {
        for edge in edges {
            let nested_depth = depth + 1;
            if nested_depth > MAXIMUM_RECORD_VALUE_NESTING {
                return Err(
                    RecordValueFieldDescriptorValidationError::RecordValueNestingTooDeep {
                        record_value_type: owner,
                        field: edge.field,
                        nested_record_value_type: edge.nested_record_value_type,
                        maximum: MAXIMUM_RECORD_VALUE_NESTING,
                        actual: nested_depth,
                    },
                );
            }
            visit_record_value_field_nesting(
                edge.nested_record_value_type,
                nested_depth,
                edges_by_owner,
                greatest_validated_depth,
            )?;
        }
    }
    Ok(())
}

pub(crate) fn classify_record_value_field_descriptor(
    catalogue: &CatalogueSnapshot,
    standard: &CatalogueSnapshot,
    descriptor: &TypeDescriptor,
) -> Result<RecordValueFieldDescriptorClass, RecordValueFieldDescriptorClassificationError> {
    let TypeDescriptorKind::Named(type_id) = descriptor.kind() else {
        return Err(RecordValueFieldDescriptorClassificationError::Unsupported);
    };
    let application_enum = catalogue.enum_type_by_id(type_id).is_some();
    let application_record = catalogue.record_value_type_by_id(type_id).is_some();
    let standard_enum = standard.enum_type_by_id(type_id).is_some();
    let standard_scalar = standard
        .value_type_by_id(type_id)
        .and_then(accepted_record_scalar);
    if (application_enum || application_record) && (standard_enum || standard_scalar.is_some()) {
        return Err(RecordValueFieldDescriptorClassificationError::Ambiguous { type_id });
    }
    if application_enum {
        return Ok(RecordValueFieldDescriptorClass::ApplicationEnum(type_id));
    }
    if application_record {
        return Ok(RecordValueFieldDescriptorClass::ApplicationRecord(type_id));
    }
    if standard_enum {
        return Ok(RecordValueFieldDescriptorClass::StandardEnum(type_id));
    }
    if standard_scalar.is_some() {
        return Ok(RecordValueFieldDescriptorClass::StandardPrimitive(type_id));
    }
    Err(RecordValueFieldDescriptorClassificationError::Unsupported)
}

pub(crate) fn record_value_field_runtime_type(
    catalogue: &CatalogueSnapshot,
    standard: &CatalogueSnapshot,
    resolved_type: ResolvedType,
) -> Option<ResolvedType> {
    match resolved_type {
        ResolvedType::Value(value_type) => standard
            .value_type_by_id(value_type)
            .and_then(accepted_record_scalar)
            .map(ResolvedType::scalar),
        ResolvedType::Named(enum_type) => (catalogue.enum_type_by_id(enum_type).is_some()
            || standard.enum_type_by_id(enum_type).is_some())
        .then_some(ResolvedType::named(enum_type)),
        ResolvedType::Scalar(_) | ResolvedType::Reference { .. } => None,
    }
}

fn accepted_record_scalar(value_type: &ValueTypeDefinition) -> Option<StandardScalar> {
    if value_type.kind() != ValueTypeKind::Primitive
        || value_type.mutability() != ValueTypeMutability::Immutable
        || value_type.persistence() != ValueTypePersistence::Persistable
    {
        return None;
    }
    match value_type.representation_contract() {
        "orna.kernel.value.boolean@1" => Some(StandardScalar::Boolean),
        "orna.kernel.value.integer@1" => Some(StandardScalar::Integer),
        "orna.kernel.value.bigint@1" => Some(StandardScalar::BigInt),
        "orna.kernel.value.float@1" => Some(StandardScalar::Float),
        "orna.kernel.value.character-large-object@1" => Some(StandardScalar::CharacterLargeObject),
        "orna.kernel.value.binary-large-object@1" => Some(StandardScalar::BinaryLargeObject),
        _ => None,
    }
}

fn validate_catalogue_hash_context_version_one(
    catalogue: &CatalogueSnapshot,
    revisions: &[FunctionRevisionRecord],
    origins: &[DefinitionOrigin],
    references: &[DefinitionReference],
) -> Result<(), RevisionInvariantError> {
    if let Some(enum_type) = catalogue.enum_types().first() {
        return Err(
            RevisionInvariantError::ValueTypeDefinitionRequiresCatalogueHashVersionTwo {
                value_type: enum_type.id(),
            },
        );
    }
    if let Some(value_type) = catalogue.value_types().first() {
        return Err(
            RevisionInvariantError::ValueTypeDefinitionRequiresCatalogueHashVersionTwo {
                value_type: value_type.id(),
            },
        );
    }
    if let Some(binding) = catalogue.type_bindings().first() {
        return Err(
            RevisionInvariantError::TypeBindingRequiresCatalogueHashVersionTwo {
                binding: binding.id(),
            },
        );
    }
    for origin in origins {
        match origin.identity() {
            DefinitionIdentity::ValueType(_) | DefinitionIdentity::TypeBinding(_) => {
                return Err(
                    RevisionInvariantError::DefinitionOriginRequiresCatalogueHashVersionTwo {
                        identity: origin.identity(),
                    },
                );
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
            DefinitionReferenceTarget::ValueType(target) => {
                return Err(
                    RevisionInvariantError::ValueTypeReferenceRequiresCatalogueHashVersionTwo {
                        function: reference.source_function(),
                        revision: reference.source_revision(),
                        target,
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
    for revision in revisions {
        match revision.semantic_hash_version() {
            FunctionSemanticHashVersion::Version1 => {}
            FunctionSemanticHashVersion::Version2 => {
                return Err(RevisionInvariantError::FunctionSemanticHashVersionRequiresCatalogueHashVersionTwo {
                    function: revision.function(),
                    revision: revision.id(),
                });
            }
        }
    }
    Ok(())
}

fn validate_catalogue_hash_context_version_two(
    revisions: &[FunctionRevisionRecord],
    references: &[DefinitionReference],
) -> Result<(), RevisionInvariantError> {
    for reference in references {
        let target = match reference.target() {
            DefinitionReferenceTarget::ValueType(target) => target,
            DefinitionReferenceTarget::ObjectType(_)
            | DefinitionReferenceTarget::Field { .. }
            | DefinitionReferenceTarget::Function(_)
            | DefinitionReferenceTarget::Parameter { .. }
            | DefinitionReferenceTarget::Expression(_) => continue,
        };
        let revision = revisions
            .iter()
            .find(|revision| revision.id() == reference.source_revision())
            .ok_or(
                RevisionInvariantError::ValueTypeReferenceFunctionRevisionUnavailable {
                    function: reference.source_function(),
                    revision: reference.source_revision(),
                    target,
                },
            )?;
        match revision.semantic_hash_version() {
            FunctionSemanticHashVersion::Version1 => {
                return Err(
                    RevisionInvariantError::ValueTypeReferenceRequiresFunctionSemanticHashVersionTwo {
                        function: reference.source_function(),
                        revision: reference.source_revision(),
                        target,
                    },
                );
            }
            FunctionSemanticHashVersion::Version2 => {}
        }
    }
    Ok(())
}

fn validate_references(
    source: &StoredSourceRevision,
    catalogue: &CatalogueSnapshot,
    standard: Option<&CatalogueSnapshot>,
    expressions: &[ExpressionArtifact],
    revisions: &[FunctionRevisionRecord],
    references: &[DefinitionReference],
) -> Result<(), RevisionInvariantError> {
    let revision_by_id = revisions
        .iter()
        .map(|revision| (revision.id, revision))
        .collect::<HashMap<_, _>>();
    let mut ordinals = HashSet::with_capacity(references.len());
    let expression_ids = expressions
        .iter()
        .map(ExpressionArtifact::id)
        .collect::<HashSet<_>>();

    for reference in references {
        validate_source_origin(source, reference.source_origin)?;
        if !reference_target_exists(catalogue, standard, &expression_ids, reference.target) {
            return Err(RevisionInvariantError::ReferenceTargetNotInRevision {
                target: reference.target,
            });
        }
        if !reference_kind_accepts_target(reference.kind, reference.target) {
            return Err(RevisionInvariantError::ReferenceKindTargetMismatch {
                kind: reference.kind,
                target: reference.target,
            });
        }
        let function = catalogue.function_by_id(reference.source_function).ok_or(
            RevisionInvariantError::ReferenceFunctionNotInCatalogue {
                function: reference.source_function,
                revision: reference.source_revision,
            },
        )?;
        if function.current_revision() != reference.source_revision {
            return Err(RevisionInvariantError::ReferenceRevisionNotCurrent {
                function: reference.source_function,
                expected: function.current_revision(),
                actual: reference.source_revision,
            });
        }
        if let Some(revision) = revision_by_id.get(&reference.source_revision)
            && revision.function != reference.source_function
        {
            return Err(RevisionInvariantError::ReferenceFunctionRevisionMismatch {
                function: reference.source_function,
                revision: reference.source_revision,
            });
        }
        if !ordinals.insert((reference.source_revision, reference.ordinal)) {
            return Err(RevisionInvariantError::DuplicateReferenceOrdinal {
                revision: reference.source_revision,
                ordinal: reference.ordinal,
            });
        }
    }
    Ok(())
}

fn validate_standard_executables(
    source: &StoredSourceRevision,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    executables: &[StandardExecutable],
) -> Result<(), RevisionInvariantError> {
    if catalogue.functions().len() != executables.len() {
        return Err(
            RevisionInvariantError::StandardExecutableSequenceLengthMismatch {
                catalogue_functions: catalogue.functions().len(),
                executables: executables.len(),
            },
        );
    }

    let revisions = executables
        .iter()
        .map(|executable| executable.revision().clone())
        .collect::<Vec<_>>();
    let mut references = Vec::new();
    let mut previous_function = None;
    for (index, (function, executable)) in catalogue
        .functions()
        .iter()
        .zip(executables.iter())
        .enumerate()
    {
        if let Some(previous) = previous_function
            && previous >= function.id()
        {
            return Err(
                RevisionInvariantError::StandardExecutableCatalogueFunctionOrder {
                    ordinal: index,
                    previous,
                    actual: function.id(),
                },
            );
        }
        previous_function = Some(function.id());
        if function.id() != executable.function() {
            return Err(
                RevisionInvariantError::StandardExecutableSequenceFunctionMismatch {
                    ordinal: index,
                    catalogue_function: function.id(),
                    executable_function: executable.function(),
                },
            );
        }
        if executable.revision().semantic_hash_version() != FunctionSemanticHashVersion::Version2 {
            return Err(
                RevisionInvariantError::StandardExecutableSemanticHashVersionMismatch {
                    function: executable.function(),
                    revision: executable.revision().id(),
                    version: executable.revision().semantic_hash_version(),
                },
            );
        }
        for (ordinal, reference) in executable.references().iter().enumerate() {
            let expected = u32::try_from(ordinal).map_err(|_| {
                RevisionInvariantError::StandardExecutableReferenceOrdinalOutOfRange {
                    function: executable.function(),
                    revision: executable.revision().id(),
                }
            })?;
            if reference.ordinal() != expected {
                return Err(
                    RevisionInvariantError::StandardExecutableReferenceOrdinalOutOfSequence {
                        function: executable.function(),
                        revision: executable.revision().id(),
                        expected,
                        actual: reference.ordinal(),
                    },
                );
            }
            if reference.source_function() != executable.function()
                || reference.source_revision() != executable.revision().id()
            {
                return Err(
                    RevisionInvariantError::StandardExecutableReferenceOwnerMismatch {
                        function: executable.function(),
                        revision: executable.revision().id(),
                        reference_function: reference.source_function(),
                        reference_revision: reference.source_revision(),
                    },
                );
            }
            references.push(reference.clone());
        }
    }

    validate_function_revisions(
        source,
        catalogue,
        origins,
        &revisions,
        FunctionRevisionSet::NewCandidate,
    )?;
    validate_references(source, catalogue, None, &[], &revisions, &references)
}

fn validate_source_origin(
    source: &StoredSourceRevision,
    origin: SourceOrigin,
) -> Result<(), RevisionInvariantError> {
    let unit = source.source_unit(origin.source_unit).ok_or(
        RevisionInvariantError::SourceOriginUnitNotInRevision {
            source_unit: origin.source_unit,
        },
    )?;
    let content_length = u32::try_from(unit.content.len()).map_err(|_| {
        RevisionInvariantError::SourceContentTooLarge {
            source_unit: unit.id,
        }
    })?;
    if origin.byte_end > content_length {
        return Err(RevisionInvariantError::SourceOriginOutOfBounds {
            source_unit: origin.source_unit,
            byte_start: origin.byte_start,
            byte_end: origin.byte_end,
            content_length,
        });
    }
    if !unit.content.is_char_boundary(origin.byte_start as usize)
        || !unit.content.is_char_boundary(origin.byte_end as usize)
    {
        return Err(RevisionInvariantError::SourceOriginNotCharacterBoundary {
            source_unit: origin.source_unit,
            byte_start: origin.byte_start,
            byte_end: origin.byte_end,
        });
    }
    Ok(())
}

fn expected_definition_identities(
    catalogue: &CatalogueSnapshot,
    expressions: &[ExpressionArtifact],
) -> Vec<DefinitionIdentity> {
    let mut identities = Vec::new();
    identities.extend(
        catalogue
            .schemas()
            .iter()
            .map(|schema| DefinitionIdentity::Schema(schema.id())),
    );
    for object_type in catalogue.object_types() {
        identities.push(DefinitionIdentity::ObjectType(object_type.id()));
        identities.extend(
            object_type
                .fields()
                .iter()
                .map(|field| DefinitionIdentity::Field {
                    owner: object_type.id(),
                    field: field.id(),
                }),
        );
    }
    identities.extend(
        catalogue
            .value_types()
            .iter()
            .map(|value_type| DefinitionIdentity::ValueType(value_type.id())),
    );
    identities.extend(
        catalogue
            .enum_types()
            .iter()
            .map(|enum_type| DefinitionIdentity::ValueType(enum_type.id())),
    );
    for record_value_type in catalogue.record_value_types() {
        identities.push(DefinitionIdentity::ValueType(record_value_type.id()));
        identities.extend(record_value_type.fields().iter().map(|field| {
            DefinitionIdentity::Field {
                owner: record_value_type.id(),
                field: field.id(),
            }
        }));
    }
    identities.extend(
        catalogue
            .type_bindings()
            .iter()
            .map(|binding| DefinitionIdentity::TypeBinding(binding.id())),
    );
    for function in catalogue.functions() {
        identities.push(DefinitionIdentity::Function(function.id()));
        identities.extend(function.parameters().iter().map(|parameter| {
            DefinitionIdentity::Parameter {
                owner: function.id(),
                parameter: parameter.id(),
            }
        }));
        if let FunctionReturn::Rows(columns) = function.return_type() {
            identities.extend(columns.iter().map(|column| {
                DefinitionIdentity::FunctionReturnColumn {
                    owner: function.id(),
                    ordinal: column.ordinal(),
                }
            }));
        }
    }
    identities.extend(
        expressions
            .iter()
            .map(|expression| DefinitionIdentity::Expression(expression.id())),
    );
    identities
}

fn definition_exists(
    catalogue: &CatalogueSnapshot,
    expression_ids: &HashSet<ExpressionId>,
    identity: DefinitionIdentity,
) -> bool {
    match identity {
        DefinitionIdentity::Schema(id) => catalogue.schema_by_id(id).is_some(),
        DefinitionIdentity::ObjectType(id) => catalogue.object_type_by_id(id).is_some(),
        DefinitionIdentity::ValueType(id) => {
            catalogue.value_type_by_id(id).is_some()
                || catalogue.enum_type_by_id(id).is_some()
                || catalogue.record_value_type_by_id(id).is_some()
        }
        DefinitionIdentity::TypeBinding(id) => catalogue.type_binding_by_id(id).is_some(),
        DefinitionIdentity::Field { owner, field } => {
            catalogue
                .object_type_by_id(owner)
                .and_then(|object_type| object_type.field_by_id(field))
                .is_some()
                || catalogue
                    .record_value_type_by_id(owner)
                    .and_then(|record_value_type| record_value_type.field_by_id(field))
                    .is_some()
        }
        DefinitionIdentity::Function(id) => catalogue.function_by_id(id).is_some(),
        DefinitionIdentity::Parameter { owner, parameter } => catalogue
            .function_by_id(owner)
            .and_then(|function| function.parameter_by_id(parameter))
            .is_some(),
        DefinitionIdentity::FunctionReturnColumn { owner, ordinal } => catalogue
            .function_by_id(owner)
            .and_then(|function| match function.return_type() {
                FunctionReturn::Single(_) => None,
                FunctionReturn::Rows(columns) => columns.get(ordinal as usize),
            })
            .is_some(),
        DefinitionIdentity::Expression(id) => expression_ids.contains(&id),
    }
}

fn reference_target_exists(
    catalogue: &CatalogueSnapshot,
    standard: Option<&CatalogueSnapshot>,
    expression_ids: &HashSet<ExpressionId>,
    target: DefinitionReferenceTarget,
) -> bool {
    if let DefinitionReferenceTarget::Field { owner, field } = target {
        return catalogue
            .object_type_by_id(owner)
            .and_then(|object_type| object_type.field_by_id(field))
            .is_some()
            || catalogue
                .record_value_type_by_id(owner)
                .and_then(|record_value_type| record_value_type.field_by_id(field))
                .is_some()
            || standard.is_some_and(|standard| {
                standard
                    .object_type_by_id(owner)
                    .and_then(|object_type| object_type.field_by_id(field))
                    .is_some()
                    || standard
                        .record_value_type_by_id(owner)
                        .and_then(|record_value_type| record_value_type.field_by_id(field))
                        .is_some()
            });
    }
    let identity = target.into();
    definition_exists(catalogue, expression_ids, identity)
        || standard.is_some_and(|standard| definition_exists(standard, expression_ids, identity))
}

pub(crate) fn reference_kind_accepts_target(
    kind: DefinitionReferenceKind,
    target: DefinitionReferenceTarget,
) -> bool {
    let target = match target {
        DefinitionReferenceTarget::ObjectType(_) => ReferenceTargetKind::ObjectType,
        DefinitionReferenceTarget::ValueType(_) => ReferenceTargetKind::ValueType,
        DefinitionReferenceTarget::Field { .. } => ReferenceTargetKind::Field,
        DefinitionReferenceTarget::Function(_) => ReferenceTargetKind::Function,
        DefinitionReferenceTarget::Parameter { .. } => ReferenceTargetKind::Parameter,
        DefinitionReferenceTarget::Expression(_) => ReferenceTargetKind::Expression,
    };

    match kind {
        DefinitionReferenceKind::FunctionCall => target == ReferenceTargetKind::Function,
        DefinitionReferenceKind::NamedType => {
            target == ReferenceTargetKind::ObjectType || target == ReferenceTargetKind::ValueType
        }
        DefinitionReferenceKind::ObjectReference
        | DefinitionReferenceKind::QueryObject
        | DefinitionReferenceKind::WriteObject => target == ReferenceTargetKind::ObjectType,
        DefinitionReferenceKind::ParameterRead => target == ReferenceTargetKind::Parameter,
        DefinitionReferenceKind::QueryField | DefinitionReferenceKind::WriteField => {
            target == ReferenceTargetKind::Field
        }
        DefinitionReferenceKind::Expression => target == ReferenceTargetKind::Expression,
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ReferenceTargetKind {
    ObjectType,
    ValueType,
    Field,
    Function,
    Parameter,
    Expression,
}

/// An error returned when durable revision records violate a local invariant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RevisionInvariantError {
    /// A source range has its end before its start.
    SourceOriginReversed {
        source_unit: SourceUnitId,
        byte_start: u32,
        byte_end: u32,
    },
    /// A source unit has no logical path.
    EmptyLogicalPath { source_unit: SourceUnitId },
    /// Stored source unit order is not contiguous and zero-based.
    SourceOrdinalOutOfSequence {
        source_unit: SourceUnitId,
        expected: u32,
        actual: u32,
    },
    /// A source collection is too large for a `u32` ordinal.
    SourceOrdinalOutOfRange { source_unit: SourceUnitId },
    /// A source revision contains the same source-unit identity twice.
    DuplicateSourceUnitId { source_unit: SourceUnitId },
    /// A source revision contains the same logical path twice.
    DuplicateLogicalPath {
        logical_path: String,
        first: SourceUnitId,
        duplicate: SourceUnitId,
    },
    /// A source revision names itself as its parent.
    SourceRevisionSelfParent { revision: SourceRevisionId },
    /// A standard-library source revision has a parent.
    StandardLibrarySourceHasParent {
        source: SourceRevisionId,
        parent: Option<SourceRevisionId>,
    },
    /// A standard-library revision has no compatible language label.
    EmptyStandardLibraryLanguageVersion { revision: StandardLibraryRevisionId },
    /// A standard-library catalogue contains a definition outside its digest model.
    UnsupportedStandardLibraryDefinition { revision: StandardLibraryRevisionId },
    /// A version-1 standard-library snapshot retained executable evidence.
    VersionOneStandardLibraryHasExecutable { revision: StandardLibraryRevisionId },
    /// A version-2 standard-library source revision omitted its parent.
    VersionTwoStandardLibrarySourceHasNoParent { source: SourceRevisionId },
    /// An executable record disagrees with its immutable function revision.
    StandardExecutableFunctionMismatch {
        function: FunctionId,
        revision_function: FunctionId,
        revision: FunctionRevisionId,
    },
    /// The executable sequence does not cover the catalogue function sequence.
    StandardExecutableSequenceLengthMismatch {
        catalogue_functions: usize,
        executables: usize,
    },
    /// A standard executable occurs at a different function position.
    StandardExecutableSequenceFunctionMismatch {
        ordinal: usize,
        catalogue_function: FunctionId,
        executable_function: FunctionId,
    },
    /// The version-2 standard catalogue function sequence is not canonical.
    StandardExecutableCatalogueFunctionOrder {
        ordinal: usize,
        previous: FunctionId,
        actual: FunctionId,
    },
    /// A standard executable uses a semantic-hash version outside the V2 contract.
    StandardExecutableSemanticHashVersionMismatch {
        function: FunctionId,
        revision: FunctionRevisionId,
        version: FunctionSemanticHashVersion,
    },
    /// A standard executable reference index cannot fit its durable ordinal.
    StandardExecutableReferenceOrdinalOutOfRange {
        function: FunctionId,
        revision: FunctionRevisionId,
    },
    /// A standard executable reference ordinal is not contiguous and zero-based.
    StandardExecutableReferenceOrdinalOutOfSequence {
        function: FunctionId,
        revision: FunctionRevisionId,
        expected: u32,
        actual: u32,
    },
    /// A standard executable reference names a different immutable owner.
    StandardExecutableReferenceOwnerMismatch {
        function: FunctionId,
        revision: FunctionRevisionId,
        reference_function: FunctionId,
        reference_revision: FunctionRevisionId,
    },
    /// An artifact format identifier is empty.
    EmptyArtifactFormat,
    /// An artifact format version is zero.
    ZeroArtifactVersion { format: String },
    /// An artifact contains no payload bytes.
    EmptyArtifactPayload { format: String },
    /// A function revision number is zero.
    ZeroFunctionRevisionNumber {
        function: FunctionId,
        id: FunctionRevisionId,
    },
    /// A function revision has no language version label.
    EmptyLanguageVersion {
        function: FunctionId,
        id: FunctionRevisionId,
    },
    /// A resolved value identity was paired with a version-1 catalogue hash.
    ResolvedValueRequiresCatalogueHashVersionTwo {
        /// The catalogue slot containing the incompatible resolved value.
        identity: DefinitionIdentity,
        /// The supplied durable standard value-type identity.
        value_type: TypeId,
    },
    /// A legacy scalar descriptor was selected for durable version-2 storage.
    LegacyScalarRequiresCatalogueHashVersionOne {
        /// The catalogue slot containing the incompatible legacy scalar.
        identity: DefinitionIdentity,
        /// The supplied compatibility scalar.
        scalar: StandardScalar,
    },
    /// A resolved value identity is absent from the pinned verified standard.
    ResolvedValueTypeNotInPinnedStandard {
        /// The catalogue slot containing the unresolved value identity.
        identity: DefinitionIdentity,
        /// The absent durable standard value-type identity.
        value_type: TypeId,
    },
    /// A catalogue slot uses a transient opaque value type.
    OpaqueValueTypeNotAcceptedInSlot {
        /// The catalogue slot containing the rejected opaque value.
        identity: DefinitionIdentity,
        /// The rejected opaque value type identity.
        value_type: TypeId,
    },
    /// A version-2 function semantic hash was paired with a version-1 catalogue hash.
    FunctionSemanticHashVersionRequiresCatalogueHashVersionTwo {
        /// The function whose immutable revision uses the newer contract.
        function: FunctionId,
        /// The incompatible immutable function revision.
        revision: FunctionRevisionId,
    },
    /// A value-type definition was paired with a version-1 catalogue hash.
    ValueTypeDefinitionRequiresCatalogueHashVersionTwo {
        /// The incompatible value-type definition.
        value_type: TypeId,
    },
    /// A record value type was paired with a version-1 catalogue hash.
    RecordValueTypeRequiresCatalogueHashVersionTwo {
        /// The incompatible record value type.
        record_value_type: TypeId,
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
    /// A type-name binding was paired with a version-1 catalogue hash.
    TypeBindingRequiresCatalogueHashVersionTwo {
        /// The incompatible direct binding.
        binding: TypeBindingId,
    },
    /// A value-type or binding origin was paired with a version-1 catalogue hash.
    DefinitionOriginRequiresCatalogueHashVersionTwo {
        /// The incompatible origin owner.
        identity: DefinitionIdentity,
    },
    /// A value-type reference was paired with a version-1 catalogue hash.
    ValueTypeReferenceRequiresCatalogueHashVersionTwo {
        /// The function containing the incompatible reference.
        function: FunctionId,
        /// The function revision containing the incompatible reference.
        revision: FunctionRevisionId,
        /// The referenced value type.
        target: TypeId,
    },
    /// A value-type reference has no supplied function revision version to verify.
    ValueTypeReferenceFunctionRevisionUnavailable {
        /// The function containing the reference.
        function: FunctionId,
        /// The unavailable function revision.
        revision: FunctionRevisionId,
        /// The referenced value type.
        target: TypeId,
    },
    /// A value-type reference belongs to a version-1 function semantic hash.
    ValueTypeReferenceRequiresFunctionSemanticHashVersionTwo {
        /// The function containing the incompatible reference.
        function: FunctionId,
        /// The incompatible function revision.
        revision: FunctionRevisionId,
        /// The referenced value type.
        target: TypeId,
    },
    /// A revision pair source identity differs from the stored source revision.
    SourceRevisionPairMismatch {
        pair: SourceRevisionId,
        source: SourceRevisionId,
    },
    /// A revision pair catalogue identity differs from the stored catalogue.
    CatalogueRevisionPairMismatch {
        pair: CatalogueRevisionId,
        catalogue: CatalogueRevisionId,
    },
    /// A durable revision position uses the offline-check-only catalogue identity.
    ReservedOfflineCheckCatalogueRevision {
        /// The reserved identity, always [`EMPTY_APPLICATION_CATALOGUE_REVISION_ID`].
        revision: CatalogueRevisionId,
        /// The rejected durable revision position.
        role: DurableCatalogueRevisionRole,
    },
    /// An application catalogue uses a function identity reserved for the kernel.
    ReservedSystemFunctionIdentity {
        /// The rejected reserved system function identity.
        function: FunctionId,
    },
    /// An application catalogue uses a function name reserved for the kernel.
    ReservedSystemFunctionName {
        /// The application function carrying the reserved name.
        function: FunctionId,
    },
    /// A catalogue uses a type identity reserved for a sealed invocation carrier.
    ReservedInvocationCarrierIdentity {
        /// The rejected reserved invocation-carrier identity.
        carrier: TypeId,
    },
    /// A catalogue uses a type name reserved for a sealed invocation carrier.
    ReservedInvocationCarrierName {
        /// The catalogue type carrying the reserved invocation-carrier name.
        type_id: TypeId,
    },
    /// A deployable source parent differs from its expected base source.
    DeployableSourceParentMismatch {
        expected: SourceRevisionId,
        actual: Option<SourceRevisionId>,
    },
    /// A deployable catalogue parent differs from its expected base catalogue.
    DeployableCatalogueParentMismatch {
        expected: CatalogueRevisionId,
        actual: CatalogueRevisionId,
    },
    /// A version-2 deployable omitted its complete current function revisions.
    DeployableCurrentFunctionRevisionsRequired,
    /// Complete deployable evidence omits a candidate's current function revision.
    MissingDeployableCurrentFunctionRevision {
        function: FunctionId,
        revision: FunctionRevisionId,
    },
    /// A newly installed revision differs from the corresponding current evidence.
    NewFunctionRevisionCurrentEvidenceMismatch {
        function: FunctionId,
        revision: FunctionRevisionId,
    },
    /// A candidate catalogue names itself as its parent.
    CatalogueRevisionSelfParent { revision: CatalogueRevisionId },
    /// An expression identity appears more than once.
    DuplicateExpressionId { expression: ExpressionId },
    /// A source range names a source unit not retained by the revision.
    SourceOriginUnitNotInRevision { source_unit: SourceUnitId },
    /// A source unit cannot be represented by `u32` byte offsets.
    SourceContentTooLarge { source_unit: SourceUnitId },
    /// A source range is outside its retained source content.
    SourceOriginOutOfBounds {
        source_unit: SourceUnitId,
        byte_start: u32,
        byte_end: u32,
        content_length: u32,
    },
    /// A source range splits a UTF-8 code point.
    SourceOriginNotCharacterBoundary {
        source_unit: SourceUnitId,
        byte_start: u32,
        byte_end: u32,
    },
    /// A definition identity has more than one retained origin.
    DuplicateDefinitionOrigin { identity: DefinitionIdentity },
    /// An origin does not name a definition in its candidate revision.
    OriginDefinitionNotInRevision { identity: DefinitionIdentity },
    /// A definition in the revision has no retained source origin.
    MissingDefinitionOrigin { identity: DefinitionIdentity },
    /// A function-revision identity appears more than once.
    DuplicateFunctionRevisionId { revision: FunctionRevisionId },
    /// A function has the same revision number twice.
    DuplicateFunctionRevisionNumber {
        function: FunctionId,
        revision_number: u64,
    },
    /// A function has the same declaration and semantic hash pair twice.
    DuplicateFunctionRevisionHashPair {
        function: FunctionId,
        declaration_content_hash: Sha256Digest,
        semantic_hash: Sha256Digest,
    },
    /// More than one supplied function revision belongs to one function.
    DuplicateFunctionRevisionFunction { function: FunctionId },
    /// A function revision names a function absent from the catalogue.
    FunctionRevisionFunctionNotInCatalogue {
        function: FunctionId,
        revision: FunctionRevisionId,
    },
    /// A function revision does not match the catalogue current revision.
    FunctionRevisionNotCurrent {
        function: FunctionId,
        expected: FunctionRevisionId,
        actual: FunctionRevisionId,
    },
    /// A function artifact domain differs from the catalogue execution domain.
    FunctionRevisionArtifactDomainMismatch {
        function: FunctionId,
        revision: FunctionRevisionId,
        expected: ExecutableArtifactKind,
        actual: ExecutableArtifactKind,
    },
    /// A function revision declaration range differs from its definition origin.
    FunctionRevisionOriginMismatch {
        function: FunctionId,
        revision: FunctionRevisionId,
        definition_origin: SourceOrigin,
        declaration_origin: SourceOrigin,
    },
    /// An active catalogue function has no matching active revision record.
    MissingActiveFunctionRevision {
        function: FunctionId,
        revision: FunctionRevisionId,
    },
    /// A reference source function is absent from the catalogue.
    ReferenceFunctionNotInCatalogue {
        function: FunctionId,
        revision: FunctionRevisionId,
    },
    /// A reference source revision is not the catalogue current revision.
    ReferenceRevisionNotCurrent {
        function: FunctionId,
        expected: FunctionRevisionId,
        actual: FunctionRevisionId,
    },
    /// A reference source function differs from its supplied revision record.
    ReferenceFunctionRevisionMismatch {
        function: FunctionId,
        revision: FunctionRevisionId,
    },
    /// A reference target is absent from the candidate catalogue and artifacts.
    ReferenceTargetNotInRevision { target: DefinitionReferenceTarget },
    /// A reference category cannot target that definition kind.
    ReferenceKindTargetMismatch {
        kind: DefinitionReferenceKind,
        target: DefinitionReferenceTarget,
    },
    /// A source function revision has the same reference ordinal twice.
    DuplicateReferenceOrdinal {
        revision: FunctionRevisionId,
        ordinal: u32,
    },
}

impl fmt::Display for RevisionInvariantError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        use RevisionInvariantError::*;
        match self {
            SourceOriginReversed { .. } => formatter.write_str("source origin end precedes start"),
            EmptyLogicalPath { .. } => {
                formatter.write_str("stored source unit has an empty logical path")
            }
            SourceOrdinalOutOfSequence { .. } => {
                formatter.write_str("stored source ordinals are not contiguous")
            }
            SourceOrdinalOutOfRange { .. } => {
                formatter.write_str("stored source ordinal exceeds u32")
            }
            DuplicateSourceUnitId { .. } => {
                formatter.write_str("duplicate stored source unit identity")
            }
            DuplicateLogicalPath { .. } => {
                formatter.write_str("duplicate stored source logical path")
            }
            SourceRevisionSelfParent { .. } => {
                formatter.write_str("source revision is its own parent")
            }
            StandardLibrarySourceHasParent { .. } => {
                formatter.write_str("standard library source revision has a parent")
            }
            EmptyStandardLibraryLanguageVersion { .. } => {
                formatter.write_str("standard library language version is empty")
            }
            UnsupportedStandardLibraryDefinition { .. } => {
                formatter.write_str("standard library catalogue contains an unsupported definition")
            }
            VersionOneStandardLibraryHasExecutable { .. } => {
                formatter.write_str("standard library digest version 1 has executable evidence")
            }
            VersionTwoStandardLibrarySourceHasNoParent { .. } => {
                formatter.write_str("standard library digest version 2 source has no parent")
            }
            StandardExecutableFunctionMismatch { .. } => {
                formatter.write_str("standard executable function differs from its revision")
            }
            StandardExecutableSequenceLengthMismatch { .. } => {
                formatter.write_str("standard executable sequence does not cover catalogue functions")
            }
            StandardExecutableSequenceFunctionMismatch { .. } => {
                formatter.write_str("standard executable sequence function differs from catalogue")
            }
            StandardExecutableCatalogueFunctionOrder { .. } => {
                formatter.write_str("standard executable catalogue functions are not in canonical order")
            }
            StandardExecutableSemanticHashVersionMismatch { .. } => formatter
                .write_str("standard executable requires function semantic hash version 2"),
            StandardExecutableReferenceOrdinalOutOfRange { .. } => formatter
                .write_str("standard executable reference position exceeds durable ordinal"),
            StandardExecutableReferenceOrdinalOutOfSequence { .. } => formatter
                .write_str("standard executable reference ordinals are not contiguous"),
            StandardExecutableReferenceOwnerMismatch { .. } => formatter
                .write_str("standard executable reference differs from its immutable owner"),
            EmptyArtifactFormat => formatter.write_str("artifact format is empty"),
            ZeroArtifactVersion { .. } => formatter.write_str("artifact format version is zero"),
            EmptyArtifactPayload { .. } => formatter.write_str("artifact payload is empty"),
            ZeroFunctionRevisionNumber { .. } => {
                formatter.write_str("function revision number is zero")
            }
            EmptyLanguageVersion { .. } => {
                formatter.write_str("function language version is empty")
            }
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
            FunctionSemanticHashVersionRequiresCatalogueHashVersionTwo { .. } => formatter
                .write_str("function semantic hash version 2 requires catalogue hash version 2"),
            ValueTypeDefinitionRequiresCatalogueHashVersionTwo { .. } => {
                formatter.write_str("value types require catalogue hash version 2")
            }
            RecordValueTypeRequiresCatalogueHashVersionTwo { .. } => {
                formatter.write_str("record value types require catalogue hash version 2")
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
            TypeBindingRequiresCatalogueHashVersionTwo { .. } => {
                formatter.write_str("type-name bindings require catalogue hash version 2")
            }
            DefinitionOriginRequiresCatalogueHashVersionTwo { .. } => formatter
                .write_str("value-type and type-binding origins require catalogue hash version 2"),
            ValueTypeReferenceRequiresCatalogueHashVersionTwo { .. } => {
                formatter.write_str("value-type references require catalogue hash version 2")
            }
            ValueTypeReferenceFunctionRevisionUnavailable { .. } => formatter.write_str(
                "cannot verify a value-type reference without its function revision record",
            ),
            ValueTypeReferenceRequiresFunctionSemanticHashVersionTwo { .. } => formatter
                .write_str("value-type references require function semantic hash version 2"),
            SourceRevisionPairMismatch { .. } => {
                formatter.write_str("revision pair source does not match stored source")
            }
            CatalogueRevisionPairMismatch { .. } => {
                formatter.write_str("revision pair catalogue does not match catalogue snapshot")
            }
            ReservedOfflineCheckCatalogueRevision { .. } => formatter
                .write_str("the reserved offline-check catalogue identity cannot be used in a durable revision"),
            ReservedSystemFunctionIdentity { .. } => formatter.write_str(
                "the reserved system function identity cannot enter an application catalogue",
            ),
            ReservedSystemFunctionName { .. } => formatter.write_str(
                "the reserved system function name cannot enter an application catalogue",
            ),
            ReservedInvocationCarrierIdentity { .. } => formatter.write_str(
                "the reserved invocation carrier identity cannot enter a catalogue",
            ),
            ReservedInvocationCarrierName { .. } => formatter
                .write_str("the reserved invocation carrier name cannot enter a catalogue"),
            DeployableSourceParentMismatch { .. } => {
                formatter.write_str("deployable source parent does not match expected base")
            }
            DeployableCatalogueParentMismatch { .. } => {
                formatter.write_str("deployable catalogue parent does not match expected base")
            }
            DeployableCurrentFunctionRevisionsRequired => formatter.write_str(
                "catalogue hash version 2 requires complete current function revision evidence",
            ),
            MissingDeployableCurrentFunctionRevision { .. } => formatter
                .write_str("current function revision evidence is incomplete for the candidate"),
            NewFunctionRevisionCurrentEvidenceMismatch { .. } => formatter.write_str(
                "a new function revision does not match the supplied current revision evidence",
            ),
            CatalogueRevisionSelfParent { .. } => {
                formatter.write_str("candidate catalogue is its own parent")
            }
            DuplicateExpressionId { .. } => formatter.write_str("duplicate expression identity"),
            SourceOriginUnitNotInRevision { .. } => {
                formatter.write_str("source origin unit is absent from stored revision")
            }
            SourceContentTooLarge { .. } => {
                formatter.write_str("source content exceeds u32 byte offsets")
            }
            SourceOriginOutOfBounds { .. } => {
                formatter.write_str("source origin is outside stored source content")
            }
            SourceOriginNotCharacterBoundary { .. } => {
                formatter.write_str("source origin splits a UTF-8 character")
            }
            DuplicateDefinitionOrigin { .. } => formatter.write_str("duplicate definition origin"),
            OriginDefinitionNotInRevision { .. } => {
                formatter.write_str("origin definition is absent from revision")
            }
            MissingDefinitionOrigin { .. } => {
                formatter.write_str("revision definition has no source origin")
            }
            DuplicateFunctionRevisionId { .. } => {
                formatter.write_str("duplicate function revision identity")
            }
            DuplicateFunctionRevisionNumber { .. } => {
                formatter.write_str("duplicate function revision number")
            }
            DuplicateFunctionRevisionHashPair { .. } => formatter
                .write_str("duplicate function revision declaration and semantic hash pair"),
            DuplicateFunctionRevisionFunction { .. } => {
                formatter.write_str("duplicate supplied function revision function")
            }
            FunctionRevisionFunctionNotInCatalogue { .. } => {
                formatter.write_str("function revision function is absent from catalogue")
            }
            FunctionRevisionNotCurrent { .. } => {
                formatter.write_str("function revision is not catalogue current revision")
            }
            FunctionRevisionArtifactDomainMismatch { .. } => {
                formatter.write_str("function artifact domain differs from catalogue domain")
            }
            FunctionRevisionOriginMismatch { .. } => {
                formatter.write_str("function revision declaration differs from definition origin")
            }
            MissingActiveFunctionRevision { .. } => {
                formatter.write_str("active catalogue function has no revision record")
            }
            ReferenceFunctionNotInCatalogue { .. } => {
                formatter.write_str("reference function is absent from catalogue")
            }
            ReferenceRevisionNotCurrent { .. } => {
                formatter.write_str("reference revision is not catalogue current revision")
            }
            ReferenceFunctionRevisionMismatch { .. } => {
                formatter.write_str("reference function and revision differ")
            }
            ReferenceTargetNotInRevision { .. } => {
                formatter.write_str("reference target is absent from revision")
            }
            ReferenceKindTargetMismatch { .. } => {
                formatter.write_str("reference kind cannot target that definition kind")
            }
            DuplicateReferenceOrdinal { .. } => formatter.write_str("duplicate reference ordinal"),
        }
    }
}

impl Error for RevisionInvariantError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        canonical_hash::{
            calculate_standard_library_digest_for_test, source_bundle_digest,
            source_revision_record_digest, source_unit_content_digest,
            verify_standard_library_snapshot,
        },
        catalogue::{
            EnumTypeDefinition, FieldDefinition, FunctionDefinition, FunctionDomain,
            FunctionReturn, FunctionReturnColumnDefinition, FunctionSecurity, FunctionTransaction,
            FunctionVolatility, ObjectTypeDefinition, ParameterDefinition, QualifiedSemanticName,
            RecordValueFieldDefinition, RecordValueTypeDefinition, SchemaDefinition, TypeBinding,
            ValueTypeDefinition, ValueTypeMutability, ValueTypePersistence,
        },
        types::{ResolvedType, StandardScalar, TypeDescriptor, TypeDescriptorKind},
    };

    const fn id<const BYTE: u8>() -> [u8; 16] {
        [BYTE; 16]
    }

    const fn digest<const BYTE: u8>() -> Sha256Digest {
        Sha256Digest::from_bytes([BYTE; 32])
    }

    #[test]
    fn canonical_hash_versions_convert_from_exact_supported_numbers() {
        assert_eq!(CatalogueHashVersion::Version1.to_u32(), 1);
        assert_eq!(CatalogueHashVersion::Version2.to_u32(), 2);
        assert_eq!(FunctionSemanticHashVersion::Version1.to_u32(), 1);
        assert_eq!(FunctionSemanticHashVersion::Version2.to_u32(), 2);
        assert_eq!(StandardLibraryDigestVersion::Version1.to_u32(), 1);
        assert_eq!(StandardLibraryDigestVersion::Version2.to_u32(), 2);
        assert_eq!(
            CatalogueHashVersion::try_from(1),
            Ok(CatalogueHashVersion::Version1)
        );
        assert_eq!(
            CatalogueHashVersion::try_from(2),
            Ok(CatalogueHashVersion::Version2)
        );
        assert_eq!(
            FunctionSemanticHashVersion::try_from(1),
            Ok(FunctionSemanticHashVersion::Version1)
        );
        assert_eq!(
            FunctionSemanticHashVersion::try_from(2),
            Ok(FunctionSemanticHashVersion::Version2)
        );
        assert_eq!(
            StandardLibraryDigestVersion::try_from(1),
            Ok(StandardLibraryDigestVersion::Version1)
        );
        assert_eq!(
            StandardLibraryDigestVersion::try_from(2),
            Ok(StandardLibraryDigestVersion::Version2)
        );

        for unsupported in [0, 3, u32::MAX] {
            assert!(CatalogueHashVersion::try_from(unsupported).is_err());
            assert!(FunctionSemanticHashVersion::try_from(unsupported).is_err());
            assert!(StandardLibraryDigestVersion::try_from(unsupported).is_err());
        }
    }

    #[test]
    fn unsupported_hash_versions_retain_the_exact_number_and_explain_the_hash_contract() {
        let catalogue = CatalogueHashVersion::try_from(41).unwrap_err();
        assert_eq!(
            catalogue,
            HashVersionError::UnsupportedCatalogue { value: 41 }
        );
        assert_eq!(
            catalogue.to_string(),
            "unsupported catalogue hash version 41"
        );

        let function_semantic = FunctionSemanticHashVersion::try_from(42).unwrap_err();
        assert_eq!(
            function_semantic,
            HashVersionError::UnsupportedFunctionSemantic { value: 42 }
        );
        assert_eq!(
            function_semantic.to_string(),
            "unsupported function semantic hash version 42"
        );

        let standard_library = StandardLibraryDigestVersion::try_from(43).unwrap_err();
        assert_eq!(
            standard_library,
            HashVersionError::UnsupportedStandardLibraryDigest { value: 43 }
        );
        assert_eq!(
            standard_library.to_string(),
            "unsupported standard library digest version 43"
        );
    }

    fn source(parent: Option<SourceRevisionId>) -> StoredSourceRevision {
        StoredSourceRevision::new(
            SourceBundleId::from_bytes(id::<1>()),
            SourceRevisionId::from_bytes(id::<2>()),
            parent,
            vec![
                StoredSourceUnit::new(
                    SourceUnitId::from_bytes(id::<3>()),
                    0,
                    "crm/schema.orna",
                    "CREATE SCHEMA crm;\n",
                    digest::<3>(),
                )
                .unwrap(),
                StoredSourceUnit::new(
                    SourceUnitId::from_bytes(id::<4>()),
                    1,
                    "crm/functions.orna",
                    "-- cafe\u{301}\nFUNCTION crm.lookup;\n",
                    digest::<4>(),
                )
                .unwrap(),
            ],
            digest::<5>(),
            digest::<6>(),
        )
        .unwrap()
    }

    fn empty_catalogue() -> CatalogueSnapshot {
        CatalogueSnapshot::new(CatalogueRevisionId::from_bytes(id::<7>()), vec![], vec![]).unwrap()
    }

    fn active_for_flat_type_conversion(
        catalogue: CatalogueSnapshot,
        catalogue_hash_context: CatalogueHashContext,
    ) -> ActiveDatabaseRevision {
        let source = source(None);
        ActiveDatabaseRevision {
            pair: RevisionPair::new(source.id(), catalogue.revision()),
            source,
            catalogue,
            catalogue_hash: digest::<7>(),
            catalogue_hash_context,
            expressions: vec![],
            function_revisions: vec![],
            historical_function_revisions: vec![],
            origins: vec![],
            references: vec![],
        }
    }

    fn flat_type_application_catalogue() -> CatalogueSnapshot {
        CatalogueSnapshot::new_with_record_value_types(
            CatalogueRevisionId::from_bytes(id::<7>()),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes(id::<8>()),
                QualifiedSemanticName::new(["crm"]).unwrap(),
            )],
            vec![ObjectTypeDefinition::new(
                TypeId::from_bytes(id::<80>()),
                QualifiedSemanticName::new(["crm", "contact"]).unwrap(),
                vec![],
            )],
            vec![ValueTypeDefinition::opaque(
                TypeId::from_bytes(id::<81>()),
                QualifiedSemanticName::new(["crm", "token"]).unwrap(),
                "crm.token@1",
            )],
            vec![EnumTypeDefinition::new(
                TypeId::from_bytes(id::<82>()),
                QualifiedSemanticName::new(["crm", "stage"]).unwrap(),
                ["lead"],
            )],
            vec![RecordValueTypeDefinition::new(
                TypeId::from_bytes(id::<83>()),
                QualifiedSemanticName::new(["crm", "status"]).unwrap(),
                vec![
                    RecordValueFieldDefinition::try_new_descriptor(
                        FieldId::from_bytes(id::<84>()),
                        "active",
                        0,
                        TypeDescriptor::named(TypeId::from_bytes(id::<71>())),
                    )
                    .unwrap(),
                ],
            )],
            vec![],
        )
        .unwrap()
    }

    fn flat_type_standard_context() -> CatalogueHashContext {
        let catalogue = CatalogueSnapshot::new_with_enum_types(
            CatalogueRevisionId::from_bytes(id::<72>()),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes(id::<73>()),
                QualifiedSemanticName::new(["std"]).unwrap(),
            )],
            vec![],
            vec![
                standard_boolean_definition(),
                ValueTypeDefinition::opaque(
                    TypeId::from_bytes(id::<74>()),
                    QualifiedSemanticName::new(["std", "token"]).unwrap(),
                    "std.token@1",
                ),
            ],
            vec![EnumTypeDefinition::new(
                TypeId::from_bytes(id::<75>()),
                QualifiedSemanticName::new(["std", "mode"]).unwrap(),
                ["safe"],
            )],
            vec![],
        )
        .unwrap();
        let content = "standard flat type descriptor fixture";
        let unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes(id::<3>()),
            0,
            "std/types.orna",
            content,
            source_unit_content_digest(content).unwrap(),
        )
        .unwrap();
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
        let source = StoredSourceRevision::new(
            SourceBundleId::from_bytes(id::<1>()),
            SourceRevisionId::from_bytes(id::<2>()),
            None,
            vec![unit],
            bundle_hash,
            source_revision_record_digest(SourceBundleId::from_bytes(id::<1>()), None, bundle_hash)
                .unwrap(),
        )
        .unwrap();
        let source_unit = SourceUnitId::from_bytes(id::<3>());
        let origins = [
            DefinitionIdentity::Schema(SchemaId::from_bytes(id::<73>())),
            DefinitionIdentity::ValueType(TypeId::from_bytes(id::<71>())),
            DefinitionIdentity::ValueType(TypeId::from_bytes(id::<74>())),
            DefinitionIdentity::ValueType(TypeId::from_bytes(id::<75>())),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, identity)| {
            DefinitionOrigin::new(
                identity,
                SourceOrigin::new(source_unit, index as u32, index as u32 + 1).unwrap(),
            )
        })
        .collect::<Vec<_>>();
        let provisional = StandardLibrarySnapshot::new(
            StandardLibraryRevisionId::from_bytes(id::<74>()),
            StandardLibraryDigestVersion::Version1,
            source.clone(),
            "orna.language/1",
            catalogue.clone(),
            origins.clone(),
            digest::<75>(),
        )
        .unwrap();
        let digest = calculate_standard_library_digest_for_test(&provisional).unwrap();
        let standard = StandardLibrarySnapshot::new(
            provisional.revision(),
            provisional.digest_version(),
            source,
            provisional.language_version(),
            catalogue,
            origins,
            digest,
        )
        .unwrap();
        CatalogueHashContext::version_two(verify_standard_library_snapshot(standard).unwrap())
    }

    #[test]
    fn active_revision_converts_only_catalogue_identified_flat_type_leaves() {
        let active = active_for_flat_type_conversion(
            flat_type_application_catalogue(),
            flat_type_standard_context(),
        );

        for type_id in [
            TypeId::from_bytes(id::<82>()),
            TypeId::from_bytes(id::<83>()),
            TypeId::from_bytes(id::<75>()),
        ] {
            assert_eq!(
                active
                    .type_descriptor_for(ResolvedType::named(type_id))
                    .unwrap()
                    .kind(),
                TypeDescriptorKind::Named(type_id)
            );
        }
        for type_id in [
            TypeId::from_bytes(id::<71>()),
            TypeId::from_bytes(id::<74>()),
        ] {
            assert_eq!(
                active
                    .type_descriptor_for(ResolvedType::value(type_id))
                    .unwrap()
                    .kind(),
                TypeDescriptorKind::Named(type_id)
            );
        }
        let object = TypeId::from_bytes(id::<80>());
        assert_eq!(
            active
                .type_descriptor_for(ResolvedType::reference(object))
                .unwrap()
                .kind(),
            TypeDescriptorKind::Reference(object)
        );
    }

    #[test]
    fn active_revision_flat_type_conversion_rejects_every_missing_or_wrong_category() {
        let active = active_for_flat_type_conversion(
            flat_type_application_catalogue(),
            flat_type_standard_context(),
        );
        let missing = TypeId::from_bytes(id::<99>());

        assert_eq!(
            active.type_descriptor_for(ResolvedType::scalar(StandardScalar::Boolean)),
            Err(FlatTypeDescriptorError::LegacyScalar {
                scalar: StandardScalar::Boolean,
            })
        );
        assert_eq!(
            active.type_descriptor_for(ResolvedType::named(missing)),
            Err(FlatTypeDescriptorError::UnknownNamedType { id: missing })
        );
        assert_eq!(
            active.type_descriptor_for(ResolvedType::named(TypeId::from_bytes(id::<80>()))),
            Err(FlatTypeDescriptorError::NamedObjectType {
                id: TypeId::from_bytes(id::<80>()),
            })
        );
        assert_eq!(
            active.type_descriptor_for(ResolvedType::named(TypeId::from_bytes(id::<81>()))),
            Err(FlatTypeDescriptorError::NamedValueType {
                id: TypeId::from_bytes(id::<81>()),
            })
        );
        assert_eq!(
            active.type_descriptor_for(ResolvedType::named(TypeId::from_bytes(id::<71>()))),
            Err(FlatTypeDescriptorError::NamedValueType {
                id: TypeId::from_bytes(id::<71>()),
            })
        );
        assert_eq!(
            active.type_descriptor_for(ResolvedType::value(missing)),
            Err(FlatTypeDescriptorError::UnknownStandardValueType {
                value_type: missing,
            })
        );
        assert_eq!(
            active.type_descriptor_for(ResolvedType::value(TypeId::from_bytes(id::<81>()))),
            Err(FlatTypeDescriptorError::UnknownStandardValueType {
                value_type: TypeId::from_bytes(id::<81>()),
            })
        );
        assert_eq!(
            active.type_descriptor_for(ResolvedType::value(TypeId::from_bytes(id::<75>()))),
            Err(FlatTypeDescriptorError::UnknownStandardValueType {
                value_type: TypeId::from_bytes(id::<75>()),
            })
        );
        assert_eq!(
            active.type_descriptor_for(ResolvedType::reference(TypeId::from_bytes(id::<82>()))),
            Err(FlatTypeDescriptorError::ReferenceTargetNotObject {
                target: TypeId::from_bytes(id::<82>()),
            })
        );
    }

    #[test]
    fn active_revision_flat_type_conversion_closes_version_one_and_colliding_identities() {
        let value_type = TypeId::from_bytes(id::<71>());
        let version_one = active_for_flat_type_conversion(
            flat_type_application_catalogue(),
            CatalogueHashContext::version_one(),
        );
        assert_eq!(
            version_one.type_descriptor_for(ResolvedType::value(value_type)),
            Err(FlatTypeDescriptorError::StandardLibraryUnavailable { value_type })
        );
        assert_eq!(
            version_one.type_descriptor_for(ResolvedType::scalar(StandardScalar::Boolean)),
            Err(FlatTypeDescriptorError::LegacyScalar {
                scalar: StandardScalar::Boolean,
            })
        );

        let collision = TypeId::from_bytes(id::<75>());
        let application = CatalogueSnapshot::new_with_enum_types(
            CatalogueRevisionId::from_bytes(id::<7>()),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes(id::<8>()),
                QualifiedSemanticName::new(["crm"]).unwrap(),
            )],
            vec![],
            vec![],
            vec![EnumTypeDefinition::new(
                collision,
                QualifiedSemanticName::new(["crm", "mode"]).unwrap(),
                ["open"],
            )],
            vec![],
        )
        .unwrap();
        let active = active_for_flat_type_conversion(application, flat_type_standard_context());
        assert_eq!(
            active.type_descriptor_for(ResolvedType::named(collision)),
            Err(FlatTypeDescriptorError::AmbiguousNamedType { id: collision })
        );
    }

    #[test]
    fn flat_type_descriptor_errors_have_exact_actionable_messages_without_sources() {
        let type_id = TypeId::from_bytes(id::<99>());
        let cases = [
            (
                FlatTypeDescriptorError::LegacyScalar {
                    scalar: StandardScalar::Boolean,
                },
                "legacy scalar type has no catalogue identity",
            ),
            (
                FlatTypeDescriptorError::AmbiguousNamedType { id: type_id },
                "resolved named type is present in both application and standard catalogues",
            ),
            (
                FlatTypeDescriptorError::UnknownNamedType { id: type_id },
                "resolved named type is absent from the active catalogue",
            ),
            (
                FlatTypeDescriptorError::NamedObjectType { id: type_id },
                "resolved named type is an object and requires REF",
            ),
            (
                FlatTypeDescriptorError::NamedValueType { id: type_id },
                "resolved named type is a value definition and requires a value identity",
            ),
            (
                FlatTypeDescriptorError::StandardLibraryUnavailable {
                    value_type: type_id,
                },
                "the active database has no standard library for the resolved value type",
            ),
            (
                FlatTypeDescriptorError::UnknownStandardValueType {
                    value_type: type_id,
                },
                "resolved value type is absent from the pinned standard library",
            ),
            (
                FlatTypeDescriptorError::ReferenceTargetNotObject { target: type_id },
                "resolved reference target is not an active application object",
            ),
        ];

        for (error, expected) in cases {
            assert_eq!(error.to_string(), expected);
            assert!(error.source().is_none());
        }
    }

    fn unchecked_standard_with_catalogue_revision(
        catalogue_revision: CatalogueRevisionId,
    ) -> VerifiedStandardLibrarySnapshot {
        VerifiedStandardLibrarySnapshot::new(StandardLibrarySnapshot {
            inner: Arc::new(StandardLibrarySnapshotData {
                revision: StandardLibraryRevisionId::from_bytes(id::<74>()),
                digest_version: StandardLibraryDigestVersion::Version1,
                source: source(None),
                language_version: "orna.language/1".to_owned(),
                catalogue: CatalogueSnapshot::new(catalogue_revision, vec![], vec![]).unwrap(),
                executables: vec![],
                origins: vec![],
                digest: digest::<75>(),
            }),
        })
    }

    fn function_catalogue(function_revision: FunctionRevisionId) -> CatalogueSnapshot {
        function_catalogue_with_objects(function_revision, vec![])
    }

    fn function_catalogue_v2(function_revision: FunctionRevisionId) -> CatalogueSnapshot {
        function_catalogue_with_resolved_type(
            function_revision,
            vec![],
            ResolvedType::value(TypeId::from_bytes(id::<71>())),
        )
    }

    fn function_catalogue_with_objects(
        function_revision: FunctionRevisionId,
        object_types: Vec<ObjectTypeDefinition>,
    ) -> CatalogueSnapshot {
        function_catalogue_with_resolved_type(
            function_revision,
            object_types,
            ResolvedType::scalar(StandardScalar::Boolean),
        )
    }

    fn function_catalogue_with_resolved_type(
        function_revision: FunctionRevisionId,
        object_types: Vec<ObjectTypeDefinition>,
        resolved_type: ResolvedType,
    ) -> CatalogueSnapshot {
        function_catalogue_with_identity(
            FunctionId::from_bytes(id::<9>()),
            function_revision,
            object_types,
            resolved_type,
        )
    }

    fn function_catalogue_with_identity(
        function_id: FunctionId,
        function_revision: FunctionRevisionId,
        object_types: Vec<ObjectTypeDefinition>,
        resolved_type: ResolvedType,
    ) -> CatalogueSnapshot {
        let schema = SchemaDefinition::new(
            SchemaId::from_bytes(id::<8>()),
            QualifiedSemanticName::new(["crm"]).unwrap(),
        );
        let function = FunctionDefinition::new(
            function_id,
            QualifiedSemanticName::new(["crm", "lookup"]).unwrap(),
            FunctionDomain::Server,
            vec![],
            FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
                "found",
                0,
                resolved_type,
            )]),
            function_revision,
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Stable,
        );
        CatalogueSnapshot::new_with_functions(
            CatalogueRevisionId::from_bytes(id::<7>()),
            vec![schema],
            object_types,
            vec![function],
        )
        .unwrap()
    }

    fn function_definition_named(
        name: &[&str],
        function_id: FunctionId,
        function_revision: FunctionRevisionId,
        resolved_type: ResolvedType,
    ) -> FunctionDefinition {
        FunctionDefinition::new(
            function_id,
            QualifiedSemanticName::new(name.iter().copied()).unwrap(),
            FunctionDomain::Server,
            vec![],
            FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
                "found",
                0,
                resolved_type,
            )]),
            function_revision,
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Stable,
        )
    }

    fn function_catalogue_with_functions(functions: Vec<FunctionDefinition>) -> CatalogueSnapshot {
        assert!(
            !functions.is_empty(),
            "the catalogue requires at least one function"
        );
        let namespace = {
            let parts = functions[0].name().parts();
            &parts[..parts.len() - 1]
        };
        assert!(
            functions.iter().all(|function| {
                let parts = function.name().parts();
                parts.len() == namespace.len() + 1 && &parts[..namespace.len()] == namespace
            }),
            "all supplied functions must share the same namespace"
        );
        let schema = SchemaDefinition::new(
            SchemaId::from_bytes(id::<8>()),
            QualifiedSemanticName::new(namespace.iter().cloned()).unwrap(),
        );
        CatalogueSnapshot::new_with_functions(
            CatalogueRevisionId::from_bytes(id::<7>()),
            vec![schema],
            vec![],
            functions,
        )
        .unwrap()
    }

    fn resolved_type_slots_catalogue(
        field_type: ResolvedType,
        parameter_type: ResolvedType,
        return_type: FunctionReturn,
    ) -> CatalogueSnapshot {
        let schema = SchemaDefinition::new(
            SchemaId::from_bytes(id::<8>()),
            QualifiedSemanticName::new(["crm"]).unwrap(),
        );
        let object_type = ObjectTypeDefinition::new(
            TypeId::from_bytes(id::<80>()),
            QualifiedSemanticName::new(["crm", "task"]).unwrap(),
            vec![FieldDefinition::new(
                FieldId::from_bytes(id::<81>()),
                "value",
                0,
                field_type,
                false,
                false,
                None,
                None,
            )],
        );
        let function = FunctionDefinition::new(
            FunctionId::from_bytes(id::<82>()),
            QualifiedSemanticName::new(["crm", "enabled"]).unwrap(),
            FunctionDomain::Client,
            vec![ParameterDefinition::new(
                ParameterId::from_bytes(id::<83>()),
                "input",
                0,
                parameter_type,
                None,
            )],
            return_type,
            FunctionRevisionId::from_bytes(id::<84>()),
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        );
        CatalogueSnapshot::new_with_functions(
            CatalogueRevisionId::from_bytes(id::<7>()),
            vec![schema],
            vec![object_type],
            vec![function],
        )
        .unwrap()
    }

    fn catalogue_with_opaque_client_return(
        opaque: TypeId,
        domain: FunctionDomain,
        parameters: Vec<ParameterDefinition>,
        security: FunctionSecurity,
        volatility: FunctionVolatility,
    ) -> CatalogueSnapshot {
        let schema = SchemaDefinition::new(
            SchemaId::from_bytes(id::<8>()),
            QualifiedSemanticName::new(["crm"]).unwrap(),
        );
        let function = FunctionDefinition::new(
            FunctionId::from_bytes(id::<82>()),
            QualifiedSemanticName::new(["crm", "token"]).unwrap(),
            domain,
            parameters,
            FunctionReturn::Single(ResolvedType::value(opaque)),
            FunctionRevisionId::from_bytes(id::<84>()),
            security,
            None,
            volatility,
        );
        CatalogueSnapshot::new_with_functions(
            CatalogueRevisionId::from_bytes(id::<7>()),
            vec![schema],
            vec![],
            vec![function],
        )
        .unwrap()
    }

    #[test]
    fn standard_library_snapshot_rejects_a_parented_source_with_exact_context() {
        let revision = StandardLibraryRevisionId::from_bytes(id::<74>());
        let parent = SourceRevisionId::from_bytes(id::<75>());
        let standard_source = source(Some(parent));
        let source_id = standard_source.id();

        let error = StandardLibrarySnapshot::new(
            revision,
            StandardLibraryDigestVersion::Version1,
            standard_source,
            "orna.language/1",
            empty_catalogue(),
            vec![],
            digest::<76>(),
        )
        .unwrap_err();

        assert_eq!(
            error,
            RevisionInvariantError::StandardLibrarySourceHasParent {
                source: source_id,
                parent: Some(parent),
            }
        );
        assert_eq!(
            error.to_string(),
            "standard library source revision has a parent"
        );
    }

    #[test]
    fn standard_library_snapshot_rejects_an_empty_language_version_with_exact_revision() {
        let revision = StandardLibraryRevisionId::from_bytes(id::<74>());

        let error = StandardLibrarySnapshot::new(
            revision,
            StandardLibraryDigestVersion::Version1,
            source(None),
            "",
            empty_catalogue(),
            vec![],
            digest::<76>(),
        )
        .unwrap_err();

        assert_eq!(
            error,
            RevisionInvariantError::EmptyStandardLibraryLanguageVersion { revision }
        );
        assert_eq!(
            error.to_string(),
            "standard library language version is empty"
        );
    }

    #[test]
    fn rejects_the_offline_application_sentinel_from_a_standard_snapshot() {
        let revision = StandardLibraryRevisionId::from_bytes(id::<74>());
        let catalogue =
            CatalogueSnapshot::new(EMPTY_APPLICATION_CATALOGUE_REVISION_ID, vec![], vec![])
                .unwrap();

        let error = StandardLibrarySnapshot::new(
            revision,
            StandardLibraryDigestVersion::Version1,
            source(None),
            "orna.language/1",
            catalogue,
            vec![],
            digest::<75>(),
        )
        .unwrap_err();

        assert_eq!(
            error,
            RevisionInvariantError::ReservedOfflineCheckCatalogueRevision {
                revision: EMPTY_APPLICATION_CATALOGUE_REVISION_ID,
                role: DurableCatalogueRevisionRole::ActiveOrRecoveredStandard,
            }
        );
        assert_eq!(
            error.to_string(),
            "the reserved offline-check catalogue identity cannot be used in a durable revision"
        );
        assert!(Error::source(&error).is_none());
    }

    #[test]
    fn rejects_the_offline_application_sentinel_from_active_and_deployable_positions_in_order() {
        let regular_catalogue = CatalogueRevisionId::from_bytes(id::<7>());
        let expected_source = SourceRevisionId::from_bytes(id::<81>());
        let candidate_source = source(Some(expected_source));
        let assert_reserved = |error: RevisionInvariantError, role| {
            assert_eq!(
                error,
                RevisionInvariantError::ReservedOfflineCheckCatalogueRevision {
                    revision: EMPTY_APPLICATION_CATALOGUE_REVISION_ID,
                    role,
                }
            );
            assert_eq!(
                error.to_string(),
                "the reserved offline-check catalogue identity cannot be used in a durable revision"
            );
            assert!(Error::source(&error).is_none());
        };

        let active_source = source(None);
        let active_error = ActiveDatabaseRevision::new(
            RevisionPair::new(active_source.id(), EMPTY_APPLICATION_CATALOGUE_REVISION_ID),
            active_source,
            CatalogueSnapshot::new(EMPTY_APPLICATION_CATALOGUE_REVISION_ID, vec![], vec![])
                .unwrap(),
            digest::<76>(),
            vec![],
            vec![],
            vec![],
            vec![],
        )
        .unwrap_err();
        assert_reserved(
            active_error,
            DurableCatalogueRevisionRole::ActiveOrRecoveredApplication,
        );

        let active_source = source(None);
        let app_before_standard_error = ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                RevisionPair::new(active_source.id(), EMPTY_APPLICATION_CATALOGUE_REVISION_ID),
                active_source,
                CatalogueSnapshot::new(EMPTY_APPLICATION_CATALOGUE_REVISION_ID, vec![], vec![])
                    .unwrap(),
                digest::<76>(),
                ActiveRevisionContent::new(vec![], vec![], vec![], vec![]),
            ),
            CatalogueHashContext::version_two(unchecked_standard_with_catalogue_revision(
                EMPTY_APPLICATION_CATALOGUE_REVISION_ID,
            )),
        )
        .unwrap_err();
        assert_reserved(
            app_before_standard_error,
            DurableCatalogueRevisionRole::ActiveOrRecoveredApplication,
        );

        let active_source = source(None);
        let standard_error = ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                RevisionPair::new(active_source.id(), regular_catalogue),
                active_source,
                CatalogueSnapshot::new(regular_catalogue, vec![], vec![]).unwrap(),
                digest::<76>(),
                ActiveRevisionContent::new(vec![], vec![], vec![], vec![]),
            ),
            CatalogueHashContext::version_two(unchecked_standard_with_catalogue_revision(
                EMPTY_APPLICATION_CATALOGUE_REVISION_ID,
            )),
        )
        .unwrap_err();
        assert_reserved(
            standard_error,
            DurableCatalogueRevisionRole::ActiveOrRecoveredStandard,
        );

        let deployable = |expected_catalogue, parent_catalogue, candidate_catalogue| {
            DeployableRevision::new(
                RevisionPair::new(expected_source, expected_catalogue),
                candidate_source.clone(),
                parent_catalogue,
                CatalogueSnapshot::new(candidate_catalogue, vec![], vec![]).unwrap(),
                digest::<77>(),
                vec![],
                vec![],
                vec![],
                vec![],
            )
        };
        assert_reserved(
            deployable(
                EMPTY_APPLICATION_CATALOGUE_REVISION_ID,
                EMPTY_APPLICATION_CATALOGUE_REVISION_ID,
                EMPTY_APPLICATION_CATALOGUE_REVISION_ID,
            )
            .unwrap_err(),
            DurableCatalogueRevisionRole::DeployableExpectedBase,
        );
        assert_reserved(
            deployable(
                regular_catalogue,
                EMPTY_APPLICATION_CATALOGUE_REVISION_ID,
                EMPTY_APPLICATION_CATALOGUE_REVISION_ID,
            )
            .unwrap_err(),
            DurableCatalogueRevisionRole::DeployableParent,
        );
        assert_reserved(
            deployable(
                regular_catalogue,
                regular_catalogue,
                EMPTY_APPLICATION_CATALOGUE_REVISION_ID,
            )
            .unwrap_err(),
            DurableCatalogueRevisionRole::DeployableCandidate,
        );
    }

    #[test]
    fn standard_library_snapshot_rejects_object_and_function_definitions_with_exact_revision() {
        let revision = StandardLibraryRevisionId::from_bytes(id::<74>());
        let object_catalogue = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes(id::<7>()),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes(id::<8>()),
                QualifiedSemanticName::new(["crm"]).unwrap(),
            )],
            vec![ObjectTypeDefinition::new(
                TypeId::from_bytes(id::<12>()),
                QualifiedSemanticName::new(["crm", "contact"]).unwrap(),
                vec![],
            )],
        )
        .unwrap();
        assert_eq!(object_catalogue.object_types().len(), 1);
        assert!(object_catalogue.functions().is_empty());

        let function_catalogue = function_catalogue(FunctionRevisionId::from_bytes(id::<11>()));
        assert!(function_catalogue.object_types().is_empty());
        assert_eq!(function_catalogue.functions().len(), 1);

        for catalogue in [object_catalogue, function_catalogue] {
            let error = StandardLibrarySnapshot::new(
                revision,
                StandardLibraryDigestVersion::Version1,
                source(None),
                "orna.language/1",
                catalogue,
                vec![],
                digest::<76>(),
            )
            .unwrap_err();

            assert_eq!(
                error,
                RevisionInvariantError::UnsupportedStandardLibraryDefinition { revision }
            );
            assert_eq!(
                error.to_string(),
                "standard library catalogue contains an unsupported definition"
            );
        }
    }

    #[test]
    fn version_two_standard_snapshot_requires_complete_ordered_executable_evidence() {
        let function = FunctionId::from_bytes(id::<90>());
        assert!(matches!(
            StandardExecutable::new(function, function_revision(), vec![]),
            Err(RevisionInvariantError::StandardExecutableFunctionMismatch { .. })
        ));
        let function_revision = FunctionRevisionId::from_bytes(id::<91>());
        let schema = SchemaDefinition::new(
            SchemaId::from_bytes(id::<92>()),
            QualifiedSemanticName::new(["std", "invoke"]).unwrap(),
        );
        let catalogue = CatalogueSnapshot::new_with_functions(
            CatalogueRevisionId::from_bytes(id::<93>()),
            vec![schema.clone()],
            vec![],
            vec![FunctionDefinition::new(
                function,
                QualifiedSemanticName::new(["std", "invoke", "echo"]).unwrap(),
                FunctionDomain::Server,
                vec![],
                FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Integer)),
                function_revision,
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::ReadOnly),
                FunctionVolatility::Stable,
            )],
        )
        .unwrap();
        let snapshot_source = source(Some(SourceRevisionId::from_bytes(id::<94>())));
        let declaration = SourceOrigin::new(SourceUnitId::from_bytes(id::<4>()), 10, 29).unwrap();
        let revision = FunctionRevisionRecord::new(
            function,
            function_revision,
            1,
            declaration,
            digest::<95>(),
            digest::<96>(),
            "orna.language/1",
            artifact(),
        )
        .unwrap()
        .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
        let executable = StandardExecutable::new(function, revision.clone(), vec![]).unwrap();
        let origins = vec![
            DefinitionOrigin::new(
                DefinitionIdentity::Schema(schema.id()),
                SourceOrigin::new(SourceUnitId::from_bytes(id::<3>()), 0, 1).unwrap(),
            ),
            DefinitionOrigin::new(DefinitionIdentity::Function(function), declaration),
        ];

        let snapshot = StandardLibrarySnapshot::new_with_executables(
            StandardLibraryRevisionId::from_bytes(id::<97>()),
            StandardLibraryDigestVersion::Version2,
            snapshot_source.clone(),
            "orna.language/1",
            catalogue.clone(),
            vec![executable.clone()],
            origins.clone(),
            digest::<98>(),
        )
        .unwrap();
        assert_eq!(snapshot.executables(), [executable]);

        let lower_function = FunctionId::from_bytes(id::<89>());
        let lower_revision = FunctionRevisionId::from_bytes(id::<88>());
        let lower_definition = FunctionDefinition::new(
            lower_function,
            QualifiedSemanticName::new(["std", "invoke", "later"]).unwrap(),
            FunctionDomain::Server,
            vec![],
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Integer)),
            lower_revision,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        let reordered_catalogue = CatalogueSnapshot::new_with_functions(
            CatalogueRevisionId::from_bytes(id::<93>()),
            vec![schema.clone()],
            vec![],
            vec![
                snapshot.catalogue().functions()[0].clone(),
                lower_definition,
            ],
        )
        .unwrap();
        let lower_executable = StandardExecutable::new(
            lower_function,
            FunctionRevisionRecord::new(
                lower_function,
                lower_revision,
                1,
                declaration,
                digest::<87>(),
                digest::<86>(),
                "orna.language/1",
                artifact(),
            )
            .unwrap()
            .with_semantic_hash_version(FunctionSemanticHashVersion::Version2),
            vec![],
        )
        .unwrap();
        let mut reordered_origins = snapshot.origins().to_vec();
        reordered_origins.push(DefinitionOrigin::new(
            DefinitionIdentity::Function(lower_function),
            declaration,
        ));
        assert!(matches!(
            StandardLibrarySnapshot::new_with_executables(
                StandardLibraryRevisionId::from_bytes(id::<97>()),
                StandardLibraryDigestVersion::Version2,
                source(Some(SourceRevisionId::from_bytes(id::<94>()))),
                "orna.language/1",
                reordered_catalogue,
                vec![snapshot.executables()[0].clone(), lower_executable],
                reordered_origins,
                digest::<98>(),
            ),
            Err(
                RevisionInvariantError::StandardExecutableCatalogueFunctionOrder { ordinal: 1, .. }
            )
        ));

        assert!(matches!(
            StandardLibrarySnapshot::new_with_executables(
                StandardLibraryRevisionId::from_bytes(id::<97>()),
                StandardLibraryDigestVersion::Version2,
                source(None),
                "orna.language/1",
                catalogue.clone(),
                vec![],
                origins.clone(),
                digest::<98>(),
            ),
            Err(RevisionInvariantError::VersionTwoStandardLibrarySourceHasNoParent { .. })
        ));
        assert!(matches!(
            StandardLibrarySnapshot::new_with_executables(
                StandardLibraryRevisionId::from_bytes(id::<97>()),
                StandardLibraryDigestVersion::Version2,
                snapshot_source,
                "orna.language/1",
                catalogue,
                vec![],
                origins,
                digest::<98>(),
            ),
            Err(RevisionInvariantError::StandardExecutableSequenceLengthMismatch { .. })
        ));

        let version_one_executable = StandardExecutable::new(
            function,
            FunctionRevisionRecord::new(
                function,
                function_revision,
                1,
                declaration,
                digest::<95>(),
                digest::<96>(),
                "orna.language/1",
                artifact(),
            )
            .unwrap(),
            vec![],
        )
        .unwrap();
        assert!(matches!(
            StandardLibrarySnapshot::new_with_executables(
                StandardLibraryRevisionId::from_bytes(id::<97>()),
                StandardLibraryDigestVersion::Version2,
                source(Some(SourceRevisionId::from_bytes(id::<94>()))),
                "orna.language/1",
                snapshot.catalogue().clone(),
                vec![version_one_executable],
                snapshot.origins().to_vec(),
                digest::<98>(),
            ),
            Err(RevisionInvariantError::StandardExecutableSemanticHashVersionMismatch { .. })
        ));

        let out_of_order = StandardExecutable::new(
            function,
            revision.clone(),
            vec![DefinitionReference::new(
                function,
                function_revision,
                1,
                DefinitionReferenceTarget::Function(function),
                DefinitionReferenceKind::FunctionCall,
                declaration,
            )],
        )
        .unwrap();
        assert!(matches!(
            StandardLibrarySnapshot::new_with_executables(
                StandardLibraryRevisionId::from_bytes(id::<97>()),
                StandardLibraryDigestVersion::Version2,
                source(Some(SourceRevisionId::from_bytes(id::<94>()))),
                "orna.language/1",
                snapshot.catalogue().clone(),
                vec![out_of_order],
                snapshot.origins().to_vec(),
                digest::<98>(),
            ),
            Err(RevisionInvariantError::StandardExecutableReferenceOrdinalOutOfSequence { .. })
        ));

        let crossed_reference = StandardExecutable::new(
            function,
            revision,
            vec![DefinitionReference::new(
                FunctionId::from_bytes(id::<99>()),
                function_revision,
                0,
                DefinitionReferenceTarget::Function(function),
                DefinitionReferenceKind::FunctionCall,
                declaration,
            )],
        )
        .unwrap();
        assert!(matches!(
            StandardLibrarySnapshot::new_with_executables(
                StandardLibraryRevisionId::from_bytes(id::<97>()),
                StandardLibraryDigestVersion::Version2,
                source(Some(SourceRevisionId::from_bytes(id::<94>()))),
                "orna.language/1",
                snapshot.catalogue().clone(),
                vec![crossed_reference],
                snapshot.origins().to_vec(),
                digest::<98>(),
            ),
            Err(RevisionInvariantError::StandardExecutableReferenceOwnerMismatch { .. })
        ));
    }

    fn write_catalogue(function_revision: FunctionRevisionId) -> CatalogueSnapshot {
        let object_type = ObjectTypeDefinition::new(
            TypeId::from_bytes(id::<12>()),
            QualifiedSemanticName::new(["crm", "contact"]).unwrap(),
            vec![FieldDefinition::new(
                FieldId::from_bytes(id::<13>()),
                "active",
                0,
                ResolvedType::scalar(StandardScalar::Boolean),
                false,
                true,
                None,
                None,
            )],
        );
        function_catalogue_with_objects(function_revision, vec![object_type])
    }

    fn artifact() -> ExecutableArtifact {
        ExecutableArtifact::new(
            ExecutableArtifactKind::Server,
            "orna.server-plan",
            1,
            vec![1, 2, 3],
            digest::<10>(),
        )
        .unwrap()
    }

    fn function_revision() -> FunctionRevisionRecord {
        function_revision_fixture(
            FunctionId::from_bytes(id::<9>()),
            FunctionRevisionId::from_bytes(id::<11>()),
            digest::<11>(),
            digest::<12>(),
        )
    }

    fn function_revision_fixture(
        function: FunctionId,
        revision: FunctionRevisionId,
        declaration_content_hash: Sha256Digest,
        semantic_hash: Sha256Digest,
    ) -> FunctionRevisionRecord {
        FunctionRevisionRecord::new(
            function,
            revision,
            1,
            SourceOrigin::new(SourceUnitId::from_bytes(id::<4>()), 10, 29).unwrap(),
            declaration_content_hash,
            semantic_hash,
            "orna-1",
            artifact(),
        )
        .unwrap()
    }

    fn function_revision_v2() -> FunctionRevisionRecord {
        function_revision().with_semantic_hash_version(FunctionSemanticHashVersion::Version2)
    }

    fn resolved_slot_function_revision() -> FunctionRevisionRecord {
        FunctionRevisionRecord::new(
            FunctionId::from_bytes(id::<82>()),
            FunctionRevisionId::from_bytes(id::<84>()),
            1,
            SourceOrigin::new(SourceUnitId::from_bytes(id::<4>()), 10, 29).unwrap(),
            digest::<11>(),
            digest::<12>(),
            "orna-1",
            ExecutableArtifact::new(
                ExecutableArtifactKind::Client,
                "orna.client-plan",
                1,
                vec![1, 2, 3],
                digest::<10>(),
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn unaffected_function_revision() -> FunctionRevisionRecord {
        function_revision_fixture(
            FunctionId::from_bytes(id::<19>()),
            FunctionRevisionId::from_bytes(id::<22>()),
            digest::<21>(),
            digest::<22>(),
        )
    }

    fn mixed_function_catalogue(
        affected_revision: FunctionRevisionId,
        unaffected_revision: FunctionRevisionId,
    ) -> CatalogueSnapshot {
        let schema = SchemaDefinition::new(
            SchemaId::from_bytes(id::<8>()),
            QualifiedSemanticName::new(["crm"]).unwrap(),
        );
        let functions = [
            (
                FunctionId::from_bytes(id::<9>()),
                "lookup",
                affected_revision,
            ),
            (
                FunctionId::from_bytes(id::<19>()),
                "unchanged",
                unaffected_revision,
            ),
        ]
        .map(|(function, name, revision)| {
            FunctionDefinition::new(
                function,
                QualifiedSemanticName::new(["crm", name]).unwrap(),
                FunctionDomain::Server,
                vec![],
                FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
                    "found",
                    0,
                    ResolvedType::value(TypeId::from_bytes(id::<71>())),
                )]),
                revision,
                FunctionSecurity::Invoker,
                None,
                FunctionVolatility::Stable,
            )
        });
        CatalogueSnapshot::new_with_functions(
            CatalogueRevisionId::from_bytes(id::<7>()),
            vec![schema],
            vec![],
            functions.into(),
        )
        .unwrap()
    }

    fn mixed_function_origins(
        affected: &FunctionRevisionRecord,
        unaffected: &FunctionRevisionRecord,
    ) -> Vec<DefinitionOrigin> {
        let mut origins = function_origins(affected);
        origins.extend([
            DefinitionOrigin::new(
                DefinitionIdentity::Function(unaffected.function()),
                unaffected.declaration_origin(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::FunctionReturnColumn {
                    owner: unaffected.function(),
                    ordinal: 0,
                },
                unaffected.declaration_origin(),
            ),
        ]);
        origins
    }

    fn standard_context() -> CatalogueHashContext {
        standard_context_with_value_types(vec![standard_boolean_definition()])
    }

    fn standard_boolean_definition() -> ValueTypeDefinition {
        ValueTypeDefinition::primitive(
            TypeId::from_bytes(id::<71>()),
            QualifiedSemanticName::new(["std", "boolean"]).unwrap(),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Persistable,
            "orna.kernel.value.boolean@1",
        )
    }

    fn opaque_standard_context() -> CatalogueHashContext {
        standard_context_with_value_types(vec![
            standard_boolean_definition(),
            ValueTypeDefinition::opaque(
                TypeId::from_bytes(id::<72>()),
                QualifiedSemanticName::new(["std", "token"]).unwrap(),
                "std.token@1",
            ),
        ])
    }

    fn standard_context_with_value_types(
        value_types: Vec<ValueTypeDefinition>,
    ) -> CatalogueHashContext {
        let unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes(id::<3>()),
            0,
            "std/types.orna",
            "CREATE SCHEMA std; CREATE TYPE std.boolean;",
            source_unit_content_digest("CREATE SCHEMA std; CREATE TYPE std.boolean;").unwrap(),
        )
        .unwrap();
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
        let source = StoredSourceRevision::new(
            SourceBundleId::from_bytes(id::<1>()),
            SourceRevisionId::from_bytes(id::<2>()),
            None,
            vec![unit],
            bundle_hash,
            source_revision_record_digest(SourceBundleId::from_bytes(id::<1>()), None, bundle_hash)
                .unwrap(),
        )
        .unwrap();
        let value_type_ids = value_types
            .iter()
            .map(ValueTypeDefinition::id)
            .collect::<Vec<_>>();
        let catalogue = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes(id::<72>()),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes(id::<73>()),
                QualifiedSemanticName::new(["std"]).unwrap(),
            )],
            vec![],
            value_types,
            vec![],
        )
        .unwrap();
        let mut origins = vec![DefinitionOrigin::new(
            DefinitionIdentity::Schema(SchemaId::from_bytes(id::<73>())),
            SourceOrigin::new(SourceUnitId::from_bytes(id::<3>()), 0, 1).unwrap(),
        )];
        origins.extend(
            value_type_ids
                .into_iter()
                .enumerate()
                .map(|(index, value_type_id)| {
                    DefinitionOrigin::new(
                        DefinitionIdentity::ValueType(value_type_id),
                        SourceOrigin::new(
                            SourceUnitId::from_bytes(id::<3>()),
                            index as u32 + 1,
                            index as u32 + 2,
                        )
                        .unwrap(),
                    )
                }),
        );
        let provisional = StandardLibrarySnapshot::new(
            StandardLibraryRevisionId::from_bytes(id::<74>()),
            StandardLibraryDigestVersion::Version1,
            source.clone(),
            "orna.language/1",
            catalogue.clone(),
            origins.clone(),
            digest::<75>(),
        )
        .unwrap();
        let digest = calculate_standard_library_digest_for_test(&provisional).unwrap();
        let standard = StandardLibrarySnapshot::new(
            provisional.revision(),
            provisional.digest_version(),
            source,
            provisional.language_version(),
            catalogue,
            origins,
            digest,
        )
        .unwrap();
        CatalogueHashContext::version_two(verify_standard_library_snapshot(standard).unwrap())
    }

    fn value_type_catalogue() -> CatalogueSnapshot {
        CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes(id::<7>()),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes(id::<8>()),
                QualifiedSemanticName::new(["crm"]).unwrap(),
            )],
            vec![],
            vec![ValueTypeDefinition::primitive(
                TypeId::from_bytes(id::<71>()),
                QualifiedSemanticName::new(["crm", "flag"]).unwrap(),
                ValueTypeMutability::Immutable,
                ValueTypePersistence::Persistable,
                "orna.kernel.value.flag@1",
            )],
            vec![],
        )
        .unwrap()
    }

    fn enum_type_catalogue() -> CatalogueSnapshot {
        CatalogueSnapshot::new_with_enum_types(
            CatalogueRevisionId::from_bytes(id::<7>()),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes(id::<8>()),
                QualifiedSemanticName::new(["crm"]).unwrap(),
            )],
            vec![],
            vec![],
            vec![EnumTypeDefinition::new(
                TypeId::from_bytes(id::<71>()),
                QualifiedSemanticName::new(["crm", "stage"]).unwrap(),
                ["lead", "customer"],
            )],
            vec![],
        )
        .unwrap()
    }

    fn record_value_type_catalogue() -> CatalogueSnapshot {
        CatalogueSnapshot::new_with_record_value_types(
            CatalogueRevisionId::from_bytes(id::<7>()),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes(id::<8>()),
                QualifiedSemanticName::new(["crm"]).unwrap(),
            )],
            vec![],
            vec![],
            vec![],
            vec![RecordValueTypeDefinition::new(
                TypeId::from_bytes(id::<76>()),
                QualifiedSemanticName::new(["crm", "status"]).unwrap(),
                vec![
                    RecordValueFieldDefinition::try_new_descriptor(
                        FieldId::from_bytes(id::<77>()),
                        "active",
                        0,
                        TypeDescriptor::named(TypeId::from_bytes(id::<71>())),
                    )
                    .unwrap(),
                ],
            )],
            vec![],
        )
        .unwrap()
    }

    #[test]
    fn revision_references_accept_exact_record_value_fields() {
        let catalogue = record_value_type_catalogue();
        let target = DefinitionReferenceTarget::Field {
            owner: TypeId::from_bytes(id::<76>()),
            field: FieldId::from_bytes(id::<77>()),
        };
        assert!(reference_target_exists(
            &catalogue,
            None,
            &HashSet::new(),
            target,
        ));
        assert!(!reference_target_exists(
            &catalogue,
            None,
            &HashSet::new(),
            DefinitionReferenceTarget::Field {
                owner: TypeId::from_bytes(id::<76>()),
                field: FieldId::from_bytes(id::<78>()),
            },
        ));
    }

    fn record_graph_standard() -> CatalogueSnapshot {
        CatalogueSnapshot::new(CatalogueRevisionId::from_bytes(id::<150>()), vec![], vec![])
            .unwrap()
    }

    fn record_graph_standard_with_boolean() -> CatalogueSnapshot {
        CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes(id::<151>()),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes(id::<152>()),
                QualifiedSemanticName::new(["std"]).unwrap(),
            )],
            vec![],
            vec![ValueTypeDefinition::primitive(
                TypeId::from_bytes(id::<71>()),
                QualifiedSemanticName::new(["std", "boolean"]).unwrap(),
                ValueTypeMutability::Immutable,
                ValueTypePersistence::Persistable,
                "orna.kernel.value.boolean@1",
            )],
            vec![],
        )
        .unwrap()
    }

    fn record_graph_schema() -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::from_bytes(id::<8>()),
            QualifiedSemanticName::new(["crm"]).unwrap(),
        )
    }

    fn record_graph_type(
        record_byte: u8,
        name: &str,
        fields: Vec<(u8, u32, TypeId)>,
    ) -> RecordValueTypeDefinition {
        let fields = fields
            .into_iter()
            .map(|(field_byte, ordinal, target)| {
                RecordValueFieldDefinition::try_new_descriptor(
                    FieldId::from_bytes([field_byte; 16]),
                    format!("edge_{ordinal}"),
                    ordinal,
                    TypeDescriptor::named(target),
                )
                .unwrap()
            })
            .collect();
        RecordValueTypeDefinition::new(
            TypeId::from_bytes([record_byte; 16]),
            QualifiedSemanticName::new(["crm", name]).unwrap(),
            fields,
        )
    }

    fn record_graph_catalogue(records: Vec<RecordValueTypeDefinition>) -> CatalogueSnapshot {
        CatalogueSnapshot::new_with_record_value_types(
            CatalogueRevisionId::from_bytes(id::<152>()),
            vec![record_graph_schema()],
            vec![],
            vec![],
            vec![],
            records,
            vec![],
        )
        .unwrap()
    }

    fn record_graph_type_with_id(
        id: [u8; 16],
        name: &str,
        fields: Vec<([u8; 16], u32, TypeId)>,
    ) -> RecordValueTypeDefinition {
        let fields = fields
            .into_iter()
            .map(|(field_id, ordinal, target)| {
                RecordValueFieldDefinition::try_new_descriptor(
                    FieldId::from_bytes(field_id),
                    format!("edge_{ordinal}"),
                    ordinal,
                    TypeDescriptor::named(target),
                )
                .unwrap()
            })
            .collect();
        RecordValueTypeDefinition::new(
            TypeId::from_bytes(id),
            QualifiedSemanticName::new(["crm", name]).unwrap(),
            fields,
        )
    }

    /// A deterministic record identity for one chain index beyond one byte.
    fn index_type_id(index: u32) -> [u8; 16] {
        let value = index + 0x1000;
        let mut bytes = [0; 16];
        bytes[0] = (value >> 8) as u8;
        bytes[1] = value as u8;
        bytes
    }

    /// A deterministic field identity for one chain index beyond one byte.
    fn index_field_id(index: u32) -> [u8; 16] {
        let mut bytes = index_type_id(index);
        bytes[15] = 0x01;
        bytes
    }

    fn record_graph_origins_for(catalogue: &CatalogueSnapshot) -> Vec<DefinitionOrigin> {
        let source_unit = SourceUnitId::from_bytes(id::<3>());
        let mut identities = vec![DefinitionIdentity::Schema(SchemaId::from_bytes(id::<8>()))];
        for record in catalogue.record_value_types() {
            identities.push(DefinitionIdentity::ValueType(record.id()));
            for field in record.fields() {
                identities.push(DefinitionIdentity::Field {
                    owner: record.id(),
                    field: field.id(),
                });
            }
        }
        identities
            .into_iter()
            .enumerate()
            .map(|(index, identity)| {
                DefinitionOrigin::new(
                    identity,
                    SourceOrigin::new(source_unit, index as u32, index as u32 + 1).unwrap(),
                )
            })
            .collect()
    }

    fn validate_record_graph(
        records: Vec<RecordValueTypeDefinition>,
    ) -> Result<(), RecordValueFieldDescriptorValidationError> {
        validate_record_value_field_descriptors(
            &record_graph_catalogue(records),
            &record_graph_standard_with_boolean(),
        )
    }

    #[test]
    fn record_value_field_application_record_provenance_and_active_projection() {
        let record_a = record_graph_type(
            200,
            "record_a",
            vec![(210, 0, TypeId::from_bytes(id::<201>()))],
        );
        let record_b = record_graph_type(
            201,
            "record_b",
            vec![(211, 0, TypeId::from_bytes(id::<71>()))],
        );
        let catalogue = record_graph_catalogue(vec![record_a.clone(), record_b.clone()]);

        assert_eq!(
            classify_record_value_field_descriptor(
                &catalogue,
                &record_graph_standard(),
                &TypeDescriptor::named(TypeId::from_bytes(id::<200>())),
            ),
            Ok(RecordValueFieldDescriptorClass::ApplicationRecord(
                TypeId::from_bytes(id::<200>())
            ))
        );
        assert_eq!(
            classify_record_value_field_descriptor(
                &catalogue,
                &record_graph_standard(),
                &TypeDescriptor::named(TypeId::from_bytes(id::<201>())),
            ),
            Ok(RecordValueFieldDescriptorClass::ApplicationRecord(
                TypeId::from_bytes(id::<201>())
            ))
        );
        assert_eq!(
            classify_record_value_field_descriptor(
                &catalogue,
                &record_graph_standard(),
                &TypeDescriptor::reference(TypeId::from_bytes(id::<200>())),
            ),
            Err(RecordValueFieldDescriptorClassificationError::Unsupported)
        );

        let active = active_for_flat_type_conversion(catalogue, standard_context());
        assert_eq!(
            active.record_value_field_descriptor_runtime_type(&TypeDescriptor::named(
                TypeId::from_bytes(id::<200>())
            )),
            Some(ResolvedType::named(TypeId::from_bytes(id::<200>())))
        );
        assert_eq!(
            active.record_value_field_descriptor_runtime_type(&TypeDescriptor::named(
                TypeId::from_bytes(id::<71>())
            )),
            Some(ResolvedType::scalar(StandardScalar::Boolean))
        );
        assert_eq!(
            active.record_value_field_descriptor_runtime_type(&TypeDescriptor::reference(
                TypeId::from_bytes(id::<200>())
            )),
            None
        );
        assert_eq!(validate_record_graph(vec![record_a, record_b]), Ok(()));
    }

    #[test]
    fn record_value_field_self_cycle_is_rejected_exactly() {
        let record_a = record_graph_type(
            200,
            "record_a",
            vec![(210, 0, TypeId::from_bytes(id::<200>()))],
        );
        assert_eq!(
            validate_record_graph(vec![record_a]).unwrap_err(),
            RecordValueFieldDescriptorValidationError::RecursiveRecordValueField {
                record_value_type: TypeId::from_bytes(id::<200>()),
                field: FieldId::from_bytes(id::<210>()),
                nested_record_value_type: TypeId::from_bytes(id::<200>()),
            }
        );
    }

    #[test]
    fn record_value_field_three_cycle_closes_at_the_exact_back_edge() {
        let record_a = record_graph_type(
            200,
            "record_a",
            vec![(210, 0, TypeId::from_bytes(id::<201>()))],
        );
        let record_b = record_graph_type(
            201,
            "record_b",
            vec![(211, 0, TypeId::from_bytes(id::<202>()))],
        );
        let record_c = record_graph_type(
            202,
            "record_c",
            vec![(212, 0, TypeId::from_bytes(id::<200>()))],
        );
        assert_eq!(
            validate_record_graph(vec![record_a, record_b, record_c]).unwrap_err(),
            RecordValueFieldDescriptorValidationError::RecursiveRecordValueField {
                record_value_type: TypeId::from_bytes(id::<202>()),
                field: FieldId::from_bytes(id::<212>()),
                nested_record_value_type: TypeId::from_bytes(id::<200>()),
            }
        );
    }

    #[test]
    fn record_value_field_cycle_selection_is_deterministic_across_orders() {
        // The A-B-C-A cycle must report the same closing edge when the record
        // input order is reversed.
        let forward = vec![
            record_graph_type(
                200,
                "record_a",
                vec![(210, 0, TypeId::from_bytes(id::<201>()))],
            ),
            record_graph_type(
                201,
                "record_b",
                vec![(211, 0, TypeId::from_bytes(id::<202>()))],
            ),
            record_graph_type(
                202,
                "record_c",
                vec![(212, 0, TypeId::from_bytes(id::<200>()))],
            ),
        ];
        let reversed = forward.iter().rev().cloned().collect::<Vec<_>>();
        let expected = RecordValueFieldDescriptorValidationError::RecursiveRecordValueField {
            record_value_type: TypeId::from_bytes(id::<202>()),
            field: FieldId::from_bytes(id::<212>()),
            nested_record_value_type: TypeId::from_bytes(id::<200>()),
        };
        assert_eq!(validate_record_graph(forward).unwrap_err(), expected);
        assert_eq!(validate_record_graph(reversed).unwrap_err(), expected);

        // The closing edge is selected by ordinal, not by input field order.
        let first_field_first = vec![
            record_graph_type(
                200,
                "record_a",
                vec![
                    (210, 0, TypeId::from_bytes(id::<201>())),
                    (214, 1, TypeId::from_bytes(id::<202>())),
                ],
            ),
            record_graph_type(
                201,
                "record_b",
                vec![(211, 0, TypeId::from_bytes(id::<200>()))],
            ),
            record_graph_type(
                202,
                "record_c",
                vec![(212, 0, TypeId::from_bytes(id::<71>()))],
            ),
        ];
        assert_eq!(
            validate_record_graph(first_field_first).unwrap_err(),
            RecordValueFieldDescriptorValidationError::RecursiveRecordValueField {
                record_value_type: TypeId::from_bytes(id::<201>()),
                field: FieldId::from_bytes(id::<211>()),
                nested_record_value_type: TypeId::from_bytes(id::<200>()),
            }
        );
        let second_field_first = vec![
            record_graph_type(
                200,
                "record_a",
                vec![
                    (214, 0, TypeId::from_bytes(id::<201>())),
                    (210, 1, TypeId::from_bytes(id::<202>())),
                ],
            ),
            record_graph_type(
                201,
                "record_b",
                vec![(211, 0, TypeId::from_bytes(id::<200>()))],
            ),
            record_graph_type(
                202,
                "record_c",
                vec![(212, 0, TypeId::from_bytes(id::<71>()))],
            ),
        ];
        assert_eq!(
            validate_record_graph(second_field_first).unwrap_err(),
            RecordValueFieldDescriptorValidationError::RecursiveRecordValueField {
                record_value_type: TypeId::from_bytes(id::<201>()),
                field: FieldId::from_bytes(id::<211>()),
                nested_record_value_type: TypeId::from_bytes(id::<200>()),
            }
        );
    }

    #[test]
    fn record_value_field_diamond_dag_is_accepted() {
        let records = vec![
            record_graph_type(
                200,
                "record_a",
                vec![
                    (210, 0, TypeId::from_bytes(id::<201>())),
                    (214, 1, TypeId::from_bytes(id::<202>())),
                ],
            ),
            record_graph_type(
                201,
                "record_b",
                vec![(211, 0, TypeId::from_bytes(id::<203>()))],
            ),
            record_graph_type(
                202,
                "record_c",
                vec![(212, 0, TypeId::from_bytes(id::<203>()))],
            ),
            record_graph_type(
                203,
                "record_d",
                vec![(213, 0, TypeId::from_bytes(id::<71>()))],
            ),
        ];
        assert_eq!(validate_record_graph(records), Ok(()));
    }

    #[test]
    fn record_value_field_classification_errors_precede_cycles() {
        // The application catalogue also defines a record at the standard
        // boolean identity, so record_a's first field is ambiguous, while the
        // record_a -> record_b -> record_a cycle exists. Classification runs
        // before cycle detection, so the ambiguous field must win.
        let colliding = record_graph_type(
            71,
            "record_71",
            vec![(215, 0, TypeId::from_bytes(id::<201>()))],
        );
        let record_a = record_graph_type(
            200,
            "record_a",
            vec![
                (210, 0, TypeId::from_bytes(id::<71>())),
                (214, 1, TypeId::from_bytes(id::<201>())),
            ],
        );
        let record_b = record_graph_type(
            201,
            "record_b",
            vec![(211, 0, TypeId::from_bytes(id::<200>()))],
        );
        let catalogue = record_graph_catalogue(vec![colliding, record_a, record_b]);
        let error = validate_record_value_field_descriptors(
            &catalogue,
            &record_graph_standard_with_boolean(),
        )
        .unwrap_err();
        assert_eq!(
            error,
            RecordValueFieldDescriptorValidationError::Ambiguous {
                record_value_type: TypeId::from_bytes(id::<200>()),
                field: FieldId::from_bytes(id::<210>()),
                type_id: TypeId::from_bytes(id::<71>()),
            }
        );
    }

    #[test]
    fn record_value_field_cycles_precede_depth() {
        let record_a = record_graph_type(
            200,
            "record_a",
            vec![
                (210, 0, TypeId::from_bytes(id::<201>())),
                (214, 1, TypeId::from_bytes(id::<202>())),
            ],
        );
        let record_b = record_graph_type(
            201,
            "record_b",
            vec![(211, 0, TypeId::from_bytes(id::<200>()))],
        );
        let chain = (0..40)
            .map(|index| {
                let record_byte = 202 + index as u8;
                let next_byte = if index == 39 {
                    71
                } else {
                    202 + index as u8 + 1
                };
                record_graph_type(
                    record_byte,
                    &format!("chain_{index}"),
                    vec![(250, 0, TypeId::from_bytes([next_byte; 16]))],
                )
            })
            .collect::<Vec<_>>();
        let mut records = vec![record_a, record_b];
        records.extend(chain);
        assert_eq!(
            validate_record_graph(records).unwrap_err(),
            RecordValueFieldDescriptorValidationError::RecursiveRecordValueField {
                record_value_type: TypeId::from_bytes(id::<201>()),
                field: FieldId::from_bytes(id::<211>()),
                nested_record_value_type: TypeId::from_bytes(id::<200>()),
            }
        );
    }

    #[test]
    fn record_value_field_nesting_accepts_32_edges_and_rejects_33_exactly() {
        let accepted = (0..33)
            .map(|index| {
                let next_byte = if index == 32 {
                    71
                } else {
                    200 + index as u8 + 1
                };
                record_graph_type(
                    200 + index as u8,
                    &format!("chain_{index}"),
                    vec![(210 + index as u8, 0, TypeId::from_bytes([next_byte; 16]))],
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(validate_record_graph(accepted), Ok(()));

        let too_deep = (0..34)
            .map(|index| {
                let next_byte = if index == 33 {
                    71
                } else {
                    200 + index as u8 + 1
                };
                record_graph_type(
                    200 + index as u8,
                    &format!("chain_{index}"),
                    vec![(210 + index as u8, 0, TypeId::from_bytes([next_byte; 16]))],
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            validate_record_graph(too_deep).unwrap_err(),
            RecordValueFieldDescriptorValidationError::RecordValueNestingTooDeep {
                record_value_type: TypeId::from_bytes(id::<232>()),
                field: FieldId::from_bytes(id::<242>()),
                nested_record_value_type: TypeId::from_bytes(id::<233>()),
                maximum: 32,
                actual: 33,
            }
        );
    }

    #[test]
    fn record_value_field_long_acyclic_chain_returns_the_exact_depth_error_without_crashing() {
        let chain = (0..4096)
            .map(|index| {
                let next = if index == 4095 {
                    TypeId::from_bytes([71; 16])
                } else {
                    TypeId::from_bytes(index_type_id(index + 1))
                };
                record_graph_type_with_id(
                    index_type_id(index),
                    &format!("chain_{index}"),
                    vec![(index_field_id(index), 0, next)],
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            validate_record_graph(chain).unwrap_err(),
            RecordValueFieldDescriptorValidationError::RecordValueNestingTooDeep {
                record_value_type: TypeId::from_bytes(index_type_id(32)),
                field: FieldId::from_bytes(index_field_id(32)),
                nested_record_value_type: TypeId::from_bytes(index_type_id(33)),
                maximum: 32,
                actual: 33,
            }
        );
    }

    #[test]
    fn record_value_field_shared_suffix_memoisation_still_fails_on_a_later_deep_root() {
        let shallow_root = record_graph_type(
            2,
            "shallow_root",
            vec![(212, 0, TypeId::from_bytes([3; 16]))],
        );
        let suffix_leaf = record_graph_type(
            3,
            "suffix_leaf",
            vec![(213, 0, TypeId::from_bytes([71; 16]))],
        );
        let deep_root =
            record_graph_type(4, "deep_root", vec![(214, 0, TypeId::from_bytes([5; 16]))]);
        let deep_chain = (0..32)
            .map(|index| {
                let next = if index == 31 {
                    TypeId::from_bytes([3; 16])
                } else {
                    TypeId::from_bytes([5 + index as u8 + 1; 16])
                };
                record_graph_type(
                    5 + index as u8,
                    &format!("deep_{index}"),
                    vec![(215 + index as u8, 0, next)],
                )
            })
            .collect::<Vec<_>>();
        let mut records = vec![shallow_root, suffix_leaf, deep_root];
        records.extend(deep_chain);
        assert_eq!(
            validate_record_graph(records).unwrap_err(),
            RecordValueFieldDescriptorValidationError::RecordValueNestingTooDeep {
                record_value_type: TypeId::from_bytes([36; 16]),
                field: FieldId::from_bytes([246; 16]),
                nested_record_value_type: TypeId::from_bytes([3; 16]),
                maximum: 32,
                actual: 33,
            }
        );
    }

    fn deployable_with(
        catalogue: CatalogueSnapshot,
        context: CatalogueHashContext,
    ) -> Result<DeployableRevision, RevisionInvariantError> {
        let expected_base = RevisionPair::new(
            SourceRevisionId::from_bytes(id::<78>()),
            CatalogueRevisionId::from_bytes(id::<79>()),
        );
        DeployableRevision::new_with_catalogue_hash_context(
            DeployableRevisionInput::new(
                expected_base,
                source(Some(expected_base.source())),
                expected_base.catalogue(),
                catalogue.clone(),
                digest::<7>(),
                DeployableRevisionContent::new(
                    record_graph_origins_for(&catalogue),
                    vec![],
                    vec![],
                    vec![],
                )
                .with_current_function_revisions(vec![]),
            ),
            context,
        )
    }

    #[test]
    fn deployable_classification_returns_application_record_for_nested_targets() {
        let catalogue = record_graph_catalogue(vec![
            record_graph_type(
                200,
                "outer",
                vec![(210, 0, TypeId::from_bytes(id::<201>()))],
            ),
            record_graph_type(201, "inner", vec![(211, 0, TypeId::from_bytes(id::<71>()))]),
        ]);
        let deployable = deployable_with(catalogue, standard_context())
            .expect("nested record catalogue must admit");
        assert_eq!(
            deployable.record_value_field_descriptor_class(&TypeDescriptor::named(
                TypeId::from_bytes(id::<200>())
            )),
            Ok(RecordValueFieldDescriptorClass::ApplicationRecord(
                TypeId::from_bytes(id::<200>())
            ))
        );
        assert_eq!(
            deployable.record_value_field_descriptor_class(&TypeDescriptor::named(
                TypeId::from_bytes(id::<201>())
            )),
            Ok(RecordValueFieldDescriptorClass::ApplicationRecord(
                TypeId::from_bytes(id::<201>())
            ))
        );
    }

    #[test]
    fn revision_admission_maps_cycles_and_depth_to_exact_errors() {
        let cyclic = record_graph_catalogue(vec![record_graph_type(
            200,
            "record_a",
            vec![(210, 0, TypeId::from_bytes([200; 16]))],
        )]);
        let expected_cycle = RevisionInvariantError::RecursiveRecordValueField {
            record_value_type: TypeId::from_bytes([200; 16]),
            field: FieldId::from_bytes([210; 16]),
            nested_record_value_type: TypeId::from_bytes([200; 16]),
        };
        let deployable_error = deployable_with(cyclic.clone(), standard_context())
            .expect_err("a self cycle must fail deployable admission");
        assert_eq!(deployable_error, expected_cycle);
        assert_eq!(
            deployable_error.to_string(),
            "record value fields must not form a recursive cycle"
        );

        let source = source(None);
        let pair = RevisionPair::new(source.id(), cyclic.revision());
        let active_origins = record_graph_origins_for(&cyclic);
        let active_error = ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                pair,
                source,
                cyclic,
                digest::<7>(),
                ActiveRevisionContent::new(vec![], vec![], active_origins, vec![]),
            ),
            standard_context(),
        )
        .expect_err("a self cycle must fail active admission");
        assert_eq!(active_error, expected_cycle);

        let too_deep = record_graph_catalogue(
            (0..34)
                .map(|index| {
                    let next_byte = if index == 33 {
                        71
                    } else {
                        200 + index as u8 + 1
                    };
                    record_graph_type(
                        200 + index as u8,
                        &format!("chain_{index}"),
                        vec![(210 + index as u8, 0, TypeId::from_bytes([next_byte; 16]))],
                    )
                })
                .collect(),
        );
        let expected_depth = RevisionInvariantError::RecordValueNestingTooDeep {
            record_value_type: TypeId::from_bytes([232; 16]),
            field: FieldId::from_bytes([242; 16]),
            nested_record_value_type: TypeId::from_bytes([233; 16]),
            maximum: 32,
            actual: 33,
        };
        let deployable_depth = deployable_with(too_deep.clone(), standard_context())
            .expect_err("a depth-33 chain must fail deployable admission");
        assert_eq!(deployable_depth, expected_depth);
        assert_eq!(
            deployable_depth.to_string(),
            "record value nesting exceeds the maximum depth"
        );
    }

    #[test]
    fn application_record_colliding_with_a_standard_enum_is_ambiguous() {
        let candidate = record_graph_catalogue(vec![
            record_graph_type(
                75,
                "colliding",
                vec![(210, 0, TypeId::from_bytes([200; 16]))],
            ),
            record_graph_type(200, "leaf", vec![(211, 0, TypeId::from_bytes([71; 16]))]),
        ]);
        let deployable = deployable_with(candidate, flat_type_standard_context())
            .expect("colliding record catalogue must admit");
        assert_eq!(
            deployable.record_value_field_descriptor_class(&TypeDescriptor::named(
                TypeId::from_bytes([75; 16])
            )),
            Err(RecordValueFieldDescriptorError::Ambiguous {
                type_id: TypeId::from_bytes([75; 16]),
            })
        );

        let with_field = record_graph_catalogue(vec![
            record_graph_type(
                75,
                "colliding",
                vec![(211, 0, TypeId::from_bytes([71; 16]))],
            ),
            record_graph_type(200, "user", vec![(210, 0, TypeId::from_bytes([75; 16]))]),
        ]);
        let expected = RevisionInvariantError::AmbiguousRecordValueFieldType {
            record_value_type: TypeId::from_bytes([200; 16]),
            field: FieldId::from_bytes([210; 16]),
            type_id: TypeId::from_bytes([75; 16]),
        };
        let error = deployable_with(with_field, flat_type_standard_context())
            .expect_err("an ambiguous record field must fail admission");
        assert_eq!(error, expected);
        assert_eq!(
            error.to_string(),
            "record field type is present in both application and standard catalogues"
        );
    }

    fn catalogue_with_record_value_slot() -> CatalogueSnapshot {
        let record_value_type = TypeId::from_bytes(id::<76>());
        CatalogueSnapshot::new_with_functions_and_record_value_types(
            CatalogueRevisionId::from_bytes(id::<7>()),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes(id::<8>()),
                QualifiedSemanticName::new(["crm"]).unwrap(),
            )],
            vec![ObjectTypeDefinition::new(
                TypeId::from_bytes(id::<80>()),
                QualifiedSemanticName::new(["crm", "contact"]).unwrap(),
                vec![FieldDefinition::new(
                    FieldId::from_bytes(id::<81>()),
                    "status",
                    0,
                    ResolvedType::named(record_value_type),
                    false,
                    false,
                    None,
                    None,
                )],
            )],
            vec![],
            vec![],
            vec![RecordValueTypeDefinition::new(
                record_value_type,
                QualifiedSemanticName::new(["crm", "status"]).unwrap(),
                vec![
                    RecordValueFieldDefinition::try_new_descriptor(
                        FieldId::from_bytes(id::<77>()),
                        "active",
                        0,
                        TypeDescriptor::named(TypeId::from_bytes(id::<71>())),
                    )
                    .unwrap(),
                ],
            )],
            vec![],
            vec![FunctionDefinition::new(
                FunctionId::from_bytes(id::<82>()),
                QualifiedSemanticName::new(["crm", "read_status"]).unwrap(),
                FunctionDomain::Server,
                vec![],
                FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
                    "status",
                    0,
                    ResolvedType::named(record_value_type),
                )]),
                FunctionRevisionId::from_bytes(id::<83>()),
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::ReadOnly),
                FunctionVolatility::Stable,
            )],
        )
        .unwrap()
    }

    fn record_value_type_origins() -> Vec<DefinitionOrigin> {
        let source = SourceUnitId::from_bytes(id::<3>());
        vec![
            DefinitionOrigin::new(
                DefinitionIdentity::Schema(SchemaId::from_bytes(id::<8>())),
                SourceOrigin::new(source, 0, 1).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::ValueType(TypeId::from_bytes(id::<76>())),
                SourceOrigin::new(source, 1, 2).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::Field {
                    owner: TypeId::from_bytes(id::<76>()),
                    field: FieldId::from_bytes(id::<77>()),
                },
                SourceOrigin::new(source, 2, 3).unwrap(),
            ),
        ]
    }

    #[test]
    fn record_value_types_require_version_two_at_every_revision_admission_boundary() {
        let record_value_type = TypeId::from_bytes(id::<76>());
        let expected = RevisionInvariantError::RecordValueTypeRequiresCatalogueHashVersionTwo {
            record_value_type,
        };

        assert_eq!(
            validate_catalogue_hash_context_coherence(
                &CatalogueHashContext::version_one(),
                &record_value_type_catalogue(),
                &[],
                &[],
                &[],
            ),
            Err(expected.clone())
        );
        assert!(
            validate_catalogue_hash_context_coherence(
                &standard_context(),
                &record_value_type_catalogue(),
                &[],
                &[],
                &[],
            )
            .is_ok()
        );
        let version_two_source = source(None);
        let version_two_catalogue = record_value_type_catalogue();
        let version_two_pair =
            RevisionPair::new(version_two_source.id(), version_two_catalogue.revision());
        assert!(
            ActiveDatabaseRevision::new_with_catalogue_hash_context(
                ActiveDatabaseRevisionInput::new(
                    version_two_pair,
                    version_two_source,
                    version_two_catalogue,
                    digest::<7>(),
                    ActiveRevisionContent::new(vec![], vec![], record_value_type_origins(), vec![]),
                ),
                standard_context(),
            )
            .is_ok()
        );

        let active_source = source(None);
        let catalogue = record_value_type_catalogue();
        let pair = RevisionPair::new(active_source.id(), catalogue.revision());
        assert_eq!(
            ActiveDatabaseRevision::new(
                pair,
                active_source,
                catalogue,
                digest::<7>(),
                vec![],
                vec![],
                vec![],
                vec![],
            )
            .unwrap_err(),
            expected.clone()
        );

        let expected_base = RevisionPair::new(
            SourceRevisionId::from_bytes(id::<78>()),
            CatalogueRevisionId::from_bytes(id::<79>()),
        );
        let deployable = DeployableRevision::new_with_catalogue_hash_context(
            DeployableRevisionInput::new(
                expected_base,
                source(Some(expected_base.source())),
                expected_base.catalogue(),
                record_value_type_catalogue(),
                digest::<7>(),
                DeployableRevisionContent::new(record_value_type_origins(), vec![], vec![], vec![])
                    .with_current_function_revisions(vec![]),
            ),
            standard_context(),
        )
        .unwrap();
        assert_eq!(
            deployable.record_value_field_descriptor_class(&TypeDescriptor::named(
                TypeId::from_bytes(id::<71>()),
            )),
            Ok(RecordValueFieldDescriptorClass::StandardPrimitive(
                TypeId::from_bytes(id::<71>()),
            ))
        );
        assert_eq!(
            deployable.record_value_field_descriptor_class(&TypeDescriptor::reference(
                TypeId::from_bytes(id::<80>()),
            )),
            Err(RecordValueFieldDescriptorError::Unsupported)
        );
        let version_one = DeployableRevision::new(
            expected_base,
            source(Some(expected_base.source())),
            expected_base.catalogue(),
            empty_catalogue(),
            digest::<7>(),
            vec![],
            vec![],
            vec![],
            vec![],
        )
        .unwrap();
        assert_eq!(
            version_one.record_value_field_descriptor_class(&TypeDescriptor::named(
                TypeId::from_bytes(id::<71>()),
            )),
            Err(RecordValueFieldDescriptorError::StandardLibraryUnavailable)
        );
        let collision = DeployableRevision::new_with_catalogue_hash_context(
            DeployableRevisionInput::new(
                expected_base,
                source(Some(expected_base.source())),
                expected_base.catalogue(),
                enum_type_catalogue(),
                digest::<7>(),
                DeployableRevisionContent::new(value_type_origins(), vec![], vec![], vec![])
                    .with_current_function_revisions(vec![]),
            ),
            standard_context(),
        )
        .unwrap();
        assert_eq!(
            collision.record_value_field_descriptor_class(&TypeDescriptor::named(
                TypeId::from_bytes(id::<71>()),
            )),
            Err(RecordValueFieldDescriptorError::Ambiguous {
                type_id: TypeId::from_bytes(id::<71>()),
            })
        );
        let standard_enum = DeployableRevision::new_with_catalogue_hash_context(
            DeployableRevisionInput::new(
                expected_base,
                source(Some(expected_base.source())),
                expected_base.catalogue(),
                empty_catalogue(),
                digest::<7>(),
                DeployableRevisionContent::new(vec![], vec![], vec![], vec![])
                    .with_current_function_revisions(vec![]),
            ),
            flat_type_standard_context(),
        )
        .unwrap();
        assert_eq!(
            standard_enum.record_value_field_descriptor_class(&TypeDescriptor::named(
                TypeId::from_bytes(id::<75>()),
            )),
            Ok(RecordValueFieldDescriptorClass::StandardEnum(
                TypeId::from_bytes(id::<75>()),
            ))
        );
        assert_eq!(
            DeployableRevision::new(
                expected_base,
                source(Some(expected_base.source())),
                expected_base.catalogue(),
                record_value_type_catalogue(),
                digest::<7>(),
                vec![],
                vec![],
                vec![],
                vec![],
            )
            .unwrap_err(),
            expected.clone()
        );

        let standard_revision = StandardLibraryRevisionId::from_bytes(id::<74>());
        assert_eq!(
            StandardLibrarySnapshot::new(
                standard_revision,
                StandardLibraryDigestVersion::Version1,
                source(None),
                "orna.language/1",
                record_value_type_catalogue(),
                vec![],
                digest::<75>(),
            )
            .unwrap_err(),
            RevisionInvariantError::UnsupportedStandardLibraryDefinition {
                revision: standard_revision,
            }
        );
        assert_eq!(
            expected.to_string(),
            "record value types require catalogue hash version 2"
        );
        let identities = expected_definition_identities(&record_value_type_catalogue(), &[]);
        assert!(identities.contains(&DefinitionIdentity::ValueType(record_value_type)));
        assert!(identities.contains(&DefinitionIdentity::Field {
            owner: record_value_type,
            field: FieldId::from_bytes(id::<77>()),
        }));
    }

    #[test]
    fn record_value_types_can_enter_object_and_rows_slots() {
        assert!(
            validate_resolved_type_slots(&standard_context(), &catalogue_with_record_value_slot(),)
                .is_ok()
        );
    }

    #[test]
    fn record_value_field_type_policy_is_closed_and_uses_pinned_standard_primitives() {
        let accepted_contracts = [
            ("orna.kernel.value.boolean@1", StandardScalar::Boolean),
            ("orna.kernel.value.integer@1", StandardScalar::Integer),
            ("orna.kernel.value.bigint@1", StandardScalar::BigInt),
            ("orna.kernel.value.float@1", StandardScalar::Float),
            (
                "orna.kernel.value.character-large-object@1",
                StandardScalar::CharacterLargeObject,
            ),
            (
                "orna.kernel.value.binary-large-object@1",
                StandardScalar::BinaryLargeObject,
            ),
        ];
        let accepted_values = accepted_contracts
            .iter()
            .enumerate()
            .map(|(index, (contract, _))| {
                ValueTypeDefinition::primitive(
                    TypeId::from_bytes([index as u8 + 1; 16]),
                    QualifiedSemanticName::new(["std", "types", *contract]).unwrap(),
                    ValueTypeMutability::Immutable,
                    ValueTypePersistence::Persistable,
                    *contract,
                )
            })
            .collect::<Vec<_>>();
        let standard = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes(id::<90>()),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes(id::<91>()),
                QualifiedSemanticName::new(["std", "types"]).unwrap(),
            )],
            vec![],
            accepted_values,
            vec![],
        )
        .unwrap();
        let application = empty_catalogue();
        for (index, (_, scalar)) in accepted_contracts.iter().enumerate() {
            let resolved_type = ResolvedType::value(TypeId::from_bytes([index as u8 + 1; 16]));
            assert!(
                record_value_field_runtime_type(&application, &standard, resolved_type).is_some()
            );
            assert_eq!(
                record_value_field_runtime_type(&application, &standard, resolved_type),
                Some(ResolvedType::scalar(*scalar))
            );
        }

        for contract in [
            "orna.kernel.value.decimal@1",
            "orna.kernel.value.uuid@1",
            "orna.kernel.value.date@1",
            "orna.kernel.value.time@1",
            "orna.kernel.value.timestamp@1",
            "orna.kernel.value.duration@1",
            "orna.kernel.value.void@1",
            "orna.kernel.value.custom@1",
        ] {
            assert!(
                accepted_record_scalar(&ValueTypeDefinition::primitive(
                    TypeId::from_bytes(id::<92>()),
                    QualifiedSemanticName::new(["std", "types", "excluded"]).unwrap(),
                    ValueTypeMutability::Immutable,
                    ValueTypePersistence::Persistable,
                    contract,
                ))
                .is_none()
            );
        }

        let application_primitive = TypeId::from_bytes(id::<93>());
        let application_enum = TypeId::from_bytes(id::<94>());
        let standard_enum = TypeId::from_bytes(id::<95>());
        let application = CatalogueSnapshot::new_with_enum_types(
            CatalogueRevisionId::from_bytes(id::<96>()),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes(id::<97>()),
                QualifiedSemanticName::new(["app"]).unwrap(),
            )],
            vec![],
            vec![ValueTypeDefinition::primitive(
                application_primitive,
                QualifiedSemanticName::new(["app", "flag"]).unwrap(),
                ValueTypeMutability::Immutable,
                ValueTypePersistence::Persistable,
                "orna.kernel.value.boolean@1",
            )],
            vec![EnumTypeDefinition::new(
                application_enum,
                QualifiedSemanticName::new(["app", "phase"]).unwrap(),
                ["new"],
            )],
            vec![],
        )
        .unwrap();
        let standard = CatalogueSnapshot::new_with_enum_types(
            CatalogueRevisionId::from_bytes(id::<98>()),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes(id::<99>()),
                QualifiedSemanticName::new(["std"]).unwrap(),
            )],
            vec![],
            vec![],
            vec![EnumTypeDefinition::new(
                standard_enum,
                QualifiedSemanticName::new(["std", "phase"]).unwrap(),
                ["new"],
            )],
            vec![],
        )
        .unwrap();
        assert!(
            record_value_field_runtime_type(
                &application,
                &standard,
                ResolvedType::value(application_primitive),
            )
            .is_none()
        );
        assert!(
            record_value_field_runtime_type(
                &application,
                &standard,
                ResolvedType::named(application_enum),
            )
            .is_some()
        );
        assert_eq!(
            classify_record_value_field_descriptor(
                &application,
                &standard,
                &TypeDescriptor::named(application_enum),
            ),
            Ok(RecordValueFieldDescriptorClass::ApplicationEnum(
                application_enum,
            ))
        );
        assert_eq!(
            classify_record_value_field_descriptor(
                &application,
                &standard,
                &TypeDescriptor::named(standard_enum),
            ),
            Ok(RecordValueFieldDescriptorClass::StandardEnum(standard_enum))
        );
        let colliding_standard = CatalogueSnapshot::new_with_enum_types(
            CatalogueRevisionId::from_bytes(id::<103>()),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes(id::<104>()),
                QualifiedSemanticName::new(["std"]).unwrap(),
            )],
            vec![],
            vec![],
            vec![EnumTypeDefinition::new(
                application_enum,
                QualifiedSemanticName::new(["std", "phase"]).unwrap(),
                ["new"],
            )],
            vec![],
        )
        .unwrap();
        assert_eq!(
            classify_record_value_field_descriptor(
                &application,
                &colliding_standard,
                &TypeDescriptor::named(application_enum),
            ),
            Err(RecordValueFieldDescriptorClassificationError::Ambiguous {
                type_id: application_enum,
            })
        );
        assert!(
            record_value_field_runtime_type(
                &application,
                &standard,
                ResolvedType::named(standard_enum),
            )
            .is_some()
        );
        for unsupported in [
            ResolvedType::value(TypeId::from_bytes(id::<100>())),
            ResolvedType::named(TypeId::from_bytes(id::<101>())),
            ResolvedType::scalar(StandardScalar::Boolean),
            ResolvedType::reference(TypeId::from_bytes(id::<102>())),
        ] {
            assert!(
                record_value_field_runtime_type(&application, &standard, unsupported,).is_none()
            );
        }

        let collision = TypeId::from_bytes(id::<71>());
        let collision_catalogue = CatalogueSnapshot::new_with_record_value_types(
            CatalogueRevisionId::from_bytes(id::<96>()),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes(id::<97>()),
                QualifiedSemanticName::new(["app"]).unwrap(),
            )],
            vec![],
            vec![],
            vec![EnumTypeDefinition::new(
                collision,
                QualifiedSemanticName::new(["app", "collision"]).unwrap(),
                ["value"],
            )],
            vec![RecordValueTypeDefinition::new(
                TypeId::from_bytes(id::<98>()),
                QualifiedSemanticName::new(["app", "record"]).unwrap(),
                vec![
                    RecordValueFieldDefinition::try_new_descriptor(
                        FieldId::from_bytes(id::<99>()),
                        "value",
                        0,
                        TypeDescriptor::named(collision),
                    )
                    .unwrap(),
                ],
            )],
            vec![],
        )
        .unwrap();
        assert_eq!(
            validate_record_value_field_types(&standard_context(), &collision_catalogue),
            Err(RevisionInvariantError::AmbiguousRecordValueFieldType {
                record_value_type: TypeId::from_bytes(id::<98>()),
                field: FieldId::from_bytes(id::<99>()),
                type_id: collision,
            })
        );
        let error = RevisionInvariantError::AmbiguousRecordValueFieldType {
            record_value_type: TypeId::from_bytes(id::<98>()),
            field: FieldId::from_bytes(id::<99>()),
            type_id: collision,
        };
        assert_eq!(
            error.to_string(),
            "record field type is present in both application and standard catalogues"
        );
        assert!(std::error::Error::source(&error).is_none());

        let cases = [
            (
                RecordValueFieldDescriptorError::StandardLibraryUnavailable,
                "deployable revision has no pinned standard library for record field classification",
            ),
            (
                RecordValueFieldDescriptorError::Unsupported,
                "record field descriptor is not supported by the deployable revision",
            ),
            (
                RecordValueFieldDescriptorError::Ambiguous { type_id: collision },
                "record field type is present in both application and standard catalogues",
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(error.to_string(), expected);
            assert!(std::error::Error::source(&error).is_none());
        }
    }

    #[test]
    fn enum_types_use_value_origins_and_require_the_version_two_catalogue_contract() {
        let catalogue = enum_type_catalogue();
        let enum_identity = DefinitionIdentity::ValueType(TypeId::from_bytes(id::<71>()));
        assert!(expected_definition_identities(&catalogue, &[]).contains(&enum_identity));
        assert!(definition_exists(
            &catalogue,
            &HashSet::new(),
            enum_identity
        ));
        assert!(matches!(
            validate_catalogue_hash_context_version_one(&catalogue, &[], &[], &[]),
            Err(RevisionInvariantError::ValueTypeDefinitionRequiresCatalogueHashVersionTwo {
                value_type,
            }) if value_type == TypeId::from_bytes(id::<71>())
        ));
    }

    fn value_type_origins() -> Vec<DefinitionOrigin> {
        let source = SourceUnitId::from_bytes(id::<3>());
        vec![
            DefinitionOrigin::new(
                DefinitionIdentity::Schema(SchemaId::from_bytes(id::<8>())),
                SourceOrigin::new(source, 0, 1).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::ValueType(TypeId::from_bytes(id::<71>())),
                SourceOrigin::new(source, 1, 2).unwrap(),
            ),
        ]
    }

    fn binding_catalogue() -> (CatalogueSnapshot, TypeBindingId) {
        let binding = TypeBinding::qualified(
            QualifiedSemanticName::new(["crm", "contact_alias"]).unwrap(),
            TypeId::from_bytes(id::<12>()),
        )
        .unwrap();
        let binding_id = binding.id();
        let catalogue = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes(id::<7>()),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes(id::<8>()),
                QualifiedSemanticName::new(["crm"]).unwrap(),
            )],
            vec![ObjectTypeDefinition::new(
                TypeId::from_bytes(id::<12>()),
                QualifiedSemanticName::new(["crm", "contact"]).unwrap(),
                vec![],
            )],
            vec![],
            vec![binding],
        )
        .unwrap();
        (catalogue, binding_id)
    }

    fn binding_origins(binding: TypeBindingId) -> Vec<DefinitionOrigin> {
        let source = SourceUnitId::from_bytes(id::<3>());
        vec![
            DefinitionOrigin::new(
                DefinitionIdentity::Schema(SchemaId::from_bytes(id::<8>())),
                SourceOrigin::new(source, 0, 1).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::ObjectType(TypeId::from_bytes(id::<12>())),
                SourceOrigin::new(source, 1, 2).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::TypeBinding(binding),
                SourceOrigin::new(source, 2, 3).unwrap(),
            ),
        ]
    }

    fn historical_function_revision(
        function: FunctionId,
        revision: FunctionRevisionId,
        revision_number: u64,
        source_unit: SourceUnitId,
    ) -> FunctionRevisionRecord {
        FunctionRevisionRecord::new(
            function,
            revision,
            revision_number,
            SourceOrigin::new(source_unit, 4, 23).unwrap(),
            digest::<31>(),
            digest::<32>(),
            "orna-1",
            artifact(),
        )
        .unwrap()
    }

    fn active_with_history(
        historical_function_revisions: Vec<FunctionRevisionRecord>,
    ) -> Result<ActiveDatabaseRevision, RevisionInvariantError> {
        let source = source(None);
        let current_revision = function_revision();
        let catalogue = function_catalogue(current_revision.id());
        let origins = function_origins(&current_revision);
        let pair = RevisionPair::new(source.id(), catalogue.revision());

        ActiveDatabaseRevision::new_with_history(
            pair,
            source,
            catalogue,
            digest::<7>(),
            vec![],
            vec![current_revision],
            historical_function_revisions,
            origins,
            vec![],
        )
    }

    fn function_origins(revision: &FunctionRevisionRecord) -> Vec<DefinitionOrigin> {
        vec![
            DefinitionOrigin::new(
                DefinitionIdentity::Schema(SchemaId::from_bytes(id::<8>())),
                SourceOrigin::new(SourceUnitId::from_bytes(id::<3>()), 0, 18).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::Function(revision.function()),
                revision.declaration_origin(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::FunctionReturnColumn {
                    owner: revision.function(),
                    ordinal: 0,
                },
                revision.declaration_origin(),
            ),
        ]
    }

    fn write_origins(revision: &FunctionRevisionRecord) -> Vec<DefinitionOrigin> {
        let mut origins = function_origins(revision);
        let source = SourceUnitId::from_bytes(id::<3>());
        origins.extend([
            DefinitionOrigin::new(
                DefinitionIdentity::ObjectType(TypeId::from_bytes(id::<12>())),
                SourceOrigin::new(source, 0, 10).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::Field {
                    owner: TypeId::from_bytes(id::<12>()),
                    field: FieldId::from_bytes(id::<13>()),
                },
                SourceOrigin::new(source, 10, 18).unwrap(),
            ),
        ]);
        origins
    }

    #[test]
    fn retains_an_empty_active_revision_without_inventing_source() {
        let source = StoredSourceRevision::new(
            SourceBundleId::from_bytes(id::<1>()),
            SourceRevisionId::from_bytes(id::<2>()),
            None,
            vec![],
            digest::<1>(),
            digest::<2>(),
        )
        .unwrap();
        let catalogue = empty_catalogue();
        let pair = RevisionPair::new(source.id(), catalogue.revision());

        let active = ActiveDatabaseRevision::new(
            pair,
            source,
            catalogue,
            digest::<7>(),
            vec![],
            vec![],
            vec![],
            vec![],
        )
        .unwrap();

        assert!(active.source().units().is_empty());
        assert!(active.function_revisions().is_empty());
        assert!(active.historical_function_revisions().is_empty());
        assert_eq!(active.pair(), pair);
        assert_eq!(active.catalogue_hash(), digest::<7>());
        assert_eq!(
            active.catalogue_hash_context().version(),
            CatalogueHashVersion::Version1
        );
    }

    #[test]
    fn active_and_deployable_revisions_reject_the_reserved_health_function_identity() {
        let function = crate::security::CATALOGUE_HEALTH_FUNCTION_ID;
        let revision = function_revision_fixture(
            function,
            FunctionRevisionId::from_bytes(id::<90>()),
            digest::<90>(),
            digest::<91>(),
        );
        let catalogue = function_catalogue_with_identity(
            function,
            revision.id(),
            vec![],
            ResolvedType::scalar(StandardScalar::Boolean),
        );
        let origins = function_origins(&revision);
        let expected = RevisionInvariantError::ReservedSystemFunctionIdentity { function };
        let active_source = source(None);

        let active_error = ActiveDatabaseRevision::new(
            RevisionPair::new(active_source.id(), catalogue.revision()),
            active_source,
            catalogue.clone(),
            digest::<7>(),
            vec![],
            vec![revision.clone()],
            origins.clone(),
            vec![],
        )
        .unwrap_err();
        assert_eq!(active_error, expected);

        let expected_base = RevisionPair::new(
            SourceRevisionId::from_bytes(id::<20>()),
            CatalogueRevisionId::from_bytes(id::<21>()),
        );
        let deployable_error = DeployableRevision::new(
            expected_base,
            source(Some(expected_base.source())),
            expected_base.catalogue(),
            catalogue,
            digest::<7>(),
            origins,
            vec![],
            vec![revision],
            vec![],
        )
        .unwrap_err();
        assert_eq!(deployable_error, expected);
        assert_eq!(
            deployable_error.to_string(),
            "the reserved system function identity cannot enter an application catalogue"
        );
    }

    fn invocation_carrier_value_type(id: TypeId, parts: &[&str]) -> ValueTypeDefinition {
        ValueTypeDefinition::primitive(
            id,
            QualifiedSemanticName::new(parts.iter().copied()).unwrap(),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Persistable,
            "orna.test.invocation-carrier@1",
        )
    }

    fn invocation_carrier_catalogue(value_types: Vec<ValueTypeDefinition>) -> CatalogueSnapshot {
        CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes(id::<112>()),
            vec![
                SchemaDefinition::new(
                    SchemaId::from_bytes(id::<113>()),
                    QualifiedSemanticName::new(["sys"]).unwrap(),
                ),
                SchemaDefinition::new(
                    SchemaId::from_bytes(id::<114>()),
                    QualifiedSemanticName::new(["sys", "invoke"]).unwrap(),
                ),
            ],
            vec![],
            value_types,
            vec![],
        )
        .unwrap()
    }

    fn invocation_carrier_origins(catalogue: &CatalogueSnapshot) -> Vec<DefinitionOrigin> {
        let source_unit = SourceUnitId::from_bytes(id::<3>());
        let mut origins = vec![
            DefinitionOrigin::new(
                DefinitionIdentity::Schema(SchemaId::from_bytes(id::<113>())),
                SourceOrigin::new(source_unit, 0, 1).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::Schema(SchemaId::from_bytes(id::<114>())),
                SourceOrigin::new(source_unit, 1, 2).unwrap(),
            ),
        ];
        origins.extend(
            catalogue
                .value_types()
                .iter()
                .enumerate()
                .map(|(index, value_type)| {
                    DefinitionOrigin::new(
                        DefinitionIdentity::ValueType(value_type.id()),
                        SourceOrigin::new(source_unit, index as u32 + 2, index as u32 + 3).unwrap(),
                    )
                }),
        );
        origins
    }

    fn active_invocation_carrier_admission(
        catalogue: CatalogueSnapshot,
        origins: Vec<DefinitionOrigin>,
    ) -> Result<ActiveDatabaseRevision, RevisionInvariantError> {
        let source = source(None);
        ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                RevisionPair::new(source.id(), catalogue.revision()),
                source,
                catalogue,
                digest::<115>(),
                ActiveRevisionContent::new(vec![], vec![], origins, vec![]),
            ),
            standard_context(),
        )
    }

    fn deployable_invocation_carrier_admission(
        catalogue: CatalogueSnapshot,
        origins: Vec<DefinitionOrigin>,
    ) -> Result<DeployableRevision, RevisionInvariantError> {
        let expected_base = RevisionPair::new(
            SourceRevisionId::from_bytes(id::<117>()),
            CatalogueRevisionId::from_bytes(id::<118>()),
        );
        DeployableRevision::new_with_catalogue_hash_context(
            DeployableRevisionInput::new(
                expected_base,
                source(Some(expected_base.source())),
                expected_base.catalogue(),
                catalogue,
                digest::<115>(),
                DeployableRevisionContent::new(origins, vec![], vec![], vec![])
                    .with_current_function_revisions(vec![]),
            ),
            standard_context(),
        )
    }

    fn standard_invocation_carrier_admission(
        catalogue: CatalogueSnapshot,
        origins: Vec<DefinitionOrigin>,
    ) -> Result<StandardLibrarySnapshot, RevisionInvariantError> {
        StandardLibrarySnapshot::new(
            StandardLibraryRevisionId::from_bytes(id::<115>()),
            StandardLibraryDigestVersion::Version1,
            source(None),
            "orna.language/1",
            catalogue,
            origins,
            digest::<115>(),
        )
    }

    #[test]
    fn public_application_and_standard_admission_reject_each_reserved_carrier_identity() {
        for &carrier in crate::system::INVOCATION_CARRIERS {
            let catalogue = invocation_carrier_catalogue(vec![invocation_carrier_value_type(
                carrier.id(),
                carrier.name_parts(),
            )]);
            let expected = RevisionInvariantError::ReservedInvocationCarrierIdentity {
                carrier: carrier.id(),
            };

            assert_eq!(
                active_invocation_carrier_admission(catalogue.clone(), vec![]).unwrap_err(),
                expected
            );
            assert_eq!(
                deployable_invocation_carrier_admission(catalogue.clone(), vec![]).unwrap_err(),
                expected
            );
            assert_eq!(
                standard_invocation_carrier_admission(catalogue, vec![]).unwrap_err(),
                expected
            );
        }
    }

    #[test]
    fn public_application_and_standard_admission_reject_each_reserved_carrier_name() {
        for (index, &carrier) in crate::system::INVOCATION_CARRIERS.iter().enumerate() {
            let type_id = TypeId::from_bytes([0x80 + index as u8; 16]);
            let catalogue = invocation_carrier_catalogue(vec![invocation_carrier_value_type(
                type_id,
                carrier.name_parts(),
            )]);
            let expected = RevisionInvariantError::ReservedInvocationCarrierName { type_id };

            assert_eq!(
                active_invocation_carrier_admission(catalogue.clone(), vec![]).unwrap_err(),
                expected
            );
            assert_eq!(
                deployable_invocation_carrier_admission(catalogue.clone(), vec![]).unwrap_err(),
                expected
            );
            assert_eq!(
                standard_invocation_carrier_admission(catalogue, vec![]).unwrap_err(),
                expected
            );
        }
    }

    #[test]
    fn carrier_identities_globally_precede_carrier_names_at_public_application_admission() {
        let value_name_only = TypeId::from_bytes([0xa0; 16]);
        let catalogue = invocation_carrier_catalogue(vec![
            invocation_carrier_value_type(value_name_only, &["sys", "invoke", "Value"]),
            invocation_carrier_value_type(
                crate::system::SYS_INVOKE_EVENT_TYPE_ID,
                &["sys", "invoke", "Event2"],
            ),
            invocation_carrier_value_type(
                crate::system::SYS_INVOKE_REQUEST_TYPE_ID,
                &["sys", "invoke", "Request2"],
            ),
        ]);
        let expected = RevisionInvariantError::ReservedInvocationCarrierIdentity {
            carrier: crate::system::SYS_INVOKE_REQUEST_TYPE_ID,
        };

        assert_eq!(
            active_invocation_carrier_admission(catalogue.clone(), vec![]).unwrap_err(),
            expected
        );
        assert_eq!(
            deployable_invocation_carrier_admission(catalogue.clone(), vec![]).unwrap_err(),
            expected
        );
        assert_eq!(
            standard_invocation_carrier_admission(catalogue, vec![]).unwrap_err(),
            expected
        );
    }

    #[test]
    fn carrier_names_use_the_global_registry_order_at_public_application_admission() {
        let catalogue = invocation_carrier_catalogue(vec![
            invocation_carrier_value_type(
                TypeId::from_bytes([0x90; 16]),
                &["sys", "invoke", "Event"],
            ),
            invocation_carrier_value_type(
                TypeId::from_bytes([0x91; 16]),
                &["sys", "invoke", "Request"],
            ),
            invocation_carrier_value_type(
                TypeId::from_bytes([0x92; 16]),
                &["sys", "invoke", "Value"],
            ),
        ]);
        let expected = RevisionInvariantError::ReservedInvocationCarrierName {
            type_id: TypeId::from_bytes([0x92; 16]),
        };

        assert_eq!(
            active_invocation_carrier_admission(catalogue.clone(), vec![]).unwrap_err(),
            expected
        );
        assert_eq!(
            deployable_invocation_carrier_admission(catalogue.clone(), vec![]).unwrap_err(),
            expected
        );
        assert_eq!(
            standard_invocation_carrier_admission(catalogue, vec![]).unwrap_err(),
            expected
        );
    }

    #[test]
    fn neighbouring_invocation_carrier_names_remain_admissible_at_public_boundaries() {
        let catalogue = invocation_carrier_catalogue(vec![invocation_carrier_value_type(
            TypeId::from_bytes(id::<116>()),
            &["sys", "invoke", "Value2"],
        )]);
        let origins = invocation_carrier_origins(&catalogue);

        assert!(active_invocation_carrier_admission(catalogue.clone(), origins.clone()).is_ok());
        assert!(
            deployable_invocation_carrier_admission(catalogue.clone(), origins.clone()).is_ok()
        );
        assert!(standard_invocation_carrier_admission(catalogue, origins).is_ok());
    }

    #[test]
    fn active_and_deployable_revisions_reject_the_invoke_system_identity() {
        let function = crate::system::SYS_INVOKE_FUNCTION_ID;
        let revision = function_revision_fixture(
            function,
            FunctionRevisionId::from_bytes(id::<92>()),
            digest::<92>(),
            digest::<93>(),
        );
        let catalogue = function_catalogue_with_identity(
            function,
            revision.id(),
            vec![],
            ResolvedType::scalar(StandardScalar::Boolean),
        );
        let origins = function_origins(&revision);
        let expected = RevisionInvariantError::ReservedSystemFunctionIdentity { function };
        let active_source = source(None);

        let active_error = ActiveDatabaseRevision::new(
            RevisionPair::new(active_source.id(), catalogue.revision()),
            active_source,
            catalogue.clone(),
            digest::<7>(),
            vec![],
            vec![revision.clone()],
            origins.clone(),
            vec![],
        )
        .unwrap_err();
        assert_eq!(active_error, expected);

        let expected_base = RevisionPair::new(
            SourceRevisionId::from_bytes(id::<20>()),
            CatalogueRevisionId::from_bytes(id::<21>()),
        );
        let deployable_error = DeployableRevision::new(
            expected_base,
            source(Some(expected_base.source())),
            expected_base.catalogue(),
            catalogue,
            digest::<7>(),
            origins,
            vec![],
            vec![revision],
            vec![],
        )
        .unwrap_err();
        assert_eq!(deployable_error, expected);
        assert_eq!(
            deployable_error.to_string(),
            "the reserved system function identity cannot enter an application catalogue"
        );
    }

    #[test]
    fn active_and_deployable_revisions_reject_the_health_system_name() {
        let function = FunctionId::from_bytes([0x5a; 16]);
        let revision = function_revision_fixture(
            function,
            FunctionRevisionId::from_bytes(id::<94>()),
            digest::<94>(),
            digest::<95>(),
        );
        let catalogue = function_catalogue_with_functions(vec![function_definition_named(
            &["sys", "catalog", "health"],
            function,
            revision.id(),
            ResolvedType::scalar(StandardScalar::Boolean),
        )]);
        let origins = function_origins(&revision);
        let expected = RevisionInvariantError::ReservedSystemFunctionName { function };
        let active_source = source(None);

        let active_error = ActiveDatabaseRevision::new(
            RevisionPair::new(active_source.id(), catalogue.revision()),
            active_source,
            catalogue.clone(),
            digest::<7>(),
            vec![],
            vec![revision.clone()],
            origins.clone(),
            vec![],
        )
        .unwrap_err();
        assert_eq!(active_error, expected);

        let expected_base = RevisionPair::new(
            SourceRevisionId::from_bytes(id::<20>()),
            CatalogueRevisionId::from_bytes(id::<21>()),
        );
        let deployable_error = DeployableRevision::new(
            expected_base,
            source(Some(expected_base.source())),
            expected_base.catalogue(),
            catalogue,
            digest::<7>(),
            origins,
            vec![],
            vec![revision],
            vec![],
        )
        .unwrap_err();
        assert_eq!(deployable_error, expected);
        assert_eq!(
            deployable_error.to_string(),
            "the reserved system function name cannot enter an application catalogue"
        );
    }

    #[test]
    fn active_and_deployable_revisions_reject_the_invoke_system_name() {
        let function = FunctionId::from_bytes([0x5b; 16]);
        let revision = function_revision_fixture(
            function,
            FunctionRevisionId::from_bytes(id::<96>()),
            digest::<96>(),
            digest::<97>(),
        );
        let catalogue = function_catalogue_with_functions(vec![function_definition_named(
            &["sys", "invoke"],
            function,
            revision.id(),
            ResolvedType::scalar(StandardScalar::Boolean),
        )]);
        let origins = function_origins(&revision);
        let expected = RevisionInvariantError::ReservedSystemFunctionName { function };
        let active_source = source(None);

        let active_error = ActiveDatabaseRevision::new(
            RevisionPair::new(active_source.id(), catalogue.revision()),
            active_source,
            catalogue.clone(),
            digest::<7>(),
            vec![],
            vec![revision.clone()],
            origins.clone(),
            vec![],
        )
        .unwrap_err();
        assert_eq!(active_error, expected);

        let expected_base = RevisionPair::new(
            SourceRevisionId::from_bytes(id::<20>()),
            CatalogueRevisionId::from_bytes(id::<21>()),
        );
        let deployable_error = DeployableRevision::new(
            expected_base,
            source(Some(expected_base.source())),
            expected_base.catalogue(),
            catalogue,
            digest::<7>(),
            origins,
            vec![],
            vec![revision],
            vec![],
        )
        .unwrap_err();
        assert_eq!(deployable_error, expected);
        assert_eq!(
            deployable_error.to_string(),
            "the reserved system function name cannot enter an application catalogue"
        );
    }

    #[test]
    fn identity_collisions_precede_name_collisions_in_registry_order() {
        let name_collision = FunctionId::from_bytes([0x5c; 16]);
        let invoke_identity = crate::system::SYS_INVOKE_FUNCTION_ID;
        let health_identity = crate::security::CATALOGUE_HEALTH_FUNCTION_ID;
        let name_revision = function_revision_fixture(
            name_collision,
            FunctionRevisionId::from_bytes(id::<98>()),
            digest::<98>(),
            digest::<99>(),
        );
        let invoke_revision = function_revision_fixture(
            invoke_identity,
            FunctionRevisionId::from_bytes(id::<100>()),
            digest::<100>(),
            digest::<101>(),
        );
        let health_revision = function_revision_fixture(
            health_identity,
            FunctionRevisionId::from_bytes(id::<102>()),
            digest::<102>(),
            digest::<103>(),
        );
        // Reversed application definition input: the reserved name and the
        // invocation identity appear before the health identity. Admission
        // must still select the health identity collision because every
        // identity collision precedes every name collision and the registry
        // order is health then invocation.
        let catalogue = function_catalogue_with_functions(vec![
            function_definition_named(
                &["sys", "invoke"],
                name_collision,
                name_revision.id(),
                ResolvedType::scalar(StandardScalar::Boolean),
            ),
            function_definition_named(
                &["sys", "lookup"],
                invoke_identity,
                invoke_revision.id(),
                ResolvedType::scalar(StandardScalar::Boolean),
            ),
            function_definition_named(
                &["sys", "probe"],
                health_identity,
                health_revision.id(),
                ResolvedType::scalar(StandardScalar::Boolean),
            ),
        ]);
        let mut origins = function_origins(&name_revision);
        origins.extend(function_origins(&invoke_revision));
        origins.extend(function_origins(&health_revision));
        let expected = RevisionInvariantError::ReservedSystemFunctionIdentity {
            function: health_identity,
        };
        let active_source = source(None);

        let active_error = ActiveDatabaseRevision::new(
            RevisionPair::new(active_source.id(), catalogue.revision()),
            active_source,
            catalogue.clone(),
            digest::<7>(),
            vec![],
            vec![
                name_revision.clone(),
                invoke_revision.clone(),
                health_revision.clone(),
            ],
            origins.clone(),
            vec![],
        )
        .unwrap_err();
        assert_eq!(active_error, expected);

        let expected_base = RevisionPair::new(
            SourceRevisionId::from_bytes(id::<20>()),
            CatalogueRevisionId::from_bytes(id::<21>()),
        );
        let deployable_error = DeployableRevision::new(
            expected_base,
            source(Some(expected_base.source())),
            expected_base.catalogue(),
            catalogue,
            digest::<7>(),
            origins,
            vec![],
            vec![name_revision, invoke_revision, health_revision],
            vec![],
        )
        .unwrap_err();
        assert_eq!(deployable_error, expected);
    }

    #[test]
    fn reserved_invoke_identity_beats_a_health_name_collision_in_one_definition() {
        let function_id = crate::system::SYS_INVOKE_FUNCTION_ID;
        let revision = function_revision_fixture(
            function_id,
            FunctionRevisionId::from_bytes(id::<106>()),
            digest::<106>(),
            digest::<107>(),
        );
        // One application definition carries the invocation identity and the
        // health function's exact name. The identity phase is global and runs
        // before the name phase, so admission must report the invocation
        // identity collision even though the same definition also collides
        // with the health name.
        let catalogue = function_catalogue_with_functions(vec![function_definition_named(
            &["sys", "catalog", "health"],
            function_id,
            revision.id(),
            ResolvedType::scalar(StandardScalar::Boolean),
        )]);
        let origins = function_origins(&revision);
        let expected = RevisionInvariantError::ReservedSystemFunctionIdentity {
            function: function_id,
        };
        let active_source = source(None);

        let active_error = ActiveDatabaseRevision::new(
            RevisionPair::new(active_source.id(), catalogue.revision()),
            active_source,
            catalogue.clone(),
            digest::<7>(),
            vec![],
            vec![revision.clone()],
            origins.clone(),
            vec![],
        )
        .unwrap_err();
        assert_eq!(active_error, expected);

        let expected_base = RevisionPair::new(
            SourceRevisionId::from_bytes(id::<20>()),
            CatalogueRevisionId::from_bytes(id::<21>()),
        );
        let deployable_error = DeployableRevision::new(
            expected_base,
            source(Some(expected_base.source())),
            expected_base.catalogue(),
            catalogue,
            digest::<7>(),
            origins,
            vec![],
            vec![revision],
            vec![],
        )
        .unwrap_err();
        assert_eq!(deployable_error, expected);
        assert_eq!(
            deployable_error.to_string(),
            "the reserved system function identity cannot enter an application catalogue"
        );
    }

    #[test]
    fn name_collisions_use_registry_order_independent_of_application_vector_order() {
        let invoke_name_id = FunctionId::from_bytes([0x5e; 16]);
        let health_name_id = FunctionId::from_bytes([0x5f; 16]);
        let invoke_name_revision = function_revision_fixture(
            invoke_name_id,
            FunctionRevisionId::from_bytes(id::<108>()),
            digest::<108>(),
            digest::<109>(),
        );
        let health_name_revision = function_revision_fixture(
            health_name_id,
            FunctionRevisionId::from_bytes(id::<110>()),
            digest::<110>(),
            digest::<111>(),
        );
        // Reversed application definition input: sys.invoke appears before
        // sys.catalog.health. Name collisions must follow registry order, so
        // admission reports the health-name collision from registry position
        // zero even though the invoke-name definition comes first in the
        // application vector. Both schemas are declared exactly because each
        // function must resolve against its own parent namespace.
        let schema_sys = SchemaDefinition::new(
            SchemaId::from_bytes(id::<8>()),
            QualifiedSemanticName::new(["sys"]).unwrap(),
        );
        let schema_sys_catalog = SchemaDefinition::new(
            SchemaId::from_bytes(id::<9>()),
            QualifiedSemanticName::new(["sys", "catalog"]).unwrap(),
        );
        let catalogue = CatalogueSnapshot::new_with_functions(
            CatalogueRevisionId::from_bytes(id::<7>()),
            vec![schema_sys, schema_sys_catalog],
            vec![],
            vec![
                function_definition_named(
                    &["sys", "invoke"],
                    invoke_name_id,
                    invoke_name_revision.id(),
                    ResolvedType::scalar(StandardScalar::Boolean),
                ),
                function_definition_named(
                    &["sys", "catalog", "health"],
                    health_name_id,
                    health_name_revision.id(),
                    ResolvedType::scalar(StandardScalar::Boolean),
                ),
            ],
        )
        .unwrap();
        // Reserved-name validation is authoritative before origin
        // completeness, so the failing revision's origin fixture suffices.
        let origins = function_origins(&health_name_revision);
        let expected = RevisionInvariantError::ReservedSystemFunctionName {
            function: health_name_id,
        };
        let active_source = source(None);

        let active_error = ActiveDatabaseRevision::new(
            RevisionPair::new(active_source.id(), catalogue.revision()),
            active_source,
            catalogue.clone(),
            digest::<7>(),
            vec![],
            vec![invoke_name_revision.clone(), health_name_revision.clone()],
            origins.clone(),
            vec![],
        )
        .unwrap_err();
        assert_eq!(active_error, expected);

        let expected_base = RevisionPair::new(
            SourceRevisionId::from_bytes(id::<20>()),
            CatalogueRevisionId::from_bytes(id::<21>()),
        );
        let deployable_error = DeployableRevision::new(
            expected_base,
            source(Some(expected_base.source())),
            expected_base.catalogue(),
            catalogue,
            digest::<7>(),
            origins,
            vec![],
            vec![invoke_name_revision, health_name_revision],
            vec![],
        )
        .unwrap_err();
        assert_eq!(deployable_error, expected);
        assert_eq!(
            deployable_error.to_string(),
            "the reserved system function name cannot enter an application catalogue"
        );
    }

    #[test]
    fn neighbouring_sys_names_remain_admissible() {
        let function = FunctionId::from_bytes([0x5d; 16]);
        let revision = function_revision_fixture(
            function,
            FunctionRevisionId::from_bytes(id::<104>()),
            digest::<104>(),
            digest::<105>(),
        );
        let catalogue = function_catalogue_with_functions(vec![function_definition_named(
            &["sys", "probe"],
            function,
            revision.id(),
            ResolvedType::scalar(StandardScalar::Boolean),
        )]);
        let origins = function_origins(&revision);
        let active_source = source(None);
        ActiveDatabaseRevision::new(
            RevisionPair::new(active_source.id(), catalogue.revision()),
            active_source,
            catalogue,
            digest::<7>(),
            vec![],
            vec![revision],
            origins,
            vec![],
        )
        .expect("a neighbouring sys.probe name must remain admissible");
    }

    #[test]
    fn legacy_function_revision_constructor_defaults_to_semantic_hash_version_one() {
        assert_eq!(
            function_revision().semantic_hash_version(),
            FunctionSemanticHashVersion::Version1
        );
    }

    #[test]
    fn active_version_one_rejects_a_version_two_function_revision() {
        let source = source(None);
        let revision = function_revision_v2();
        let catalogue = function_catalogue(revision.id());
        let origins = function_origins(&revision);
        let pair = RevisionPair::new(source.id(), catalogue.revision());

        let result = ActiveDatabaseRevision::new(
            pair,
            source,
            catalogue,
            digest::<7>(),
            vec![],
            vec![revision.clone()],
            origins,
            vec![],
        );

        assert!(matches!(
            result,
            Err(
                RevisionInvariantError::FunctionSemanticHashVersionRequiresCatalogueHashVersionTwo {
                    function,
                    revision: rejected_revision,
                }
            ) if function == revision.function() && rejected_revision == revision.id()
        ));
    }

    #[test]
    fn active_version_one_rejects_each_version_two_catalogue_fact() {
        let active_source = source(None);
        let catalogue = value_type_catalogue();
        let pair = RevisionPair::new(active_source.id(), catalogue.revision());
        assert!(matches!(
            ActiveDatabaseRevision::new(
                pair,
                active_source,
                catalogue,
                digest::<7>(),
                vec![],
                vec![],
                value_type_origins(),
                vec![],
            ),
            Err(RevisionInvariantError::ValueTypeDefinitionRequiresCatalogueHashVersionTwo {
                value_type,
            }) if value_type == TypeId::from_bytes(id::<71>())
        ));

        let active_source = source(None);
        let (catalogue, binding) = binding_catalogue();
        let pair = RevisionPair::new(active_source.id(), catalogue.revision());
        assert!(matches!(
            ActiveDatabaseRevision::new(
                pair,
                active_source,
                catalogue,
                digest::<7>(),
                vec![],
                vec![],
                binding_origins(binding),
                vec![],
            ),
            Err(RevisionInvariantError::TypeBindingRequiresCatalogueHashVersionTwo {
                binding: rejected,
            }) if rejected == binding
        ));

        for identity in [
            DefinitionIdentity::ValueType(TypeId::from_bytes(id::<71>())),
            DefinitionIdentity::TypeBinding(TypeBindingId::from_bytes(id::<72>())),
        ] {
            let active_source = source(None);
            let catalogue = empty_catalogue();
            let pair = RevisionPair::new(active_source.id(), catalogue.revision());
            let origin = DefinitionOrigin::new(
                identity,
                SourceOrigin::new(SourceUnitId::from_bytes(id::<3>()), 0, 1).unwrap(),
            );
            assert!(matches!(
                ActiveDatabaseRevision::new(
                    pair,
                    active_source,
                    catalogue,
                    digest::<7>(),
                    vec![],
                    vec![],
                    vec![origin],
                    vec![],
                ),
                Err(RevisionInvariantError::DefinitionOriginRequiresCatalogueHashVersionTwo {
                    identity: rejected,
                }) if rejected == identity
            ));
        }

        let active_source = source(None);
        let revision = function_revision();
        let catalogue = function_catalogue(revision.id());
        let pair = RevisionPair::new(active_source.id(), catalogue.revision());
        let target = TypeId::from_bytes(id::<71>());
        let reference = DefinitionReference::new(
            revision.function(),
            revision.id(),
            0,
            DefinitionReferenceTarget::ValueType(target),
            DefinitionReferenceKind::NamedType,
            revision.declaration_origin(),
        );
        assert!(matches!(
            ActiveDatabaseRevision::new(
                pair,
                active_source,
                catalogue,
                digest::<7>(),
                vec![],
                vec![revision.clone()],
                function_origins(&revision),
                vec![reference],
            ),
            Err(RevisionInvariantError::ValueTypeReferenceRequiresCatalogueHashVersionTwo {
                function,
                revision: rejected_revision,
                target: rejected_target,
            }) if function == revision.function()
                && rejected_revision == revision.id()
                && rejected_target == target
        ));
    }

    #[test]
    fn active_version_two_requires_value_type_reference_owners_to_use_semantic_version_two() {
        let active_source = source(None);
        let revision = function_revision();
        let catalogue = function_catalogue_v2(revision.id());
        let pair = RevisionPair::new(active_source.id(), catalogue.revision());
        let target = TypeId::from_bytes(id::<71>());
        let reference = DefinitionReference::new(
            revision.function(),
            revision.id(),
            0,
            DefinitionReferenceTarget::ValueType(target),
            DefinitionReferenceKind::NamedType,
            revision.declaration_origin(),
        );
        let input = ActiveDatabaseRevisionInput::new(
            pair,
            active_source,
            catalogue,
            digest::<7>(),
            ActiveRevisionContent::new(
                vec![],
                vec![revision.clone()],
                function_origins(&revision),
                vec![reference],
            ),
        );

        assert!(matches!(
            ActiveDatabaseRevision::new_with_catalogue_hash_context(input, standard_context()),
            Err(RevisionInvariantError::ValueTypeReferenceRequiresFunctionSemanticHashVersionTwo {
                function,
                revision: rejected_revision,
                target: rejected_target,
            }) if function == revision.function()
                && rejected_revision == revision.id()
                && rejected_target == target
        ));
    }

    #[test]
    fn version_two_active_revision_accepts_a_standard_value_type_target() {
        let source = source(None);
        let revision = function_revision_v2();
        let catalogue = function_catalogue_v2(revision.id());
        let pair = RevisionPair::new(source.id(), catalogue.revision());
        let reference = DefinitionReference::new(
            revision.function(),
            revision.id(),
            0,
            DefinitionReferenceTarget::ValueType(TypeId::from_bytes(id::<71>())),
            DefinitionReferenceKind::NamedType,
            revision.declaration_origin(),
        );

        let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                pair,
                source,
                catalogue,
                digest::<7>(),
                ActiveRevisionContent::new(
                    vec![],
                    vec![revision],
                    function_origins(&function_revision_v2()),
                    vec![reference],
                ),
            ),
            standard_context(),
        )
        .unwrap();

        assert_eq!(
            active.catalogue_hash_context().version(),
            CatalogueHashVersion::Version2
        );
        assert!(active.catalogue_hash_context().standard().is_some());
    }

    #[test]
    fn retains_history_for_removed_functions_with_earlier_source_origins() {
        let removed_function = FunctionId::from_bytes(id::<40>());
        let earlier_source_unit = SourceUnitId::from_bytes(id::<41>());
        let historical_revision = historical_function_revision(
            removed_function,
            FunctionRevisionId::from_bytes(id::<42>()),
            7,
            earlier_source_unit,
        );

        let active = active_with_history(vec![historical_revision.clone()]).unwrap();

        assert_eq!(
            active.historical_function_revisions(),
            [historical_revision]
        );
        assert_eq!(
            active.historical_function_revisions()[0]
                .declaration_origin()
                .source_unit(),
            earlier_source_unit
        );
        assert!(
            active
                .catalogue()
                .function_by_id(removed_function)
                .is_none()
        );
    }

    #[test]
    fn rejects_function_revision_id_reused_between_current_and_history() {
        let current_revision = function_revision();
        let duplicate = historical_function_revision(
            FunctionId::from_bytes(id::<43>()),
            current_revision.id(),
            2,
            SourceUnitId::from_bytes(id::<44>()),
        );

        let result = active_with_history(vec![duplicate]);

        assert!(matches!(
            result,
            Err(RevisionInvariantError::DuplicateFunctionRevisionId { revision })
                if revision == current_revision.id()
        ));
    }

    #[test]
    fn rejects_function_revision_number_reused_between_current_and_history() {
        let current_revision = function_revision();
        let duplicate = historical_function_revision(
            current_revision.function(),
            FunctionRevisionId::from_bytes(id::<45>()),
            current_revision.revision_number(),
            SourceUnitId::from_bytes(id::<46>()),
        );

        let result = active_with_history(vec![duplicate]);

        assert!(matches!(
            result,
            Err(RevisionInvariantError::DuplicateFunctionRevisionNumber {
                function,
                revision_number,
            }) if function == current_revision.function()
                && revision_number == current_revision.revision_number()
        ));
    }

    #[test]
    fn rejects_function_revision_hash_pair_reused_between_current_and_history() {
        let current_revision = function_revision();
        let duplicate = FunctionRevisionRecord::new(
            current_revision.function(),
            FunctionRevisionId::from_bytes(id::<47>()),
            2,
            SourceOrigin::new(SourceUnitId::from_bytes(id::<48>()), 4, 23).unwrap(),
            current_revision.declaration_content_hash(),
            current_revision.semantic_hash(),
            current_revision.language_version(),
            current_revision.artifact().clone(),
        )
        .unwrap();

        let result = active_with_history(vec![duplicate]);

        assert!(matches!(
            result,
            Err(RevisionInvariantError::DuplicateFunctionRevisionHashPair {
                function,
                declaration_content_hash,
                semantic_hash,
            }) if function == current_revision.function()
                && declaration_content_hash == current_revision.declaration_content_hash()
                && semantic_hash == current_revision.semantic_hash()
        ));
    }

    #[test]
    fn rejects_function_revision_hash_pair_reused_within_history() {
        let function = FunctionId::from_bytes(id::<49>());
        let first = historical_function_revision(
            function,
            FunctionRevisionId::from_bytes(id::<50>()),
            1,
            SourceUnitId::from_bytes(id::<51>()),
        );
        let duplicate = historical_function_revision(
            function,
            FunctionRevisionId::from_bytes(id::<52>()),
            2,
            SourceUnitId::from_bytes(id::<53>()),
        );

        let result = active_with_history(vec![first.clone(), duplicate]);

        assert!(matches!(
            result,
            Err(RevisionInvariantError::DuplicateFunctionRevisionHashPair {
                function: rejected_function,
                declaration_content_hash,
                semantic_hash,
            }) if rejected_function == function
                && declaration_content_hash == first.declaration_content_hash()
                && semantic_hash == first.semantic_hash()
        ));
    }

    #[test]
    fn retains_exact_utf8_source_and_validates_its_byte_origins() {
        let source = source(None);
        let catalogue = function_catalogue(FunctionRevisionId::from_bytes(id::<11>()));
        let revision = function_revision();
        let origins = function_origins(&revision);
        let pair = RevisionPair::new(source.id(), catalogue.revision());

        let active = ActiveDatabaseRevision::new(
            pair,
            source,
            catalogue,
            digest::<7>(),
            vec![],
            vec![revision],
            origins,
            vec![],
        )
        .unwrap();

        assert_eq!(
            active.source().units()[1].content(),
            "-- cafe\u{301}\nFUNCTION crm.lookup;\n"
        );
        assert_eq!(
            active.function_revisions()[0].artifact().format(),
            "orna.server-plan"
        );
    }

    #[test]
    fn retains_the_historical_origin_of_a_reused_function_revision() {
        let source = source(None);
        let catalogue = function_catalogue(FunctionRevisionId::from_bytes(id::<11>()));
        let current_revision = function_revision();
        let historical_origin = SourceOrigin::new(SourceUnitId::from_bytes(id::<99>()), 4, 23)
            .expect("historical origin is ordered");
        let reused_revision = FunctionRevisionRecord::new(
            current_revision.function(),
            current_revision.id(),
            current_revision.revision_number(),
            historical_origin,
            current_revision.declaration_content_hash(),
            current_revision.semantic_hash(),
            current_revision.language_version(),
            current_revision.artifact().clone(),
        )
        .unwrap();
        let origins = function_origins(&current_revision);
        let pair = RevisionPair::new(source.id(), catalogue.revision());

        let active = ActiveDatabaseRevision::new(
            pair,
            source,
            catalogue,
            digest::<7>(),
            vec![],
            vec![reused_revision],
            origins,
            vec![],
        )
        .unwrap();

        assert_eq!(
            active.function_revisions()[0].declaration_origin(),
            historical_origin
        );
    }

    #[test]
    fn rejects_structural_revision_inconsistencies() {
        let duplicate = StoredSourceRevision::new(
            SourceBundleId::from_bytes(id::<1>()),
            SourceRevisionId::from_bytes(id::<2>()),
            None,
            vec![
                StoredSourceUnit::new(
                    SourceUnitId::from_bytes(id::<3>()),
                    0,
                    "a",
                    "",
                    digest::<1>(),
                )
                .unwrap(),
                StoredSourceUnit::new(
                    SourceUnitId::from_bytes(id::<3>()),
                    1,
                    "b",
                    "",
                    digest::<2>(),
                )
                .unwrap(),
            ],
            digest::<3>(),
            digest::<4>(),
        );
        assert_eq!(
            duplicate,
            Err(RevisionInvariantError::DuplicateSourceUnitId {
                source_unit: SourceUnitId::from_bytes(id::<3>())
            })
        );

        let source_without_origins = source(None);
        let catalogue_without_origins =
            function_catalogue(FunctionRevisionId::from_bytes(id::<11>()));
        let pair_without_origins = RevisionPair::new(
            source_without_origins.id(),
            catalogue_without_origins.revision(),
        );
        assert!(matches!(
            ActiveDatabaseRevision::new(
                pair_without_origins,
                source_without_origins,
                catalogue_without_origins,
                digest::<7>(),
                vec![],
                vec![function_revision()],
                vec![],
                vec![],
            ),
            Err(RevisionInvariantError::MissingDefinitionOrigin { .. })
        ));

        let invalid_source = source(None);
        let catalogue = function_catalogue(FunctionRevisionId::from_bytes(id::<11>()));
        let bad_origin = FunctionRevisionRecord::new(
            FunctionId::from_bytes(id::<9>()),
            FunctionRevisionId::from_bytes(id::<11>()),
            1,
            SourceOrigin::new(SourceUnitId::from_bytes(id::<4>()), 7, 8).unwrap(),
            digest::<1>(),
            digest::<2>(),
            "orna-1",
            artifact(),
        )
        .unwrap();
        let pair = RevisionPair::new(invalid_source.id(), catalogue.revision());
        assert!(matches!(
            ActiveDatabaseRevision::new(
                pair,
                invalid_source,
                catalogue,
                digest::<7>(),
                vec![],
                vec![bad_origin.clone()],
                function_origins(&bad_origin),
                vec![]
            ),
            Err(RevisionInvariantError::SourceOriginNotCharacterBoundary { .. })
        ));
    }

    #[test]
    fn rejects_stale_parent_and_duplicate_reference_ordinals() {
        let expected = RevisionPair::new(
            SourceRevisionId::from_bytes(id::<20>()),
            CatalogueRevisionId::from_bytes(id::<21>()),
        );
        let rejected_source = source(None);
        let catalogue = function_catalogue(FunctionRevisionId::from_bytes(id::<11>()));
        assert!(matches!(
            DeployableRevision::new(
                expected,
                rejected_source,
                expected.catalogue(),
                catalogue,
                digest::<7>(),
                vec![],
                vec![],
                vec![],
                vec![]
            ),
            Err(RevisionInvariantError::DeployableSourceParentMismatch { .. })
        ));

        let current_revision = function_revision();
        let moved_new_revision = FunctionRevisionRecord::new(
            current_revision.function(),
            current_revision.id(),
            current_revision.revision_number(),
            SourceOrigin::new(SourceUnitId::from_bytes(id::<4>()), 11, 29).unwrap(),
            current_revision.declaration_content_hash(),
            current_revision.semantic_hash(),
            current_revision.language_version(),
            current_revision.artifact().clone(),
        )
        .unwrap();
        assert!(matches!(
            DeployableRevision::new(
                expected,
                source(Some(expected.source())),
                expected.catalogue(),
                function_catalogue(current_revision.id()),
                digest::<7>(),
                function_origins(&current_revision),
                vec![],
                vec![moved_new_revision],
                vec![],
            ),
            Err(RevisionInvariantError::FunctionRevisionOriginMismatch { .. })
        ));

        let duplicate_reference_source = source(Some(expected.source()));
        let catalogue = function_catalogue(FunctionRevisionId::from_bytes(id::<11>()));
        let revision = function_revision();
        let reference = DefinitionReference::new(
            revision.function(),
            revision.id(),
            0,
            DefinitionReferenceTarget::Function(revision.function()),
            DefinitionReferenceKind::FunctionCall,
            revision.declaration_origin(),
        );
        assert!(matches!(
            DeployableRevision::new(
                expected,
                duplicate_reference_source,
                expected.catalogue(),
                catalogue,
                digest::<7>(),
                function_origins(&revision),
                vec![],
                vec![revision],
                vec![reference.clone(), reference],
            ),
            Err(RevisionInvariantError::DuplicateReferenceOrdinal { .. })
        ));

        let unknown_target_revision = function_revision();
        let unknown_target = DefinitionReference::new(
            unknown_target_revision.function(),
            unknown_target_revision.id(),
            0,
            DefinitionReferenceTarget::Expression(ExpressionId::from_bytes(id::<99>())),
            DefinitionReferenceKind::Expression,
            unknown_target_revision.declaration_origin(),
        );
        assert!(matches!(
            DeployableRevision::new(
                expected,
                source(Some(expected.source())),
                expected.catalogue(),
                function_catalogue(FunctionRevisionId::from_bytes(id::<11>())),
                digest::<7>(),
                function_origins(&unknown_target_revision),
                vec![],
                vec![unknown_target_revision],
                vec![unknown_target],
            ),
            Err(RevisionInvariantError::ReferenceTargetNotInRevision { .. })
        ));

        let mismatched_revision = function_revision();
        let mismatched_reference = DefinitionReference::new(
            mismatched_revision.function(),
            mismatched_revision.id(),
            0,
            DefinitionReferenceTarget::Function(mismatched_revision.function()),
            DefinitionReferenceKind::QueryObject,
            mismatched_revision.declaration_origin(),
        );
        assert!(matches!(
            DeployableRevision::new(
                expected,
                source(Some(expected.source())),
                expected.catalogue(),
                function_catalogue(mismatched_revision.id()),
                digest::<7>(),
                function_origins(&mismatched_revision),
                vec![],
                vec![mismatched_revision],
                vec![mismatched_reference],
            ),
            Err(RevisionInvariantError::ReferenceKindTargetMismatch { .. })
        ));

        let candidate_revision = function_revision();
        let candidate_reference = DefinitionReference::new(
            candidate_revision.function(),
            candidate_revision.id(),
            0,
            DefinitionReferenceTarget::Function(candidate_revision.function()),
            DefinitionReferenceKind::FunctionCall,
            candidate_revision.declaration_origin(),
        );
        let deployable = DeployableRevision::new(
            expected,
            source(Some(expected.source())),
            expected.catalogue(),
            function_catalogue(FunctionRevisionId::from_bytes(id::<11>())),
            digest::<7>(),
            function_origins(&candidate_revision),
            vec![],
            vec![candidate_revision],
            vec![candidate_reference],
        )
        .unwrap();
        assert_eq!(
            deployable.candidate_pair(),
            RevisionPair::new(
                SourceRevisionId::from_bytes(id::<2>()),
                CatalogueRevisionId::from_bytes(id::<7>()),
            )
        );
        assert_eq!(deployable.catalogue_hash(), digest::<7>());
        assert_eq!(
            deployable.catalogue_hash_context().version(),
            CatalogueHashVersion::Version1
        );

        let revision = function_revision_v2();
        let standard_reference = DefinitionReference::new(
            revision.function(),
            revision.id(),
            0,
            DefinitionReferenceTarget::ValueType(TypeId::from_bytes(id::<71>())),
            DefinitionReferenceKind::NamedType,
            revision.declaration_origin(),
        );
        let deployable = DeployableRevision::new_with_catalogue_hash_context(
            DeployableRevisionInput::new(
                expected,
                source(Some(expected.source())),
                expected.catalogue(),
                function_catalogue_v2(revision.id()),
                digest::<7>(),
                DeployableRevisionContent::new(
                    function_origins(&revision),
                    vec![],
                    vec![revision.clone()],
                    vec![standard_reference],
                )
                .with_current_function_revisions(vec![revision]),
            ),
            standard_context(),
        )
        .unwrap();
        assert_eq!(
            deployable.catalogue_hash_context().version(),
            CatalogueHashVersion::Version2
        );
    }

    #[test]
    fn deployable_version_one_rejects_each_version_two_catalogue_fact() {
        let expected = RevisionPair::new(
            SourceRevisionId::from_bytes(id::<20>()),
            CatalogueRevisionId::from_bytes(id::<21>()),
        );
        let catalogue = value_type_catalogue();
        assert!(matches!(
            DeployableRevision::new(
                expected,
                source(Some(expected.source())),
                expected.catalogue(),
                catalogue,
                digest::<7>(),
                value_type_origins(),
                vec![],
                vec![],
                vec![],
            ),
            Err(RevisionInvariantError::ValueTypeDefinitionRequiresCatalogueHashVersionTwo {
                value_type,
            }) if value_type == TypeId::from_bytes(id::<71>())
        ));

        let (catalogue, binding) = binding_catalogue();
        assert!(matches!(
            DeployableRevision::new(
                expected,
                source(Some(expected.source())),
                expected.catalogue(),
                catalogue,
                digest::<7>(),
                binding_origins(binding),
                vec![],
                vec![],
                vec![],
            ),
            Err(RevisionInvariantError::TypeBindingRequiresCatalogueHashVersionTwo {
                binding: rejected,
            }) if rejected == binding
        ));

        for identity in [
            DefinitionIdentity::ValueType(TypeId::from_bytes(id::<71>())),
            DefinitionIdentity::TypeBinding(TypeBindingId::from_bytes(id::<72>())),
        ] {
            let origin = DefinitionOrigin::new(
                identity,
                SourceOrigin::new(SourceUnitId::from_bytes(id::<3>()), 0, 1).unwrap(),
            );
            assert!(matches!(
                DeployableRevision::new(
                    expected,
                    source(Some(expected.source())),
                    expected.catalogue(),
                    empty_catalogue(),
                    digest::<7>(),
                    vec![origin],
                    vec![],
                    vec![],
                    vec![],
                ),
                Err(RevisionInvariantError::DefinitionOriginRequiresCatalogueHashVersionTwo {
                    identity: rejected,
                }) if rejected == identity
            ));
        }

        let revision = function_revision();
        let catalogue = function_catalogue(revision.id());
        let target = TypeId::from_bytes(id::<71>());
        let reference = DefinitionReference::new(
            revision.function(),
            revision.id(),
            0,
            DefinitionReferenceTarget::ValueType(target),
            DefinitionReferenceKind::NamedType,
            revision.declaration_origin(),
        );
        assert!(matches!(
            DeployableRevision::new(
                expected,
                source(Some(expected.source())),
                expected.catalogue(),
                catalogue,
                digest::<7>(),
                function_origins(&revision),
                vec![],
                vec![revision.clone()],
                vec![reference],
            ),
            Err(RevisionInvariantError::ValueTypeReferenceRequiresCatalogueHashVersionTwo {
                function,
                revision: rejected_revision,
                target: rejected_target,
            }) if function == revision.function()
                && rejected_revision == revision.id()
                && rejected_target == target
        ));

        let revision = function_revision_v2();
        let catalogue = function_catalogue(revision.id());
        assert!(matches!(
            DeployableRevision::new(
                expected,
                source(Some(expected.source())),
                expected.catalogue(),
                catalogue,
                digest::<7>(),
                function_origins(&revision),
                vec![],
                vec![revision.clone()],
                vec![],
            ),
            Err(
                RevisionInvariantError::FunctionSemanticHashVersionRequiresCatalogueHashVersionTwo {
                    function,
                    revision: rejected_revision,
                }
            ) if function == revision.function() && rejected_revision == revision.id()
        ));
    }

    #[test]
    fn deployable_version_two_requires_value_type_reference_owners_to_use_semantic_version_two() {
        let expected = RevisionPair::new(
            SourceRevisionId::from_bytes(id::<20>()),
            CatalogueRevisionId::from_bytes(id::<21>()),
        );
        let revision = function_revision();
        let catalogue = function_catalogue_v2(revision.id());
        let target = TypeId::from_bytes(id::<71>());
        let reference = DefinitionReference::new(
            revision.function(),
            revision.id(),
            0,
            DefinitionReferenceTarget::ValueType(target),
            DefinitionReferenceKind::NamedType,
            revision.declaration_origin(),
        );
        let input = DeployableRevisionInput::new(
            expected,
            source(Some(expected.source())),
            expected.catalogue(),
            catalogue,
            digest::<7>(),
            DeployableRevisionContent::new(
                function_origins(&revision),
                vec![],
                vec![],
                vec![reference],
            )
            .with_current_function_revisions(vec![revision.clone()]),
        );

        assert!(matches!(
            DeployableRevision::new_with_catalogue_hash_context(input, standard_context()),
            Err(RevisionInvariantError::ValueTypeReferenceRequiresFunctionSemanticHashVersionTwo {
                function,
                revision: rejected_revision,
                target: rejected_target,
            }) if function == revision.function()
                && rejected_revision == revision.id()
                && rejected_target == target
        ));
    }

    #[test]
    fn deployable_version_two_accepts_source_only_replay_with_a_reused_version_two_owner() {
        let expected = RevisionPair::new(
            SourceRevisionId::from_bytes(id::<20>()),
            CatalogueRevisionId::from_bytes(id::<21>()),
        );
        let revision = function_revision_v2();
        let target = TypeId::from_bytes(id::<71>());
        let reference = DefinitionReference::new(
            revision.function(),
            revision.id(),
            0,
            DefinitionReferenceTarget::ValueType(target),
            DefinitionReferenceKind::NamedType,
            revision.declaration_origin(),
        );
        let content = DeployableRevisionContent::new(
            function_origins(&revision),
            vec![],
            vec![],
            vec![reference],
        )
        .with_current_function_revisions(vec![revision.clone()]);
        let input = DeployableRevisionInput::new(
            expected,
            source(Some(expected.source())),
            expected.catalogue(),
            function_catalogue_v2(revision.id()),
            digest::<7>(),
            content,
        );

        let deployable =
            DeployableRevision::new_with_catalogue_hash_context(input, standard_context()).unwrap();

        assert!(deployable.new_function_revisions().is_empty());
        assert!(validate_persistable_catalogue(&deployable).is_ok());
        assert_eq!(
            deployable.current_function_revisions(),
            Some(&[revision][..])
        );
    }

    #[test]
    fn deployable_version_two_accepts_unaffected_version_one_and_affected_version_two_current_revisions()
     {
        let expected = RevisionPair::new(
            SourceRevisionId::from_bytes(id::<20>()),
            CatalogueRevisionId::from_bytes(id::<21>()),
        );
        let affected = function_revision_v2();
        let unaffected = unaffected_function_revision();
        let reference = DefinitionReference::new(
            affected.function(),
            affected.id(),
            0,
            DefinitionReferenceTarget::ValueType(TypeId::from_bytes(id::<71>())),
            DefinitionReferenceKind::NamedType,
            affected.declaration_origin(),
        );
        let content = DeployableRevisionContent::new(
            mixed_function_origins(&affected, &unaffected),
            vec![],
            vec![],
            vec![reference],
        )
        .with_current_function_revisions(vec![unaffected.clone(), affected.clone()]);
        let input = DeployableRevisionInput::new(
            expected,
            source(Some(expected.source())),
            expected.catalogue(),
            mixed_function_catalogue(affected.id(), unaffected.id()),
            digest::<7>(),
            content,
        );

        let deployable =
            DeployableRevision::new_with_catalogue_hash_context(input, standard_context()).unwrap();

        assert!(deployable.new_function_revisions().is_empty());
        assert_eq!(
            deployable.current_function_revisions(),
            Some(&[unaffected, affected][..])
        );
    }

    #[test]
    fn deployable_version_two_rejects_missing_and_crossed_current_revision_evidence() {
        let expected = RevisionPair::new(
            SourceRevisionId::from_bytes(id::<20>()),
            CatalogueRevisionId::from_bytes(id::<21>()),
        );
        let revision = function_revision_v2();
        let input_without_evidence = DeployableRevisionInput::new(
            expected,
            source(Some(expected.source())),
            expected.catalogue(),
            function_catalogue(revision.id()),
            digest::<7>(),
            DeployableRevisionContent::new(
                function_origins(&revision),
                vec![],
                vec![revision.clone()],
                vec![],
            ),
        );
        assert!(matches!(
            DeployableRevision::new_with_catalogue_hash_context(
                input_without_evidence,
                standard_context(),
            ),
            Err(RevisionInvariantError::DeployableCurrentFunctionRevisionsRequired)
        ));

        let input_with_missing_revision = DeployableRevisionInput::new(
            expected,
            source(Some(expected.source())),
            expected.catalogue(),
            function_catalogue(revision.id()),
            digest::<7>(),
            DeployableRevisionContent::new(function_origins(&revision), vec![], vec![], vec![])
                .with_current_function_revisions(vec![]),
        );
        assert!(matches!(
            DeployableRevision::new_with_catalogue_hash_context(
                input_with_missing_revision,
                standard_context(),
            ),
            Err(RevisionInvariantError::MissingDeployableCurrentFunctionRevision {
                function,
                revision: missing,
            }) if function == revision.function() && missing == revision.id()
        ));

        let crossed = FunctionRevisionRecord::new(
            revision.function(),
            revision.id(),
            revision.revision_number(),
            revision.declaration_origin(),
            revision.declaration_content_hash(),
            digest::<23>(),
            revision.language_version(),
            revision.artifact().clone(),
        )
        .unwrap()
        .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
        let input_with_crossed_revision = DeployableRevisionInput::new(
            expected,
            source(Some(expected.source())),
            expected.catalogue(),
            function_catalogue(revision.id()),
            digest::<7>(),
            DeployableRevisionContent::new(
                function_origins(&revision),
                vec![],
                vec![revision.clone()],
                vec![],
            )
            .with_current_function_revisions(vec![crossed]),
        );
        assert!(matches!(
            DeployableRevision::new_with_catalogue_hash_context(
                input_with_crossed_revision,
                standard_context(),
            ),
            Err(RevisionInvariantError::NewFunctionRevisionCurrentEvidenceMismatch {
                function,
                revision: crossed_revision,
            }) if function == revision.function() && crossed_revision == revision.id()
        ));
    }

    #[test]
    fn revision_construction_validates_resolved_types_in_durable_slot_order() {
        let field_value = TypeId::from_bytes(id::<90>());
        let parameter_value = TypeId::from_bytes(id::<91>());
        let return_value = TypeId::from_bytes(id::<92>());
        let catalogue = resolved_type_slots_catalogue(
            ResolvedType::value(field_value),
            ResolvedType::value(parameter_value),
            FunctionReturn::Single(ResolvedType::value(return_value)),
        );
        let active_source = source(None);
        let input = ActiveDatabaseRevisionInput::new(
            RevisionPair::new(active_source.id(), catalogue.revision()),
            active_source,
            catalogue,
            digest::<7>(),
            ActiveRevisionContent::new(vec![], vec![], vec![], vec![]),
        );

        assert_eq!(
            ActiveDatabaseRevision::new_with_catalogue_hash_context(
                input.clone(),
                CatalogueHashContext::version_one(),
            )
            .unwrap_err(),
            RevisionInvariantError::ResolvedValueRequiresCatalogueHashVersionTwo {
                identity: DefinitionIdentity::Field {
                    owner: TypeId::from_bytes(id::<80>()),
                    field: FieldId::from_bytes(id::<81>()),
                },
                value_type: field_value,
            }
        );
        assert_eq!(
            ActiveDatabaseRevision::new_with_catalogue_hash_context(input, standard_context())
                .unwrap_err(),
            RevisionInvariantError::ResolvedValueTypeNotInPinnedStandard {
                identity: DefinitionIdentity::Field {
                    owner: TypeId::from_bytes(id::<80>()),
                    field: FieldId::from_bytes(id::<81>()),
                },
                value_type: field_value,
            }
        );

        let pinned_value = TypeId::from_bytes(id::<71>());
        let catalogue = resolved_type_slots_catalogue(
            ResolvedType::named(TypeId::from_bytes(id::<80>())),
            ResolvedType::value(parameter_value),
            FunctionReturn::Single(ResolvedType::value(pinned_value)),
        );
        assert_eq!(
            validate_resolved_type_slots(&CatalogueHashContext::version_one(), &catalogue,),
            Err(
                RevisionInvariantError::ResolvedValueRequiresCatalogueHashVersionTwo {
                    identity: DefinitionIdentity::Parameter {
                        owner: FunctionId::from_bytes(id::<82>()),
                        parameter: ParameterId::from_bytes(id::<83>()),
                    },
                    value_type: parameter_value,
                },
            )
        );
        assert_eq!(
            validate_resolved_type_slots(&standard_context(), &catalogue,),
            Err(
                RevisionInvariantError::ResolvedValueTypeNotInPinnedStandard {
                    identity: DefinitionIdentity::Parameter {
                        owner: FunctionId::from_bytes(id::<82>()),
                        parameter: ParameterId::from_bytes(id::<83>()),
                    },
                    value_type: parameter_value,
                }
            )
        );

        let single_value = TypeId::from_bytes(id::<93>());
        let catalogue = resolved_type_slots_catalogue(
            ResolvedType::named(TypeId::from_bytes(id::<80>())),
            ResolvedType::value(pinned_value),
            FunctionReturn::Single(ResolvedType::value(single_value)),
        );
        assert_eq!(
            validate_resolved_type_slots(&standard_context(), &catalogue,),
            Err(
                RevisionInvariantError::ResolvedValueTypeNotInPinnedStandard {
                    identity: DefinitionIdentity::Function(FunctionId::from_bytes(id::<82>())),
                    value_type: single_value,
                }
            )
        );

        let rows_value = TypeId::from_bytes(id::<94>());
        let catalogue = resolved_type_slots_catalogue(
            ResolvedType::named(TypeId::from_bytes(id::<80>())),
            ResolvedType::value(pinned_value),
            FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
                "result",
                0,
                ResolvedType::value(rows_value),
            )]),
        );
        assert_eq!(
            validate_resolved_type_slots(&standard_context(), &catalogue,),
            Err(
                RevisionInvariantError::ResolvedValueTypeNotInPinnedStandard {
                    identity: DefinitionIdentity::FunctionReturnColumn {
                        owner: FunctionId::from_bytes(id::<82>()),
                        ordinal: 0,
                    },
                    value_type: rows_value,
                }
            )
        );
    }

    #[test]
    fn version_two_rejects_a_pinned_opaque_value_in_every_catalogue_slot() {
        let opaque = TypeId::from_bytes(id::<72>());
        let named = ResolvedType::named(TypeId::from_bytes(id::<80>()));
        let cases = [
            (
                resolved_type_slots_catalogue(
                    ResolvedType::value(opaque),
                    named,
                    FunctionReturn::Single(named),
                ),
                DefinitionIdentity::Field {
                    owner: TypeId::from_bytes(id::<80>()),
                    field: FieldId::from_bytes(id::<81>()),
                },
            ),
            (
                resolved_type_slots_catalogue(
                    named,
                    ResolvedType::value(opaque),
                    FunctionReturn::Single(named),
                ),
                DefinitionIdentity::Parameter {
                    owner: FunctionId::from_bytes(id::<82>()),
                    parameter: ParameterId::from_bytes(id::<83>()),
                },
            ),
            (
                resolved_type_slots_catalogue(
                    named,
                    named,
                    FunctionReturn::Single(ResolvedType::value(opaque)),
                ),
                DefinitionIdentity::Function(FunctionId::from_bytes(id::<82>())),
            ),
            (
                resolved_type_slots_catalogue(
                    named,
                    named,
                    FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
                        "token",
                        0,
                        ResolvedType::value(opaque),
                    )]),
                ),
                DefinitionIdentity::FunctionReturnColumn {
                    owner: FunctionId::from_bytes(id::<82>()),
                    ordinal: 0,
                },
            ),
        ];

        for (catalogue, identity) in cases {
            assert_eq!(
                validate_resolved_type_slots(&opaque_standard_context(), &catalogue),
                Err(RevisionInvariantError::OpaqueValueTypeNotAcceptedInSlot {
                    identity,
                    value_type: opaque,
                })
            );
        }
    }

    #[test]
    fn version_two_accepts_only_the_exact_pinned_opaque_client_return() {
        let opaque = TypeId::from_bytes(id::<72>());
        let context = opaque_standard_context();
        let accepted = catalogue_with_opaque_client_return(
            opaque,
            FunctionDomain::Client,
            vec![],
            FunctionSecurity::Invoker,
            FunctionVolatility::Immutable,
        );
        assert_eq!(validate_resolved_type_slots(&context, &accepted), Ok(()));

        let parameter = ParameterDefinition::new(
            ParameterId::from_bytes(id::<83>()),
            "enabled",
            0,
            ResolvedType::value(TypeId::from_bytes(id::<71>())),
            None,
        );
        for catalogue in [
            catalogue_with_opaque_client_return(
                opaque,
                FunctionDomain::Client,
                vec![parameter],
                FunctionSecurity::Invoker,
                FunctionVolatility::Immutable,
            ),
            catalogue_with_opaque_client_return(
                opaque,
                FunctionDomain::Server,
                vec![],
                FunctionSecurity::Invoker,
                FunctionVolatility::Immutable,
            ),
            catalogue_with_opaque_client_return(
                opaque,
                FunctionDomain::Client,
                vec![],
                FunctionSecurity::Definer,
                FunctionVolatility::Immutable,
            ),
            catalogue_with_opaque_client_return(
                opaque,
                FunctionDomain::Client,
                vec![],
                FunctionSecurity::Invoker,
                FunctionVolatility::Stable,
            ),
        ] {
            assert_eq!(
                validate_resolved_type_slots(&context, &catalogue),
                Err(RevisionInvariantError::OpaqueValueTypeNotAcceptedInSlot {
                    identity: DefinitionIdentity::Function(FunctionId::from_bytes(id::<82>())),
                    value_type: opaque,
                })
            );
        }
    }

    #[test]
    fn active_and_deployable_revisions_accept_a_standalone_pinned_opaque_definition() {
        let active_source = source(None);
        let active_catalogue = empty_catalogue();
        let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                RevisionPair::new(active_source.id(), active_catalogue.revision()),
                active_source,
                active_catalogue,
                digest::<7>(),
                ActiveRevisionContent::new(vec![], vec![], vec![], vec![]),
            ),
            opaque_standard_context(),
        )
        .unwrap();
        let opaque = active
            .catalogue_hash_context()
            .standard()
            .unwrap()
            .catalogue()
            .value_type_by_id(TypeId::from_bytes(id::<72>()))
            .unwrap();
        assert_eq!(opaque.kind(), ValueTypeKind::Opaque);

        let expected = RevisionPair::new(
            SourceRevisionId::from_bytes(id::<20>()),
            CatalogueRevisionId::from_bytes(id::<21>()),
        );
        let deployable = DeployableRevision::new_with_catalogue_hash_context(
            DeployableRevisionInput::new(
                expected,
                source(Some(expected.source())),
                expected.catalogue(),
                empty_catalogue(),
                digest::<7>(),
                DeployableRevisionContent::new(vec![], vec![], vec![], vec![])
                    .with_current_function_revisions(vec![]),
            ),
            opaque_standard_context(),
        )
        .unwrap();
        assert_eq!(
            deployable
                .catalogue_hash_context()
                .standard()
                .unwrap()
                .catalogue()
                .value_type_by_id(TypeId::from_bytes(id::<72>()))
                .unwrap()
                .kind(),
            ValueTypeKind::Opaque
        );
    }

    #[test]
    fn version_one_rejects_an_opaque_definition_before_slot_validation() {
        let opaque = TypeId::from_bytes(id::<72>());
        let catalogue = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes(id::<7>()),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes(id::<8>()),
                QualifiedSemanticName::new(["crm"]).unwrap(),
            )],
            vec![ObjectTypeDefinition::new(
                TypeId::from_bytes(id::<80>()),
                QualifiedSemanticName::new(["crm", "task"]).unwrap(),
                vec![FieldDefinition::new(
                    FieldId::from_bytes(id::<81>()),
                    "value",
                    0,
                    ResolvedType::value(TypeId::from_bytes(id::<99>())),
                    false,
                    false,
                    None,
                    None,
                )],
            )],
            vec![ValueTypeDefinition::opaque(
                opaque,
                QualifiedSemanticName::new(["crm", "token"]).unwrap(),
                "crm.token@1",
            )],
            vec![],
        )
        .unwrap();

        assert_eq!(
            validate_catalogue_hash_context_coherence(
                &CatalogueHashContext::version_one(),
                &catalogue,
                &[],
                &[],
                &[],
            ),
            Err(
                RevisionInvariantError::ValueTypeDefinitionRequiresCatalogueHashVersionTwo {
                    value_type: opaque,
                }
            )
        );
    }

    #[test]
    fn constructors_reject_version_two_scalars_in_each_durable_slot_order() {
        let pinned = ResolvedType::value(TypeId::from_bytes(id::<71>()));
        let scalar = ResolvedType::scalar(StandardScalar::Boolean);
        let cases = [
            (
                resolved_type_slots_catalogue(scalar, pinned, FunctionReturn::Single(pinned)),
                DefinitionIdentity::Field {
                    owner: TypeId::from_bytes(id::<80>()),
                    field: FieldId::from_bytes(id::<81>()),
                },
            ),
            (
                resolved_type_slots_catalogue(
                    ResolvedType::named(TypeId::from_bytes(id::<80>())),
                    scalar,
                    FunctionReturn::Single(pinned),
                ),
                DefinitionIdentity::Parameter {
                    owner: FunctionId::from_bytes(id::<82>()),
                    parameter: ParameterId::from_bytes(id::<83>()),
                },
            ),
            (
                resolved_type_slots_catalogue(
                    ResolvedType::named(TypeId::from_bytes(id::<80>())),
                    pinned,
                    FunctionReturn::Single(scalar),
                ),
                DefinitionIdentity::Function(FunctionId::from_bytes(id::<82>())),
            ),
            (
                resolved_type_slots_catalogue(
                    ResolvedType::named(TypeId::from_bytes(id::<80>())),
                    pinned,
                    FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
                        "result", 0, scalar,
                    )]),
                ),
                DefinitionIdentity::FunctionReturnColumn {
                    owner: FunctionId::from_bytes(id::<82>()),
                    ordinal: 0,
                },
            ),
        ];
        for (catalogue, identity) in cases {
            let active_source = source(None);
            let active_input = ActiveDatabaseRevisionInput::new(
                RevisionPair::new(active_source.id(), catalogue.revision()),
                active_source,
                catalogue.clone(),
                digest::<7>(),
                ActiveRevisionContent::new(vec![], vec![], vec![], vec![]),
            );
            let active_error = ActiveDatabaseRevision::new_with_catalogue_hash_context(
                active_input,
                standard_context(),
            )
            .unwrap_err();
            let expected = RevisionInvariantError::LegacyScalarRequiresCatalogueHashVersionOne {
                identity,
                scalar: StandardScalar::Boolean,
            };
            assert_eq!(active_error, expected);
            assert_eq!(active_error.to_string(), expected.to_string());
            assert!(Error::source(&active_error).is_none());

            let expected_pair = RevisionPair::new(
                SourceRevisionId::from_bytes(id::<20>()),
                CatalogueRevisionId::from_bytes(id::<21>()),
            );
            let deployable_input = DeployableRevisionInput::new(
                expected_pair,
                source(Some(expected_pair.source())),
                expected_pair.catalogue(),
                catalogue,
                digest::<7>(),
                DeployableRevisionContent::new(vec![], vec![], vec![], vec![])
                    .with_current_function_revisions(vec![resolved_slot_function_revision()]),
            );
            let deployable_error = DeployableRevision::new_with_catalogue_hash_context(
                deployable_input,
                standard_context(),
            )
            .unwrap_err();
            assert_eq!(deployable_error, expected);
            assert_eq!(deployable_error.to_string(), expected.to_string());
            assert!(Error::source(&deployable_error).is_none());
        }
    }

    #[test]
    fn constructors_reject_version_two_scalars_including_client_parameters() {
        let expected = RevisionPair::new(
            SourceRevisionId::from_bytes(id::<20>()),
            CatalogueRevisionId::from_bytes(id::<21>()),
        );
        let revision = function_revision_v2();
        let input = DeployableRevisionInput::new(
            expected,
            source(Some(expected.source())),
            expected.catalogue(),
            function_catalogue(revision.id()),
            digest::<7>(),
            DeployableRevisionContent::new(function_origins(&revision), vec![], vec![], vec![])
                .with_current_function_revisions(vec![revision]),
        );
        assert_eq!(
            DeployableRevision::new_with_catalogue_hash_context(input, standard_context())
                .unwrap_err(),
            RevisionInvariantError::LegacyScalarRequiresCatalogueHashVersionOne {
                identity: DefinitionIdentity::FunctionReturnColumn {
                    owner: FunctionId::from_bytes(id::<9>()),
                    ordinal: 0,
                },
                scalar: StandardScalar::Boolean,
            }
        );

        let hostile_client = resolved_type_slots_catalogue(
            ResolvedType::named(TypeId::from_bytes(id::<80>())),
            ResolvedType::scalar(StandardScalar::Integer),
            FunctionReturn::Single(ResolvedType::value(TypeId::from_bytes(id::<71>()))),
        );
        assert_eq!(
            validate_resolved_type_slots(&standard_context(), &hostile_client,),
            Err(
                RevisionInvariantError::LegacyScalarRequiresCatalogueHashVersionOne {
                    identity: DefinitionIdentity::Parameter {
                        owner: FunctionId::from_bytes(id::<82>()),
                        parameter: ParameterId::from_bytes(id::<83>()),
                    },
                    scalar: StandardScalar::Integer,
                },
            )
        );
    }

    #[test]
    fn resolved_value_revision_errors_have_exact_source_free_contracts() {
        let identity = DefinitionIdentity::Function(FunctionId::from_bytes(id::<82>()));
        let value_type = TypeId::from_bytes(id::<71>());
        let cases = [
            (
                RevisionInvariantError::ResolvedValueRequiresCatalogueHashVersionTwo {
                    identity,
                    value_type,
                },
                "resolved value type requires catalogue hash version 2",
            ),
            (
                RevisionInvariantError::LegacyScalarRequiresCatalogueHashVersionOne {
                    identity,
                    scalar: StandardScalar::Boolean,
                },
                "legacy scalar resolved type requires catalogue hash version 1",
            ),
            (
                RevisionInvariantError::ResolvedValueTypeNotInPinnedStandard {
                    identity,
                    value_type,
                },
                "resolved value type is absent from the pinned standard library",
            ),
            (
                RevisionInvariantError::OpaqueValueTypeNotAcceptedInSlot {
                    identity,
                    value_type,
                },
                "opaque value type is not accepted in a catalogue slot",
            ),
        ];

        for (error, display) in cases {
            assert_eq!(error.to_string(), display);
            assert!(Error::source(&error).is_none());
        }
    }

    #[test]
    fn catalogue_hash_context_errors_explain_the_required_version() {
        let function = FunctionId::from_bytes(id::<9>());
        let revision = FunctionRevisionId::from_bytes(id::<11>());
        let target = TypeId::from_bytes(id::<71>());
        let cases = [
            (
                RevisionInvariantError::ValueTypeDefinitionRequiresCatalogueHashVersionTwo {
                    value_type: target,
                },
                "value types require catalogue hash version 2",
            ),
            (
                RevisionInvariantError::TypeBindingRequiresCatalogueHashVersionTwo {
                    binding: TypeBindingId::from_bytes(id::<72>()),
                },
                "type-name bindings require catalogue hash version 2",
            ),
            (
                RevisionInvariantError::DefinitionOriginRequiresCatalogueHashVersionTwo {
                    identity: DefinitionIdentity::ValueType(target),
                },
                "value-type and type-binding origins require catalogue hash version 2",
            ),
            (
                RevisionInvariantError::ValueTypeReferenceRequiresCatalogueHashVersionTwo {
                    function,
                    revision,
                    target,
                },
                "value-type references require catalogue hash version 2",
            ),
            (
                RevisionInvariantError::FunctionSemanticHashVersionRequiresCatalogueHashVersionTwo {
                    function,
                    revision,
                },
                "function semantic hash version 2 requires catalogue hash version 2",
            ),
            (
                RevisionInvariantError::ValueTypeReferenceFunctionRevisionUnavailable {
                    function,
                    revision,
                    target,
                },
                "cannot verify a value-type reference without its function revision record",
            ),
            (
                RevisionInvariantError::ValueTypeReferenceRequiresFunctionSemanticHashVersionTwo {
                    function,
                    revision,
                    target,
                },
                "value-type references require function semantic hash version 2",
            ),
            (
                RevisionInvariantError::DeployableCurrentFunctionRevisionsRequired,
                "catalogue hash version 2 requires complete current function revision evidence",
            ),
            (
                RevisionInvariantError::MissingDeployableCurrentFunctionRevision {
                    function,
                    revision,
                },
                "current function revision evidence is incomplete for the candidate",
            ),
            (
                RevisionInvariantError::NewFunctionRevisionCurrentEvidenceMismatch {
                    function,
                    revision,
                },
                "a new function revision does not match the supplied current revision evidence",
            ),
        ];

        for (error, message) in cases {
            assert_eq!(error.to_string(), message);
        }
    }

    #[test]
    fn accepts_write_references_and_rejects_crossed_or_other_targets() {
        let expected = RevisionPair::new(
            SourceRevisionId::from_bytes(id::<20>()),
            CatalogueRevisionId::from_bytes(id::<21>()),
        );
        let revision = function_revision();
        let object_type = TypeId::from_bytes(id::<12>());
        let field = FieldId::from_bytes(id::<13>());
        let valid_references = vec![
            DefinitionReference::new(
                revision.function(),
                revision.id(),
                0,
                DefinitionReferenceTarget::ObjectType(object_type),
                DefinitionReferenceKind::WriteObject,
                revision.declaration_origin(),
            ),
            DefinitionReference::new(
                revision.function(),
                revision.id(),
                1,
                DefinitionReferenceTarget::Field {
                    owner: object_type,
                    field,
                },
                DefinitionReferenceKind::WriteField,
                revision.declaration_origin(),
            ),
        ];
        assert!(
            DeployableRevision::new(
                expected,
                source(Some(expected.source())),
                expected.catalogue(),
                write_catalogue(revision.id()),
                digest::<7>(),
                write_origins(&revision),
                vec![],
                vec![revision.clone()],
                valid_references,
            )
            .is_ok()
        );

        for (kind, target) in [
            (
                DefinitionReferenceKind::WriteObject,
                DefinitionReferenceTarget::Field {
                    owner: object_type,
                    field,
                },
            ),
            (
                DefinitionReferenceKind::WriteField,
                DefinitionReferenceTarget::ObjectType(object_type),
            ),
            (
                DefinitionReferenceKind::WriteObject,
                DefinitionReferenceTarget::Function(revision.function()),
            ),
            (
                DefinitionReferenceKind::WriteField,
                DefinitionReferenceTarget::Function(revision.function()),
            ),
        ] {
            let reference = DefinitionReference::new(
                revision.function(),
                revision.id(),
                0,
                target,
                kind,
                revision.declaration_origin(),
            );
            assert!(matches!(
                DeployableRevision::new(
                    expected,
                    source(Some(expected.source())),
                    expected.catalogue(),
                    write_catalogue(revision.id()),
                    digest::<7>(),
                    write_origins(&revision),
                    vec![],
                    vec![revision.clone()],
                    vec![reference],
                ),
                Err(RevisionInvariantError::ReferenceKindTargetMismatch { .. })
            ));
        }
    }
}
