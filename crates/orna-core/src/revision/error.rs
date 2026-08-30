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
            ReferenceOrdinalOutOfSequence { .. } => {
                formatter.write_str("reference ordinals are not contiguous")
            }
            ReferenceOrdinalOutOfRange { .. } => {
                formatter.write_str("reference ordinal exceeds durable ordinal")
            }
        }
    }
}

impl Error for RevisionInvariantError {}
