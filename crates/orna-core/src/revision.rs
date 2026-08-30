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
    collections::{BTreeMap, HashMap, HashSet},
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
mod error;

pub use error::RevisionInvariantError;

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
            None,
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
            RecordValueFieldDescriptorClass::SealedSourceMetadata => None,
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
        Self::new_with_catalogue_hash_context_and_parent(input, catalogue_hash_context, None)
    }

    /// Creates a deployable revision while allowing references to definitions
    /// retained in the expected base catalogue. The parent is validation-only
    /// and is not part of the candidate catalogue or its hash.
    pub fn new_with_catalogue_hash_context_and_parent(
        input: DeployableRevisionInput,
        catalogue_hash_context: CatalogueHashContext,
        parent: Option<&CatalogueSnapshot>,
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
        if let Some(parent) = parent
            && parent.revision() != expected_base.catalogue()
        {
            return Err(RevisionInvariantError::DeployableCatalogueParentMismatch {
                expected: expected_base.catalogue(),
                actual: parent.revision(),
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
            parent,
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
            FunctionReturn::Single(resolved_type) | FunctionReturn::Stream(resolved_type) => {
                validate_resolved_type_slot(
                    context,
                    DefinitionIdentity::Function(function.id()),
                    *resolved_type,
                    function_accepts_opaque_client_return(function),
                )?
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
                if is_sealed_inspect_type_id(value_type)
                    || value_type == crate::system::SYS_SOURCE_FUNCTION_TYPE_ID
                {
                    return Ok(());
                }
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
    /// The sealed source-function metadata carrier identity.
    SealedSourceMetadata,
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
                    | RecordValueFieldDescriptorClass::StandardPrimitive(_)
                    | RecordValueFieldDescriptorClass::SealedSourceMetadata,
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
    if type_id == crate::system::SYS_SOURCE_FUNCTION_TYPE_ID {
        return Ok(RecordValueFieldDescriptorClass::SealedSourceMetadata);
    }
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
    parent: Option<&CatalogueSnapshot>,
    expressions: &[ExpressionArtifact],
    revisions: &[FunctionRevisionRecord],
    references: &[DefinitionReference],
) -> Result<(), RevisionInvariantError> {
    let revision_by_id = revisions
        .iter()
        .map(|revision| (revision.id, revision))
        .collect::<HashMap<_, _>>();
    let mut ordinals = HashSet::with_capacity(references.len());
    let mut ordinals_by_revision = BTreeMap::<FunctionRevisionId, Vec<u32>>::new();
    let expression_ids = expressions
        .iter()
        .map(ExpressionArtifact::id)
        .collect::<HashSet<_>>();

    for reference in references {
        validate_source_origin(source, reference.source_origin)?;
        if !reference_target_exists(
            catalogue,
            standard,
            parent,
            &expression_ids,
            reference.target,
        ) {
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
        ordinals_by_revision
            .entry(reference.source_revision)
            .or_default()
            .push(reference.ordinal);
    }
    for (revision, mut ordinals) in ordinals_by_revision {
        ordinals.sort_unstable();
        for (index, actual) in ordinals.into_iter().enumerate() {
            let expected = u32::try_from(index)
                .map_err(|_| RevisionInvariantError::ReferenceOrdinalOutOfRange { revision })?;
            if actual != expected {
                return Err(RevisionInvariantError::ReferenceOrdinalOutOfSequence {
                    revision,
                    expected,
                    actual,
                });
            }
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
    validate_references(source, catalogue, None, None, &[], &revisions, &references)
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
                FunctionReturn::Single(_) | FunctionReturn::Stream(_) => None,
                FunctionReturn::Rows(columns) => columns.get(ordinal as usize),
            })
            .is_some(),
        DefinitionIdentity::Expression(id) => expression_ids.contains(&id),
    }
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
    expression_ids: &HashSet<ExpressionId>,
    target: DefinitionReferenceTarget,
) -> bool {
    if matches!(
        target,
        DefinitionReferenceTarget::ValueType(type_id)
            | DefinitionReferenceTarget::ObjectType(type_id)
            if is_sealed_inspect_type_id(type_id)
                || type_id == crate::system::SYS_SOURCE_FUNCTION_TYPE_ID
    ) {
        return true;
    }
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
            })
            || parent.is_some_and(|parent| {
                parent
                    .object_type_by_id(owner)
                    .and_then(|object_type| object_type.field_by_id(field))
                    .is_some()
                    || parent
                        .record_value_type_by_id(owner)
                        .and_then(|record_value_type| record_value_type.field_by_id(field))
                        .is_some()
            });
    }
    let identity = target.into();
    definition_exists(catalogue, expression_ids, identity)
        || standard.is_some_and(|standard| definition_exists(standard, expression_ids, identity))
        || parent.is_some_and(|parent| definition_exists(parent, expression_ids, identity))
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

#[cfg(test)]
mod tests;
