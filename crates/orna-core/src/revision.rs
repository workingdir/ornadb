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
};

use crate::{
    CatalogueRevisionId, ExpressionId, FieldId, FunctionId, FunctionRevisionId, ParameterId,
    SchemaId, SourceBundleId, SourceRevisionId, SourceUnitId, TypeId,
    catalogue::{CatalogueSnapshot, FunctionDomain, FunctionReturn},
};

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

/// The identity of a catalogue member that owns a source origin.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DefinitionIdentity {
    /// A declared logical schema.
    Schema(SchemaId),
    /// A durable object type.
    ObjectType(TypeId),
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
pub enum DefinitionReferenceTarget {
    /// A durable object type.
    ObjectType(TypeId),
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
            language_version,
            artifact,
        })
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
pub struct ActiveDatabaseRevision {
    pair: RevisionPair,
    source: StoredSourceRevision,
    catalogue: CatalogueSnapshot,
    catalogue_hash: Sha256Digest,
    expressions: Vec<ExpressionArtifact>,
    function_revisions: Vec<FunctionRevisionRecord>,
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
        validate_pair(&pair, &source, &catalogue)?;
        validate_expressions(&expressions)?;
        validate_origins(&source, &catalogue, &expressions, &origins)?;
        validate_function_revisions(
            &source,
            &catalogue,
            &origins,
            &function_revisions,
            FunctionRevisionSet::RecoveredActive,
        )?;
        validate_references(
            &source,
            &catalogue,
            &expressions,
            &function_revisions,
            &references,
        )?;

        Ok(Self {
            pair,
            source,
            catalogue,
            catalogue_hash,
            expressions,
            function_revisions,
            origins,
            references,
        })
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

    /// Returns expression artifacts by durable record order.
    pub fn expressions(&self) -> &[ExpressionArtifact] {
        &self.expressions
    }

    /// Returns active function revisions by durable record order.
    pub fn function_revisions(&self) -> &[FunctionRevisionRecord] {
        &self.function_revisions
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

/// A compiler-produced candidate that is ready for a kernel apply attempt.
#[derive(Clone, Debug)]
pub struct DeployableRevision {
    expected_base: RevisionPair,
    source: StoredSourceRevision,
    parent_catalogue: CatalogueRevisionId,
    candidate: CatalogueSnapshot,
    catalogue_hash: Sha256Digest,
    origins: Vec<DefinitionOrigin>,
    expressions: Vec<ExpressionArtifact>,
    new_function_revisions: Vec<FunctionRevisionRecord>,
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
            &expressions,
            &new_function_revisions,
            &references,
        )?;

        Ok(Self {
            expected_base,
            source,
            parent_catalogue,
            candidate,
            catalogue_hash,
            origins,
            expressions,
            new_function_revisions,
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

    if set == FunctionRevisionSet::RecoveredActive {
        for function in catalogue.functions() {
            if !function_ids.contains(&function.id()) {
                return Err(RevisionInvariantError::MissingActiveFunctionRevision {
                    function: function.id(),
                    revision: function.current_revision(),
                });
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum FunctionRevisionSet {
    RecoveredActive,
    NewCandidate,
}

fn validate_references(
    source: &StoredSourceRevision,
    catalogue: &CatalogueSnapshot,
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
        if !reference_target_exists(catalogue, &expression_ids, reference.target) {
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
    expression_ids: &HashSet<ExpressionId>,
    target: DefinitionReferenceTarget,
) -> bool {
    definition_exists(catalogue, expression_ids, target.into())
}

const fn reference_kind_accepts_target(
    kind: DefinitionReferenceKind,
    target: DefinitionReferenceTarget,
) -> bool {
    matches!(
        (kind, target),
        (
            DefinitionReferenceKind::FunctionCall,
            DefinitionReferenceTarget::Function(_)
        ) | (
            DefinitionReferenceKind::NamedType
                | DefinitionReferenceKind::ObjectReference
                | DefinitionReferenceKind::QueryObject,
            DefinitionReferenceTarget::ObjectType(_)
        ) | (
            DefinitionReferenceKind::ParameterRead,
            DefinitionReferenceTarget::Parameter { .. }
        ) | (
            DefinitionReferenceKind::QueryField,
            DefinitionReferenceTarget::Field { .. }
        ) | (
            DefinitionReferenceKind::Expression,
            DefinitionReferenceTarget::Expression(_)
        )
    )
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
            EmptyArtifactFormat => formatter.write_str("artifact format is empty"),
            ZeroArtifactVersion { .. } => formatter.write_str("artifact format version is zero"),
            EmptyArtifactPayload { .. } => formatter.write_str("artifact payload is empty"),
            ZeroFunctionRevisionNumber { .. } => {
                formatter.write_str("function revision number is zero")
            }
            EmptyLanguageVersion { .. } => {
                formatter.write_str("function language version is empty")
            }
            SourceRevisionPairMismatch { .. } => {
                formatter.write_str("revision pair source does not match stored source")
            }
            CatalogueRevisionPairMismatch { .. } => {
                formatter.write_str("revision pair catalogue does not match catalogue snapshot")
            }
            DeployableSourceParentMismatch { .. } => {
                formatter.write_str("deployable source parent does not match expected base")
            }
            DeployableCatalogueParentMismatch { .. } => {
                formatter.write_str("deployable catalogue parent does not match expected base")
            }
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
        catalogue::{
            FunctionDefinition, FunctionDomain, FunctionReturn, FunctionReturnColumnDefinition,
            FunctionSecurity, FunctionVolatility, QualifiedSemanticName, SchemaDefinition,
        },
        types::{ResolvedType, StandardScalar},
    };

    const fn id<const BYTE: u8>() -> [u8; 16] {
        [BYTE; 16]
    }

    const fn digest<const BYTE: u8>() -> Sha256Digest {
        Sha256Digest::from_bytes([BYTE; 32])
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

    fn function_catalogue(function_revision: FunctionRevisionId) -> CatalogueSnapshot {
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
            vec![],
            vec![function],
        )
        .unwrap()
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
        FunctionRevisionRecord::new(
            FunctionId::from_bytes(id::<9>()),
            FunctionRevisionId::from_bytes(id::<11>()),
            1,
            SourceOrigin::new(SourceUnitId::from_bytes(id::<4>()), 10, 29).unwrap(),
            digest::<11>(),
            digest::<12>(),
            "orna-1",
            artifact(),
        )
        .unwrap()
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
        assert_eq!(active.pair(), pair);
        assert_eq!(active.catalogue_hash(), digest::<7>());
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
    }
}
