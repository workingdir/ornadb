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
    catalogue::{CatalogueSnapshot, FunctionDomain, FunctionReturn},
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
}

impl StandardLibraryDigestVersion {
    /// Returns the exact positive durable numeric value.
    pub const fn to_u32(self) -> u32 {
        match self {
            Self::Version1 => 1,
        }
    }
}

impl TryFrom<u32> for StandardLibraryDigestVersion {
    type Error = HashVersionError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Version1),
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
        reject_offline_check_catalogue_revision(
            catalogue.revision(),
            DurableCatalogueRevisionRole::ActiveOrRecoveredStandard,
        )?;
        if source.parent().is_some() {
            return Err(RevisionInvariantError::StandardLibrarySourceHasParent {
                source: source.id(),
                parent: source.parent(),
            });
        }
        let language_version = language_version.into();
        if language_version.is_empty() {
            return Err(RevisionInvariantError::EmptyStandardLibraryLanguageVersion { revision });
        }
        if !catalogue.object_types().is_empty() || !catalogue.functions().is_empty() {
            return Err(RevisionInvariantError::UnsupportedStandardLibraryDefinition { revision });
        }
        validate_origins(&source, &catalogue, &[], &origins)?;

        Ok(Self {
            inner: Arc::new(StandardLibrarySnapshotData {
                revision,
                digest_version,
                source,
                language_version,
                catalogue,
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
    /// A field owned by an object type.
    Field {
        /// The stable object-type identity.
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
    /// A stable field owned by an object type.
    Field {
        /// The stable object-type identity.
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
    /// A mutation writes one owner-qualified object field.
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

fn validate_catalogue_hash_context_coherence(
    context: &CatalogueHashContext,
    catalogue: &CatalogueSnapshot,
    revisions: &[FunctionRevisionRecord],
    origins: &[DefinitionOrigin],
    references: &[DefinitionReference],
) -> Result<(), RevisionInvariantError> {
    match context {
        CatalogueHashContext::Version1 => {
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
        }
        CatalogueHashContext::Version2 { .. } => {
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
        DefinitionIdentity::ValueType(id) => catalogue.value_type_by_id(id).is_some(),
        DefinitionIdentity::TypeBinding(id) => catalogue.type_binding_by_id(id).is_some(),
        DefinitionIdentity::Field { owner, field } => catalogue
            .object_type_by_id(owner)
            .and_then(|object_type| object_type.field_by_id(field))
            .is_some(),
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
            EmptyArtifactFormat => formatter.write_str("artifact format is empty"),
            ZeroArtifactVersion { .. } => formatter.write_str("artifact format version is zero"),
            EmptyArtifactPayload { .. } => formatter.write_str("artifact payload is empty"),
            ZeroFunctionRevisionNumber { .. } => {
                formatter.write_str("function revision number is zero")
            }
            EmptyLanguageVersion { .. } => {
                formatter.write_str("function language version is empty")
            }
            FunctionSemanticHashVersionRequiresCatalogueHashVersionTwo { .. } => formatter
                .write_str("function semantic hash version 2 requires catalogue hash version 2"),
            ValueTypeDefinitionRequiresCatalogueHashVersionTwo { .. } => {
                formatter.write_str("value types require catalogue hash version 2")
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
            FieldDefinition, FunctionDefinition, FunctionDomain, FunctionReturn,
            FunctionReturnColumnDefinition, FunctionSecurity, FunctionVolatility,
            ObjectTypeDefinition, QualifiedSemanticName, SchemaDefinition, TypeBinding,
            ValueTypeDefinition, ValueTypeMutability, ValueTypePersistence,
        },
        types::{ResolvedType, StandardScalar},
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
                origins: vec![],
                digest: digest::<75>(),
            }),
        })
    }

    fn function_catalogue(function_revision: FunctionRevisionId) -> CatalogueSnapshot {
        function_catalogue_with_objects(function_revision, vec![])
    }

    fn function_catalogue_with_objects(
        function_revision: FunctionRevisionId,
        object_types: Vec<ObjectTypeDefinition>,
    ) -> CatalogueSnapshot {
        let schema = SchemaDefinition::new(
            SchemaId::from_bytes(id::<8>()),
            QualifiedSemanticName::new(["crm"]).unwrap(),
        );
        let function = FunctionDefinition::new(
            FunctionId::from_bytes(id::<9>()),
            QualifiedSemanticName::new(["crm", "lookup"]).unwrap(),
            FunctionDomain::Server,
            vec![],
            FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
                "found",
                0,
                ResolvedType::scalar(StandardScalar::Boolean),
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
                    ResolvedType::scalar(StandardScalar::Boolean),
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
        let value_type = ValueTypeDefinition::primitive(
            TypeId::from_bytes(id::<71>()),
            QualifiedSemanticName::new(["std", "boolean"]).unwrap(),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Persistable,
            "orna.kernel.value.boolean@1",
        );
        let catalogue = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes(id::<72>()),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes(id::<73>()),
                QualifiedSemanticName::new(["std"]).unwrap(),
            )],
            vec![],
            vec![value_type],
            vec![],
        )
        .unwrap();
        let origins = vec![
            DefinitionOrigin::new(
                DefinitionIdentity::Schema(SchemaId::from_bytes(id::<73>())),
                SourceOrigin::new(SourceUnitId::from_bytes(id::<3>()), 0, 1).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::ValueType(TypeId::from_bytes(id::<71>())),
                SourceOrigin::new(SourceUnitId::from_bytes(id::<3>()), 1, 2).unwrap(),
            ),
        ];
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
        let catalogue = function_catalogue(revision.id());
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
                function_catalogue(revision.id()),
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
            function_catalogue(revision.id()),
            digest::<7>(),
            content,
        );

        let deployable =
            DeployableRevision::new_with_catalogue_hash_context(input, standard_context()).unwrap();

        assert!(deployable.new_function_revisions().is_empty());
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
