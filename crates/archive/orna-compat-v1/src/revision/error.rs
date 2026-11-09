use super::*;

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
    /// A source revision contains two units defining the same module namespace.
    DuplicateModuleNamespace {
        namespace: String,
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
    /// A source function revision has a missing or out-of-sequence reference ordinal.
    ReferenceOrdinalOutOfSequence {
        revision: FunctionRevisionId,
        expected: u32,
        actual: u32,
    },
    /// A source function revision has more references than durable ordinals can represent.
    ReferenceOrdinalOutOfRange { revision: FunctionRevisionId },
}

impl fmt::Display for RevisionInvariantError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self::DuplicateModuleNamespace {
            namespace,
            first,
            duplicate,
        } = self
        else {
            return formatter.write_str(self.message());
        };
        write!(
            formatter,
            "stored source units {first:?} and {duplicate:?} define module namespace {namespace:?}"
        )
    }
}

impl RevisionInvariantError {
    /// The fixed diagnostic of this error.
    ///
    /// Every variant names itself in one sentence. A module-namespace collision
    /// also names its two stored units, and `Display` formats those identities
    /// from the variant; this is the sentence that names the error itself.
    const fn message(&self) -> &'static str {
        use RevisionInvariantError::*;
        match self {
            DuplicateModuleNamespace { .. } => "duplicate source unit module namespace",
            SourceOriginReversed { .. } => "source origin end precedes start",
            EmptyLogicalPath { .. } => "stored source unit has an empty logical path",
            SourceOrdinalOutOfSequence { .. } => "stored source ordinals are not contiguous",
            SourceOrdinalOutOfRange { .. } => "stored source ordinal exceeds u32",
            DuplicateSourceUnitId { .. } => "duplicate stored source unit identity",
            DuplicateLogicalPath { .. } => "duplicate stored source logical path",
            SourceRevisionSelfParent { .. } => "source revision is its own parent",
            StandardLibrarySourceHasParent { .. } => {
                "standard library source revision has a parent"
            }
            EmptyStandardLibraryLanguageVersion { .. } => {
                "standard library language version is empty"
            }
            UnsupportedStandardLibraryDefinition { .. } => {
                "standard library catalogue contains an unsupported definition"
            }
            VersionOneStandardLibraryHasExecutable { .. } => {
                "standard library digest version 1 has executable evidence"
            }
            VersionTwoStandardLibrarySourceHasNoParent { .. } => {
                "standard library digest version 2 source has no parent"
            }
            StandardExecutableFunctionMismatch { .. } => {
                "standard executable function differs from its revision"
            }
            StandardExecutableSequenceLengthMismatch { .. } => {
                "standard executable sequence does not cover catalogue functions"
            }
            StandardExecutableSequenceFunctionMismatch { .. } => {
                "standard executable sequence function differs from catalogue"
            }
            StandardExecutableCatalogueFunctionOrder { .. } => {
                "standard executable catalogue functions are not in canonical order"
            }
            StandardExecutableSemanticHashVersionMismatch { .. } => {
                "standard executable requires function semantic hash version 2"
            }
            StandardExecutableReferenceOrdinalOutOfRange { .. } => {
                "standard executable reference position exceeds durable ordinal"
            }
            StandardExecutableReferenceOrdinalOutOfSequence { .. } => {
                "standard executable reference ordinals are not contiguous"
            }
            StandardExecutableReferenceOwnerMismatch { .. } => {
                "standard executable reference differs from its immutable owner"
            }
            EmptyArtifactFormat => "artifact format is empty",
            ZeroArtifactVersion { .. } => "artifact format version is zero",
            EmptyArtifactPayload { .. } => "artifact payload is empty",
            ZeroFunctionRevisionNumber { .. } => "function revision number is zero",
            EmptyLanguageVersion { .. } => "function language version is empty",
            ResolvedValueRequiresCatalogueHashVersionTwo { .. } => {
                "resolved value type requires catalogue hash version 2"
            }
            LegacyScalarRequiresCatalogueHashVersionOne { .. } => {
                "legacy scalar resolved type requires catalogue hash version 1"
            }
            ResolvedValueTypeNotInPinnedStandard { .. } => {
                "resolved value type is absent from the pinned standard library"
            }
            OpaqueValueTypeNotAcceptedInSlot { .. } => {
                "opaque value type is not accepted in a catalogue slot"
            }
            FunctionSemanticHashVersionRequiresCatalogueHashVersionTwo { .. } => {
                "function semantic hash version 2 requires catalogue hash version 2"
            }
            ValueTypeDefinitionRequiresCatalogueHashVersionTwo { .. } => {
                "value types require catalogue hash version 2"
            }
            RecordValueTypeRequiresCatalogueHashVersionTwo { .. } => {
                "record value types require catalogue hash version 2"
            }
            UnsupportedRecordValueFieldType { .. } => {
                "record value field has an unsupported resolved type"
            }
            AmbiguousRecordValueFieldType { .. } => {
                "record field type is present in both application and standard catalogues"
            }
            RecursiveRecordValueField { .. } => {
                "record value fields must not form a recursive cycle"
            }
            RecordValueNestingTooDeep { .. } => "record value nesting exceeds the maximum depth",
            TypeBindingRequiresCatalogueHashVersionTwo { .. } => {
                "type-name bindings require catalogue hash version 2"
            }
            DefinitionOriginRequiresCatalogueHashVersionTwo { .. } => {
                "value-type and type-binding origins require catalogue hash version 2"
            }
            ValueTypeReferenceRequiresCatalogueHashVersionTwo { .. } => {
                "value-type references require catalogue hash version 2"
            }
            ValueTypeReferenceFunctionRevisionUnavailable { .. } => {
                "cannot verify a value-type reference without its function revision record"
            }
            ValueTypeReferenceRequiresFunctionSemanticHashVersionTwo { .. } => {
                "value-type references require function semantic hash version 2"
            }
            SourceRevisionPairMismatch { .. } => {
                "revision pair source does not match stored source"
            }
            CatalogueRevisionPairMismatch { .. } => {
                "revision pair catalogue does not match catalogue snapshot"
            }
            ReservedOfflineCheckCatalogueRevision { .. } => {
                "the reserved offline-check catalogue identity cannot be used in a durable revision"
            }
            ReservedSystemFunctionIdentity { .. } => {
                "the reserved system function identity cannot enter an application catalogue"
            }
            ReservedSystemFunctionName { .. } => {
                "the reserved system function name cannot enter an application catalogue"
            }
            ReservedInvocationCarrierIdentity { .. } => {
                "the reserved invocation carrier identity cannot enter a catalogue"
            }
            ReservedInvocationCarrierName { .. } => {
                "the reserved invocation carrier name cannot enter a catalogue"
            }
            DeployableSourceParentMismatch { .. } => {
                "deployable source parent does not match expected base"
            }
            DeployableCatalogueParentMismatch { .. } => {
                "deployable catalogue parent does not match expected base"
            }
            DeployableCurrentFunctionRevisionsRequired => {
                "catalogue hash version 2 requires complete current function revision evidence"
            }
            MissingDeployableCurrentFunctionRevision { .. } => {
                "current function revision evidence is incomplete for the candidate"
            }
            NewFunctionRevisionCurrentEvidenceMismatch { .. } => {
                "a new function revision does not match the supplied current revision evidence"
            }
            CatalogueRevisionSelfParent { .. } => "candidate catalogue is its own parent",
            DuplicateExpressionId { .. } => "duplicate expression identity",
            SourceOriginUnitNotInRevision { .. } => {
                "source origin unit is absent from stored revision"
            }
            SourceContentTooLarge { .. } => "source content exceeds u32 byte offsets",
            SourceOriginOutOfBounds { .. } => "source origin is outside stored source content",
            SourceOriginNotCharacterBoundary { .. } => "source origin splits a UTF-8 character",
            DuplicateDefinitionOrigin { .. } => "duplicate definition origin",
            OriginDefinitionNotInRevision { .. } => "origin definition is absent from revision",
            MissingDefinitionOrigin { .. } => "revision definition has no source origin",
            DuplicateFunctionRevisionId { .. } => "duplicate function revision identity",
            DuplicateFunctionRevisionNumber { .. } => "duplicate function revision number",
            DuplicateFunctionRevisionHashPair { .. } => {
                "duplicate function revision declaration and semantic hash pair"
            }
            DuplicateFunctionRevisionFunction { .. } => {
                "duplicate supplied function revision function"
            }
            FunctionRevisionFunctionNotInCatalogue { .. } => {
                "function revision function is absent from catalogue"
            }
            FunctionRevisionNotCurrent { .. } => {
                "function revision is not catalogue current revision"
            }
            FunctionRevisionArtifactDomainMismatch { .. } => {
                "function artifact domain differs from catalogue domain"
            }
            FunctionRevisionOriginMismatch { .. } => {
                "function revision declaration differs from definition origin"
            }
            MissingActiveFunctionRevision { .. } => {
                "active catalogue function has no revision record"
            }
            ReferenceFunctionNotInCatalogue { .. } => "reference function is absent from catalogue",
            ReferenceRevisionNotCurrent { .. } => {
                "reference revision is not catalogue current revision"
            }
            ReferenceFunctionRevisionMismatch { .. } => "reference function and revision differ",
            ReferenceTargetNotInRevision { .. } => "reference target is absent from revision",
            ReferenceKindTargetMismatch { .. } => {
                "reference kind cannot target that definition kind"
            }
            DuplicateReferenceOrdinal { .. } => "duplicate reference ordinal",
            ReferenceOrdinalOutOfSequence { .. } => "reference ordinals are not contiguous",
            ReferenceOrdinalOutOfRange { .. } => "reference ordinal exceeds durable ordinal",
        }
    }
}

impl Error for RevisionInvariantError {}
