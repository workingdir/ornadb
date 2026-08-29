//! Construction of complete durable revisions from successful compiler checks.

mod candidate_builder;
mod server_plans;

use candidate_builder::next_function_revision_number;
#[cfg(test)]
use candidate_builder::{
    client_capability_requirement, durable_client_local_id, standard_upgrade_reuse_is_current_only,
    supports_durable_unique_field,
};

#[cfg(test)]
use server_plans::{
    delete_reference_sequence, distinct_query_reference_sequence,
    identity_selected_query_reference_sequence, server_mutation_expression,
    signature_reference_sequence, validate_mutation_assignments, validate_mutation_parameters,
    validate_mutation_selector, validate_reference_sequence, version_one_query_reference_sequence,
};
use server_plans::{
    distinct_query_plan, identity_selected_query_plan, is_sealed_inspect_type_id,
    query_planning_function, server_delete_plan, server_mutation_plan,
    unique_text_selected_query_plan, version_one_query_plan,
};

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    error::Error,
    fmt,
};

use orna_artifact::{
    client_plan::{
        ACTION_FORMAT_VERSION as CLIENT_PLAN_ACTION_VERSION, ActionClientPlan, ActionOperationNode,
        CAPABILITY_FORMAT_VERSION as CLIENT_PLAN_CAPABILITY_VERSION,
        CONTROL_FLOW_FORMAT_VERSION as CLIENT_PLAN_CONTROL_FLOW_VERSION, CapabilityArgumentSource,
        CapabilityClientPlan, CapabilityRequirement, ClientExpressionNode, ClientLocal,
        ClientLocalKind, ClientPlan, ClientStatement, ControlFlowClientPlan, ControlFlowIfBranch,
        ControlFlowIfStatement, ControlFlowStatement, ControlFlowWhileStatement,
        EXPRESSION_FORMAT_VERSION as CLIENT_PLAN_EXPRESSION_VERSION, ExpressionClientPlan,
        FORMAT_IDENTITY as CLIENT_PLAN_FORMAT, FORMAT_VERSION as CLIENT_PLAN_VERSION,
        InnerClientPlan, InspectOperationNode, InspectProjection,
        LANGUAGE_VERSION_IDENTITY as CLIENT_PLAN_LANGUAGE_VERSION,
        PROCEDURAL_FORMAT_VERSION as CLIENT_PLAN_PROCEDURAL_VERSION, ProceduralClientPlan,
        RESOURCE_FORMAT_VERSION as CLIENT_PLAN_RESOURCE_VERSION, ResourceClientPlan,
        ResourceOperationNode, STATE_FORMAT_VERSION as CLIENT_PLAN_STATE_VERSION, StateClientPlan,
        StateDefault, StateScope, StateSlot,
    },
    constant_expression::{
        ConstantExpression, ConstantExpressionError, FORMAT_IDENTITY as CONSTANT_FORMAT,
        FORMAT_VERSION as CONSTANT_VERSION,
    },
    server_mutation_plan::{
        FORMAT_IDENTITY as SERVER_MUTATION_PLAN_FORMAT,
        FieldAssignment as ServerMutationFieldAssignment,
        LANGUAGE_VERSION_IDENTITY as SERVER_MUTATION_PLAN_LANGUAGE_VERSION,
        MutationExpression as ServerMutationExpression,
        MutationExpressionKind as ServerMutationExpressionKind, MutationSelector,
        RecordFieldExpression as ServerRecordFieldExpression,
        RecordFieldExpressionKind as ServerRecordFieldExpressionKind, ServerDeletePlan,
        ServerMutationPlan, ServerMutationPlanError,
    },
    server_plan::{
        FORMAT_IDENTITY as SERVER_PLAN_FORMAT, FORMAT_VERSION as SERVER_PLAN_VERSION,
        LANGUAGE_VERSION_IDENTITY as SERVER_PLAN_LANGUAGE_VERSION, ServerPlanError,
    },
};
use orna_core::{
    CatalogueRevisionId, ExpressionId, FieldId, FunctionId, FunctionRevisionId, LocalId,
    ParameterId, SchemaId, SourceBundleId, SourceRevisionId, SourceUnitId,
    StandardLibraryRevisionId, TypeBindingId, TypeId,
    canonical_hash::{
        CanonicalHashError, artifact_payload_digest, catalogue_digest_with_context,
        catalogue_digest_with_context_and_parent, function_declaration_digest,
        function_semantic_digest, function_semantic_digest_with_version, source_bundle_digest,
        source_revision_record_digest, source_unit_content_digest,
    },
    catalogue::{
        CatalogueSnapshot, CatalogueSnapshotError, EnumTypeDefinition, FieldDefinition,
        FunctionDefinition, FunctionDomain, FunctionReturn, FunctionReturnColumnDefinition,
        FunctionSecurity, FunctionTransaction, FunctionVolatility, ObjectTypeDefinition,
        ParameterDefinition, QualifiedSemanticName, RecordValueFieldDefinition,
        RecordValueTypeDefinition, SchemaDefinition, ValueTypeDefinition, ValueTypeKind,
        ValueTypeMutability, ValueTypePersistence,
    },
    revision::{
        ActiveDatabaseRevision, CatalogueHashContext, DefinitionIdentity, DefinitionOrigin,
        DefinitionReference, DefinitionReferenceKind, DefinitionReferenceTarget,
        DeployableRevision, DeployableRevisionContent, DeployableRevisionInput, ExecutableArtifact,
        ExecutableArtifactKind, ExpressionArtifact, FunctionRevisionRecord,
        FunctionSemanticHashVersion, RevisionInvariantError, RevisionPair, Sha256Digest,
        SourceOrigin, StoredSourceRevision, StoredSourceUnit, VerifiedStandardLibrarySnapshot,
    },
    source::{SourceBundle, SourceUnit},
    system::INVOCATION_CARRIERS,
    types::{ResolvedType, StandardScalar, TypeDescriptor, TypeDescriptorKind},
};

use crate::{
    CheckReport, CheckedBundle, CheckedDefinitionReferenceTarget, CheckedExpressionId,
    CheckedFieldId, CheckedFunctionId, CheckedParameterId, CheckedSchemaId, CheckedTypeId,
    CompilerDiagnostic, ConstantValue, ParseReport, STD_BOOLEAN_TYPE_ID,
    STD_CHARACTER_LARGE_OBJECT_TYPE_ID, STD_INTEGER_TYPE_ID, SemanticType, SourceLocation,
    StandardApplicationCheckContext, StandardApplicationCheckReport,
    StandardApplicationContextError, check_standard_application,
};
use crate::{
    mutation::{
        DeletePlanIr, MutationExpressionKind, MutationOperation, MutationPlanIr,
        MutationRecordFieldExpressionKind,
    },
    relational::{supports_server_select_distinct, supports_server_select_equality},
    resolver::{
        CheckedActionOperation, CheckedClientControlFlowStatement, CheckedClientExpression,
        CheckedClientFunctionBody, CheckedClientLocal, CheckedClientLocalKind,
        CheckedClientReturnShape, CheckedClientStateSlot, CheckedClientStatement,
        CheckedFieldRename, CheckedInspectOperation, CheckedInspectProjection,
        CheckedResourceOperation, CheckedStateDefault, CheckedStateScope, UNIQUE_FIELD_MESSAGE,
        durable_state_slot_id, supports_unique_text_or_required_reference,
    },
};
mod standard_upgrade;

#[cfg(test)]
use standard_upgrade::admits_append_only_standard_child;
pub use standard_upgrade::{
    PrepareStandardUpgradeError, PreparedStandardUpgrade, StandardUpgradeIdentity,
    prepare_checked_standard_upgrade,
};
pub(crate) use standard_upgrade::{
    active_reserved_standard_identity, prepare_checked_standard_upgrade_with_allocator,
};

/// One encoded CLIENT or SERVER artifact with the language version that defines it.
#[derive(Clone)]
struct PreparedFunctionArtifact {
    artifact: ExecutableArtifact,
    language_version: String,
}

struct FunctionFinalisation<'a> {
    checked: CheckedFunctionId,
    location: &'a SourceLocation,
    function: FunctionId,
    initial_revision: FunctionRevisionId,
    definition: &'a FunctionDefinition,
    prepared_artifact: PreparedFunctionArtifact,
    references: &'a [DefinitionReference],
}

#[derive(Clone)]
struct FunctionRevisionPlan {
    definition: FunctionDefinition,
    semantic_hash_version: FunctionSemanticHashVersion,
    semantic_hash: Sha256Digest,
    language_version: String,
    artifact: ExecutableArtifact,
    references: Vec<DefinitionReference>,
    reusable: Option<FunctionRevisionRecord>,
    next_revision_number: Option<u64>,
}

struct FunctionRevisionPlanInput<'a> {
    semantic_hash_version: FunctionSemanticHashVersion,
    definition: &'a FunctionDefinition,
    language_version: &'a str,
    artifact: &'a ExecutableArtifact,
    expressions: &'a [ExpressionArtifact],
    references: &'a [DefinitionReference],
    current_only: bool,
    reuse_policy: FunctionRevisionReusePolicy,
}

#[derive(Clone, Copy)]
enum FunctionRevisionReusePolicy {
    SemanticHashOnly,
    Complete,
}

impl FunctionRevisionPlan {
    fn new(
        active: &ActiveDatabaseRevision,
        function: FunctionId,
        input: FunctionRevisionPlanInput<'_>,
    ) -> Result<Self, PrepareError> {
        let FunctionRevisionPlanInput {
            semantic_hash_version,
            definition,
            language_version,
            artifact,
            expressions,
            references,
            current_only,
            reuse_policy,
        } = input;
        let semantic_hash = match semantic_hash_version {
            FunctionSemanticHashVersion::Version1 => function_semantic_digest(
                definition,
                language_version,
                artifact,
                expressions,
                references,
            )?,
            FunctionSemanticHashVersion::Version2 => function_semantic_digest_with_version(
                FunctionSemanticHashVersion::Version2,
                definition,
                language_version,
                artifact,
                expressions,
                references,
            )?,
            _ => {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "checked standard application has an unsupported semantic hash version",
                });
            }
        };
        let current = active.function_revisions().iter();
        let revisions: Box<dyn Iterator<Item = &FunctionRevisionRecord>> = if current_only {
            Box::new(current)
        } else {
            Box::new(current.chain(active.historical_function_revisions()))
        };
        let reusable = revisions
            .filter(|revision| {
                revision.function() == function
                    && revision.semantic_hash() == semantic_hash
                    && revision.semantic_hash_version() == semantic_hash_version
                    && (!matches!(reuse_policy, FunctionRevisionReusePolicy::Complete)
                        || (revision.language_version() == language_version
                            && revision.artifact() == artifact))
            })
            .min_by_key(|revision| (revision.revision_number(), revision.id().to_bytes()))
            .cloned();
        let next_revision_number = if reusable.is_some() {
            None
        } else {
            Some(next_function_revision_number(active, function)?)
        };
        Ok(Self {
            definition: definition.clone(),
            semantic_hash_version,
            semantic_hash,
            language_version: language_version.to_owned(),
            artifact: artifact.clone(),
            references: references.to_vec(),
            reusable,
            next_revision_number,
        })
    }
}

/// The complete Gate-seven lowering result for one standard upgrade.
///
/// This private value owns every semantic fact used by later gates. No checked
/// source, parse report, resolver, or candidate builder crosses this seam.
struct StandardUpgradeLoweringPlan {
    source_template: StoredSourceRevision,
    schemas: Vec<SchemaDefinition>,
    object_types: Vec<ObjectTypeDefinition>,
    expressions: Vec<ExpressionArtifact>,
    origin_templates: Vec<DefinitionOrigin>,
    functions: Vec<StandardUpgradeFunctionPlan>,
}

struct StandardUpgradeFunctionPlan {
    revision: FunctionRevisionPlan,
    declaration_origin: SourceOrigin,
    declaration_content_hash: Sha256Digest,
}

#[derive(Debug)]
struct AllocatedStandardUpgradePlan {
    source_template: StoredSourceRevision,
    source_ids: PreparedSourceIds,
    catalogue_revision: CatalogueRevisionId,
    schemas: Vec<SchemaDefinition>,
    object_types: Vec<ObjectTypeDefinition>,
    expressions: Vec<ExpressionArtifact>,
    origin_templates: Vec<DefinitionOrigin>,
    functions: Vec<AllocatedStandardUpgradeFunctionPlan>,
}

#[derive(Debug)]
struct AllocatedStandardUpgradeFunctionPlan {
    definition: FunctionDefinition,
    semantic_hash_version: FunctionSemanticHashVersion,
    semantic_hash: Sha256Digest,
    language_version: String,
    artifact: ExecutableArtifact,
    references: Vec<DefinitionReference>,
    declaration_origin: SourceOrigin,
    declaration_content_hash: Sha256Digest,
    revision: AllocatedStandardUpgradeFunctionRevision,
}

#[derive(Debug)]
enum AllocatedStandardUpgradeFunctionRevision {
    Reused(Box<FunctionRevisionRecord>),
    New {
        id: FunctionRevisionId,
        revision_number: u64,
    },
}

/// Gate-eight output. The catalogue is valid and all remaining material is
/// already lowered and allocated.
#[derive(Debug)]
struct StandardUpgradeCatalogueCandidate {
    plan: AllocatedStandardUpgradePlan,
    catalogue: CatalogueSnapshot,
}

/// Gate-nine output. The source hashes are placeholders that cannot escape
/// this private pipeline.
#[derive(Debug)]
struct StandardUpgradeCandidateRecords {
    source: StoredSourceRevision,
    catalogue: CatalogueSnapshot,
    origins: Vec<DefinitionOrigin>,
    expressions: Vec<ExpressionArtifact>,
    current_function_revisions: Vec<FunctionRevisionRecord>,
    new_function_revisions: Vec<FunctionRevisionRecord>,
    references: Vec<DefinitionReference>,
}

/// Gate-ten output. Every canonical hash has been calculated from the typed
/// Gate-nine records.
#[derive(Debug)]
struct CanonicalStandardUpgradeCandidate {
    records: StandardUpgradeCandidateRecords,
    source_bundle_hash: Sha256Digest,
    source_revision_hash: Sha256Digest,
    catalogue_hash: Sha256Digest,
}

/// Prepares one complete durable candidate from a successful compiler check.
///
/// This function does not parse source again and does not mutate storage. It
/// rejects a stale base before it allocates any candidate identity.
pub fn prepare(
    report: &CheckReport,
    expected_base: RevisionPair,
    active: &ActiveDatabaseRevision,
) -> Result<DeployableRevision, PrepareError> {
    prepare_with_allocator(report, expected_base, active, CandidateAllocator::legacy())
}

fn prepare_with_allocator(
    report: &CheckReport,
    expected_base: RevisionPair,
    active: &ActiveDatabaseRevision,
    mut allocations: CandidateAllocator,
) -> Result<DeployableRevision, PrepareError> {
    if !report.diagnostics().is_empty() || report.checked_bundle().is_none() {
        return Err(PrepareError::CheckNotComplete {
            diagnostic_count: report.diagnostics().len(),
        });
    }
    if expected_base != active.pair() {
        return Err(PrepareError::ExpectedBaseMismatch {
            expected: expected_base,
            active: active.pair(),
        });
    }
    let Some(checked) = report.checked_bundle() else {
        return Err(PrepareError::CheckNotComplete {
            diagnostic_count: report.diagnostics().len(),
        });
    };
    if checked.base_catalogue_revision() != active.pair().catalogue() {
        return Err(PrepareError::CheckedBaseMismatch {
            checked: checked.base_catalogue_revision(),
            active: active.pair().catalogue(),
        });
    }

    preflight(report.parse_report(), checked, active)?;
    let generic = !checked.client_functions().is_empty();
    let identities = if generic {
        IdentityMap::build_generic(checked, active, &mut allocations)?
    } else {
        IdentityMap::build_legacy(checked, active, &mut allocations)?
    };
    let source = PreparedSource::new(
        report.parse_report(),
        expected_base.source(),
        &mut allocations,
    )?;
    CandidateBuilder::new(
        report.parse_report(),
        checked,
        active,
        identities,
        source,
        if generic {
            PreparationMode::Generic
        } else {
            PreparationMode::LegacyV1
        },
        allocations.catalogue_revision(),
    )
    .build()
}

/// Prepares one standard-backed application candidate from a complete standard check.
///
/// This function is distinct from [`prepare`]. It accepts no legacy report or bundle.
pub fn prepare_standard_application(
    report: &StandardApplicationCheckReport,
    expected_base: RevisionPair,
    active: &ActiveDatabaseRevision,
) -> Result<DeployableRevision, PrepareStandardApplicationError> {
    let allocations = CandidateAllocator::standard(report.standard_library().verified_snapshot());
    prepare_standard_application_with_allocator(report, expected_base, active, allocations)
}

pub(crate) fn prepare_standard_application_with_allocator(
    report: &StandardApplicationCheckReport,
    expected_base: RevisionPair,
    active: &ActiveDatabaseRevision,
    mut allocations: CandidateAllocator,
) -> Result<DeployableRevision, PrepareStandardApplicationError> {
    let Some(view) = report.preparation_view() else {
        return Err(PrepareStandardApplicationError::CheckNotComplete {
            diagnostic_count: report.diagnostics().len(),
        });
    };
    if !report.diagnostics().is_empty() {
        return Err(PrepareStandardApplicationError::CheckNotComplete {
            diagnostic_count: report.diagnostics().len(),
        });
    }
    if expected_base != active.pair() {
        return Err(PrepareStandardApplicationError::ExpectedBaseMismatch {
            expected: expected_base,
            active: active.pair(),
        });
    }
    if view.checked().base_catalogue_revision() != active.pair().catalogue() {
        return Err(PrepareStandardApplicationError::CheckedBaseMismatch {
            checked: view.checked().base_catalogue_revision(),
            active: active.pair().catalogue(),
        });
    }
    let Some(active_standard) = active.catalogue_hash_context().standard() else {
        return Err(PrepareStandardApplicationError::StandardLibraryUnavailable);
    };
    if view.standard_catalogue_revision() != active_standard.catalogue().revision() {
        return Err(PrepareStandardApplicationError::StandardCatalogueMismatch {
            checked: view.standard_catalogue_revision(),
            active: active_standard.catalogue().revision(),
        });
    }
    if view.standard_library_revision() != active_standard.revision() {
        return Err(PrepareStandardApplicationError::StandardRevisionMismatch {
            checked: view.standard_library_revision(),
            active: active_standard.revision(),
        });
    }
    if view.standard_library_digest() != active_standard.digest() {
        return Err(PrepareStandardApplicationError::StandardDigestMismatch {
            checked: view.standard_library_digest(),
            active: active_standard.digest(),
        });
    }
    let evidence = view.evidence();
    let declaration_evidence = declaration_type_evidence(evidence.declaration_uses(), view.uses())?;
    body_type_evidence(evidence.type_uses(), view.uses())?;
    let signature_evidence = function_type_reference_evidence(
        &declaration_evidence,
        evidence.standard_type_references(),
        view.standard_type_references(),
    )?;
    let client_returns = signature_evidence.materialise_client_returns(view.checked())?;
    let standard_preflight = standard_preflight(
        report.parse_report(),
        view.checked(),
        active,
        report.standard_library(),
        &client_returns,
        &declaration_evidence,
    )
    .map_err(|source| PrepareStandardApplicationError::Prepare { source })?;
    let identities = IdentityMap::build_standard(
        view.checked(),
        active,
        &mut allocations,
        &standard_preflight.function_identities,
    )
    .map_err(|source| PrepareStandardApplicationError::Prepare { source })?;
    let catalogue_revision = allocations.catalogue_revision();
    let source_ids = PreparedSourceIds::allocate(report.parse_report(), &mut allocations)
        .map_err(|source| PrepareStandardApplicationError::Prepare { source })?;
    let source =
        PreparedSource::from_ids(report.parse_report(), expected_base.source(), source_ids)
            .map_err(|source| PrepareStandardApplicationError::Prepare { source })?;
    CandidateBuilder::new(
        report.parse_report(),
        view.checked(),
        active,
        identities,
        source,
        PreparationMode::StandardV2 {
            standard: report.standard_library(),
            declaration_evidence,
            signature_evidence,
            standard_preflight: Box::new(standard_preflight),
        },
        catalogue_revision,
    )
    .build()
    .map_err(|source| PrepareStandardApplicationError::Prepare { source })
}

/// A fail-closed error returned while preparing a standard-backed application candidate.
#[non_exhaustive]
#[derive(Debug)]
pub enum PrepareStandardApplicationError {
    /// Standard checking did not produce one complete checked bundle.
    CheckNotComplete { diagnostic_count: usize },
    /// The requested source and catalogue base is not the active pair.
    ExpectedBaseMismatch {
        expected: RevisionPair,
        active: RevisionPair,
    },
    /// The checked application base does not match the active catalogue.
    CheckedBaseMismatch {
        checked: CatalogueRevisionId,
        active: CatalogueRevisionId,
    },
    /// The active revision has no standard-library context.
    StandardLibraryUnavailable,
    /// The checked and active standard catalogues differ.
    StandardCatalogueMismatch {
        checked: CatalogueRevisionId,
        active: CatalogueRevisionId,
    },
    /// The checked and active standard-library revisions differ.
    StandardRevisionMismatch {
        checked: StandardLibraryRevisionId,
        active: StandardLibraryRevisionId,
    },
    /// The checked and active standard-library digests differ.
    StandardDigestMismatch {
        checked: Sha256Digest,
        active: Sha256Digest,
    },
    /// Checked declaration type evidence does not match its canonical use.
    DeclarationTypeEvidenceMismatch { kind: crate::CheckedTypeUseKind },
    /// Checked body type evidence does not match one function.
    BodyTypeEvidenceMismatch { function: CheckedFunctionId },
    /// Checked standard function-type references do not match one function.
    FunctionTypeReferenceMismatch { function: CheckedFunctionId },
    /// Shared durable preparation rejected the checked application.
    Prepare { source: PrepareError },
}

impl fmt::Display for PrepareStandardApplicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CheckNotComplete { diagnostic_count } => write!(
                formatter,
                "the standard application check has {diagnostic_count} diagnostics"
            ),
            Self::ExpectedBaseMismatch { .. } => formatter
                .write_str("the expected application base does not match the active revision"),
            Self::CheckedBaseMismatch { .. } => formatter
                .write_str("the checked application base does not match the active revision"),
            Self::StandardLibraryUnavailable => {
                formatter.write_str("the active database has no standard library")
            }
            Self::StandardCatalogueMismatch { .. } => formatter.write_str(
                "the checked standard catalogue does not match the active standard catalogue",
            ),
            Self::StandardRevisionMismatch { .. } => formatter.write_str(
                "the checked standard library revision does not match the active standard library revision",
            ),
            Self::StandardDigestMismatch { .. } => formatter.write_str(
                "the checked standard library digest does not match the active standard library digest",
            ),
            Self::DeclarationTypeEvidenceMismatch { kind } => {
                write!(formatter, "the checked declaration type evidence does not match its {} type use", checked_type_use_kind_tag(*kind))
            }
            Self::BodyTypeEvidenceMismatch { function } => write!(
                formatter,
                "the checked body type evidence does not match function {function}"
            ),
            Self::FunctionTypeReferenceMismatch { function } => write!(
                formatter,
                "the checked function type references do not match function {function}"
            ),
            Self::Prepare { source } => {
                write!(formatter, "the standard application could not be prepared: {source}")
            }
        }
    }
}

impl Error for PrepareStandardApplicationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Prepare { source } => Some(source),
            Self::CheckNotComplete { .. }
            | Self::ExpectedBaseMismatch { .. }
            | Self::CheckedBaseMismatch { .. }
            | Self::StandardLibraryUnavailable
            | Self::StandardCatalogueMismatch { .. }
            | Self::StandardRevisionMismatch { .. }
            | Self::StandardDigestMismatch { .. }
            | Self::DeclarationTypeEvidenceMismatch { .. }
            | Self::BodyTypeEvidenceMismatch { .. }
            | Self::FunctionTypeReferenceMismatch { .. } => None,
        }
    }
}

fn checked_type_use_kind_tag(kind: crate::CheckedTypeUseKind) -> &'static str {
    match kind {
        crate::CheckedTypeUseKind::Field { .. } => "field",
        crate::CheckedTypeUseKind::Parameter { .. } => "parameter",
        crate::CheckedTypeUseKind::State { .. } => "state",
        crate::CheckedTypeUseKind::Return { .. } => "return",
        crate::CheckedTypeUseKind::Expression { .. } => "expression",
        crate::CheckedTypeUseKind::Result { .. } => "result",
    }
}

enum PreparationMode<'a> {
    Generic,
    LegacyV1,
    StandardV1Match {
        declaration_evidence: DeclarationEvidence,
        standard_preflight: Box<StandardPreflight>,
    },
    StandardV2Plan {
        standard: &'a crate::CheckedStandardLibrary,
        declaration_evidence: DeclarationEvidence,
        signature_evidence: SignatureEvidence,
        standard_preflight: Box<StandardPreflight>,
    },
    StandardV2 {
        standard: &'a crate::CheckedStandardLibrary,
        declaration_evidence: DeclarationEvidence,
        signature_evidence: SignatureEvidence,
        standard_preflight: Box<StandardPreflight>,
    },
}

/// One declaration type selected for candidate lowering.
///
/// A standard value keeps the checked durable identity separate from its
/// compatibility scalar. Artefact validation uses the compatibility scalar;
/// durable candidate lowering selects the mode-specific core type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CandidateResolvedType {
    LegacyScalar(StandardScalar),
    StandardValue {
        type_id: TypeId,
        compatibility: StandardScalar,
    },
    /// A standard opaque value has no legacy scalar compatibility form.
    StandardOpaqueValue(TypeId),
    Named(TypeId),
    Reference(TypeId),
}

impl CandidateResolvedType {
    fn from_compatibility(compatibility: ResolvedType) -> Result<Self, PrepareError> {
        if let Some(scalar) = compatibility.legacy_scalar() {
            return Ok(Self::LegacyScalar(scalar));
        }
        if let Some(type_id) = compatibility.named_type() {
            if is_sealed_inspect_type_id(type_id) {
                return Ok(Self::StandardOpaqueValue(type_id));
            }
            return Ok(Self::Named(type_id));
        }
        if let Some(target) = compatibility.reference_target() {
            return Ok(Self::Reference(target));
        }
        if compatibility.value_type().is_some() {
            return Err(invalid_checked_declaration_type_evidence());
        }
        Err(invalid_checked_declaration_type_evidence())
    }

    fn compatibility_type(self) -> ResolvedType {
        match self {
            Self::LegacyScalar(scalar) => ResolvedType::Scalar(scalar),
            Self::StandardValue { compatibility, .. } => ResolvedType::Scalar(compatibility),
            Self::StandardOpaqueValue(type_id) | Self::Named(type_id) => {
                ResolvedType::Named(type_id)
            }
            Self::Reference(target) => ResolvedType::Reference { target },
        }
    }
}

/// Evidence mapped through the candidate identity map before type lowering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MappedEvidenceTarget {
    Value(TypeId),
    Named(TypeId),
    ObjectReference(TypeId),
    Unknown,
}

fn invalid_checked_declaration_type_evidence() -> PrepareError {
    PrepareError::InvalidCheckedBundle {
        reason: "checked standard declaration type evidence disagrees with its semantic type",
    }
}

/// Selects one closed candidate type from checked compatibility and evidence.
fn candidate_from_mapped_evidence(
    compatibility: ResolvedType,
    evidence: Option<MappedEvidenceTarget>,
) -> Result<CandidateResolvedType, PrepareError> {
    let candidate = CandidateResolvedType::from_compatibility(compatibility)?;
    let Some(evidence) = evidence else {
        return Ok(candidate);
    };
    if let CandidateResolvedType::LegacyScalar(compatibility) = candidate
        && let MappedEvidenceTarget::Value(type_id) = evidence
    {
        return Ok(CandidateResolvedType::StandardValue {
            type_id,
            compatibility,
        });
    }
    if let CandidateResolvedType::Reference(target) = candidate
        && let MappedEvidenceTarget::ObjectReference(actual) = evidence
        && target == actual
    {
        return Ok(CandidateResolvedType::Reference(target));
    }
    if let CandidateResolvedType::Named(target) | CandidateResolvedType::StandardOpaqueValue(target) =
        candidate
        && let MappedEvidenceTarget::Named(actual) = evidence
        && target == actual
    {
        if is_sealed_inspect_type_id(target) {
            return Ok(CandidateResolvedType::StandardOpaqueValue(target));
        }
        return Ok(CandidateResolvedType::Named(target));
    }
    Err(invalid_checked_declaration_type_evidence())
}

/// The purpose of one selected candidate type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CandidateTypeProjection {
    Compatibility,
    Durable,
}

/// The candidate lowering policy selected by preparation mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CandidateLoweringMode {
    Generic,
    LegacyV1,
    StandardV1Match,
    StandardV2Plan,
    StandardV2,
}

impl CandidateLoweringMode {
    fn lower(
        self,
        candidate: CandidateResolvedType,
        projection: CandidateTypeProjection,
    ) -> ResolvedType {
        match projection {
            CandidateTypeProjection::Compatibility => candidate.compatibility_type(),
            CandidateTypeProjection::Durable => match candidate {
                CandidateResolvedType::StandardValue {
                    type_id,
                    compatibility,
                } => self.lower_durable_standard_value(type_id, compatibility),
                CandidateResolvedType::StandardOpaqueValue(type_id) => {
                    self.lower_durable_standard_opaque_value(type_id)
                }
                candidate => candidate.compatibility_type(),
            },
        }
    }

    fn lower_durable_standard_opaque_value(self, type_id: TypeId) -> ResolvedType {
        let _ = self;
        ResolvedType::Value(type_id)
    }

    fn lower_durable_standard_value(
        self,
        type_id: TypeId,
        compatibility: StandardScalar,
    ) -> ResolvedType {
        match self {
            Self::Generic | Self::LegacyV1 | Self::StandardV1Match => {
                ResolvedType::Scalar(compatibility)
            }
            Self::StandardV2Plan | Self::StandardV2 => ResolvedType::Value(type_id),
        }
    }
}

/// The two catalogue views selected from one checked declaration stream.
struct ObjectTypeProjections {
    compatibility: Vec<ObjectTypeDefinition>,
    durable: Vec<ObjectTypeDefinition>,
}

impl PreparationMode<'_> {
    /// Selects one declaration type from one checked carrier.
    ///
    /// Compatibility serves artefact validators. Durable serves the candidate
    /// catalogue and semantic hashing. Standard V2 modes lower standard values
    /// to their durable identity; version-one modes retain scalar compatibility.
    fn lower_candidate_type(
        &self,
        candidate: CandidateResolvedType,
        projection: CandidateTypeProjection,
    ) -> ResolvedType {
        self.candidate_lowering_mode().lower(candidate, projection)
    }

    fn candidate_lowering_mode(&self) -> CandidateLoweringMode {
        match self {
            Self::Generic => CandidateLoweringMode::Generic,
            Self::LegacyV1 => CandidateLoweringMode::LegacyV1,
            Self::StandardV1Match { .. } => CandidateLoweringMode::StandardV1Match,
            Self::StandardV2Plan { .. } => CandidateLoweringMode::StandardV2Plan,
            Self::StandardV2 { .. } => CandidateLoweringMode::StandardV2,
        }
    }

    fn catalogue_hash_context(&self) -> CatalogueHashContext {
        match self {
            Self::Generic | Self::LegacyV1 | Self::StandardV1Match { .. } => {
                CatalogueHashContext::version_one()
            }
            Self::StandardV2Plan { .. } => CatalogueHashContext::version_one(),
            Self::StandardV2 { standard, .. } => {
                CatalogueHashContext::version_two(standard.verified_snapshot().clone())
            }
        }
    }
    fn durable_standard_catalogue(&self) -> Option<&CatalogueSnapshot> {
        match self {
            Self::Generic | Self::LegacyV1 | Self::StandardV1Match { .. } => None,
            Self::StandardV2Plan { standard, .. } | Self::StandardV2 { standard, .. } => {
                Some(standard.verified_snapshot().catalogue())
            }
        }
    }

    fn standard_preflight(&self) -> Option<&StandardPreflight> {
        match self {
            Self::Generic | Self::LegacyV1 => None,
            Self::StandardV1Match {
                standard_preflight, ..
            }
            | Self::StandardV2Plan {
                standard_preflight, ..
            }
            | Self::StandardV2 {
                standard_preflight, ..
            } => Some(standard_preflight),
        }
    }

    fn signature_evidence(&self) -> Option<&SignatureEvidence> {
        match self {
            Self::StandardV2 {
                signature_evidence, ..
            }
            | Self::StandardV2Plan {
                signature_evidence, ..
            } => Some(signature_evidence),
            Self::Generic | Self::LegacyV1 | Self::StandardV1Match { .. } => None,
        }
    }

    fn semantic_hash_version(
        &self,
        references: &[DefinitionReference],
    ) -> FunctionSemanticHashVersion {
        match self {
            Self::Generic | Self::LegacyV1 | Self::StandardV1Match { .. } => {
                FunctionSemanticHashVersion::Version1
            }
            Self::StandardV2Plan { .. } | Self::StandardV2 { .. }
                if references.iter().any(|reference| {
                    matches!(reference.target(), DefinitionReferenceTarget::ValueType(_))
                }) =>
            {
                FunctionSemanticHashVersion::Version2
            }
            Self::StandardV2Plan { .. } | Self::StandardV2 { .. } => {
                FunctionSemanticHashVersion::Version1
            }
        }
    }
}

/// Rebinds only the revision identity of a Gate-seven function signature.
///
/// Gate seven creates the complete semantic signature before the private
/// allocator can issue a new revision identity. Gate eight keeps that checked
/// signature and changes only this durable link.
fn rebind_function_definition_revision(
    definition: &FunctionDefinition,
    revision: FunctionRevisionId,
) -> FunctionDefinition {
    FunctionDefinition::new(
        definition.id(),
        definition.name().clone(),
        definition.domain(),
        definition.parameters().to_vec(),
        definition.return_type().clone(),
        revision,
        definition.security(),
        definition.transaction(),
        definition.volatility(),
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum EvidenceTarget {
    Value(TypeId),
    Named(CheckedTypeId),
    ObjectReference(CheckedTypeId),
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct EvidenceUse {
    kind: crate::CheckedTypeUseKind,
    target: EvidenceTarget,
    location: SourceLocation,
}

impl EvidenceUse {
    fn from_type_use(type_use: &crate::CheckedApplicationTypeUse) -> Self {
        let target = if let Some(value) = type_use.value() {
            EvidenceTarget::Value(value.type_id())
        } else if let Some(target) = type_use.named_type() {
            EvidenceTarget::Named(target)
        } else if let Some(reference) = type_use.object_reference() {
            EvidenceTarget::ObjectReference(reference.target())
        } else {
            EvidenceTarget::Unknown
        };
        Self {
            kind: type_use.kind(),
            target,
            location: type_use.location().clone(),
        }
    }
}

#[derive(Clone)]
struct DeclarationEvidence {
    ordered: Vec<EvidenceUse>,
    remaining: Vec<EvidenceUse>,
    consumed: Vec<EvidenceUse>,
}

impl DeclarationEvidence {
    fn from_validated(uses: &[crate::CheckedApplicationTypeUse]) -> Self {
        let ordered = uses
            .iter()
            .map(EvidenceUse::from_type_use)
            .collect::<Vec<_>>();
        Self {
            remaining: ordered.clone(),
            ordered,
            consumed: Vec::new(),
        }
    }

    fn consume(&mut self, kind: crate::CheckedTypeUseKind) -> Result<EvidenceUse, PrepareError> {
        let Some(index) = self
            .remaining
            .iter()
            .position(|evidence| evidence.kind == kind)
        else {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "checked standard declaration has no validated type evidence",
            });
        };
        let evidence = self.remaining.remove(index);
        self.consumed.push(evidence.clone());
        Ok(evidence)
    }

    fn lookup(&self, kind: crate::CheckedTypeUseKind) -> Result<EvidenceUse, PrepareError> {
        self.ordered
            .iter()
            .find(|evidence| evidence.kind == kind)
            .cloned()
            .ok_or(PrepareError::InvalidCheckedBundle {
                reason: "checked standard declaration has no validated type evidence",
            })
    }

    fn is_empty(&self) -> bool {
        self.remaining.is_empty()
    }
}

fn declaration_type_evidence(
    expected: &[crate::CheckedApplicationTypeUse],
    actual: &[crate::CheckedApplicationTypeUse],
) -> Result<DeclarationEvidence, PrepareStandardApplicationError> {
    let actual = actual
        .iter()
        .filter(|type_use| is_declaration_kind(type_use.kind()))
        .cloned()
        .collect::<Vec<_>>();

    for (expected, actual) in expected.iter().zip(&actual) {
        if expected != actual {
            return Err(
                PrepareStandardApplicationError::DeclarationTypeEvidenceMismatch {
                    kind: expected.kind(),
                },
            );
        }
    }
    if let Some(expected) = expected.get(actual.len()) {
        return Err(
            PrepareStandardApplicationError::DeclarationTypeEvidenceMismatch {
                kind: expected.kind(),
            },
        );
    }
    if let Some(actual) = actual.get(expected.len()) {
        return Err(
            PrepareStandardApplicationError::DeclarationTypeEvidenceMismatch {
                kind: actual.kind(),
            },
        );
    }

    Ok(DeclarationEvidence::from_validated(expected))
}

fn signature_owner(kind: crate::CheckedTypeUseKind) -> Option<CheckedFunctionId> {
    match kind {
        crate::CheckedTypeUseKind::Field { .. }
        | crate::CheckedTypeUseKind::State { .. }
        | crate::CheckedTypeUseKind::Expression { .. }
        | crate::CheckedTypeUseKind::Result { .. } => None,
        crate::CheckedTypeUseKind::Parameter { owner, .. }
        | crate::CheckedTypeUseKind::Return { owner, .. } => Some(owner),
    }
}

#[derive(Clone)]
struct SignatureSlot {
    owner: CheckedFunctionId,
    flattened_ordinal: u32,
    kind: crate::CheckedTypeUseKind,
    target: EvidenceTarget,
    location: SourceLocation,
}

#[derive(Clone)]
struct ValidatedClientReturn {
    owner: CheckedFunctionId,
    target: EvidenceTarget,
    semantic_type: SemanticType<CheckedTypeId>,
    location: SourceLocation,
}

#[derive(Clone)]
enum ValidatedClientBody {
    BooleanLiteral(bool),
    Expression(CheckedClientExpression),
    Procedural {
        locals: Vec<CheckedClientLocal>,
        statements: Vec<CheckedClientStatement>,
        return_expression: CheckedClientExpression,
    },
    ControlFlow {
        locals: Vec<CheckedClientLocal>,
        statements: Vec<CheckedClientControlFlowStatement>,
    },

    StateBlock {
        return_expression: CheckedClientExpression,
        states: Vec<CheckedClientStateSlot>,
    },
    ExternalContract(String),
}

#[derive(Clone)]
struct ValidatedClient {
    id: CheckedFunctionId,
    name: orna_core::catalogue::QualifiedSemanticName,
    location: SourceLocation,
    security: FunctionSecurity,
    transaction: Option<FunctionTransaction>,
    volatility: FunctionVolatility,
    parameters: Vec<crate::CheckedServerFunctionParameter>,
    return_target: EvidenceTarget,
    return_semantic_type: SemanticType<CheckedTypeId>,
    return_shape: CheckedClientReturnShape,
    return_location: SourceLocation,
    return_scalar: Option<StandardScalar>,
    body: ValidatedClientBody,
    references: Vec<crate::CheckedDefinitionReference>,
    capabilities: Vec<crate::CheckedClientCapability>,
}

#[derive(Clone, Default)]
struct ValidatedClientReturns {
    ordered: Vec<ValidatedClientReturn>,
}

impl ValidatedClientReturns {
    fn for_client(
        &self,
        index: usize,
        owner: CheckedFunctionId,
    ) -> Result<&ValidatedClientReturn, PrepareError> {
        let Some(return_evidence) = self.ordered.get(index) else {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "checked CLIENT function has no exact validated return evidence",
            });
        };
        if return_evidence.owner != owner {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "checked CLIENT function has no exact validated return evidence",
            });
        }
        Ok(return_evidence)
    }
}

#[derive(Clone)]
struct ValidatedFunctionIdentity {
    domain: FunctionDomain,
}

#[derive(Clone, Default)]
struct ValidatedFunctionIdentities {
    order: Vec<CheckedFunctionId>,
    functions: HashMap<CheckedFunctionId, ValidatedFunctionIdentity>,
}

impl ValidatedFunctionIdentities {
    fn from_declarations(
        declarations: &DeclarationEvidence,
        checked: &CheckedBundle,
    ) -> Result<Self, PrepareError> {
        let mut functions = HashMap::with_capacity(
            checked.server_functions().len() + checked.client_functions().len(),
        );
        for function in checked.server_functions() {
            if functions
                .insert(
                    function.id(),
                    ValidatedFunctionIdentity {
                        domain: FunctionDomain::Server,
                    },
                )
                .is_some()
            {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "duplicate checked function",
                });
            }
        }
        for function in checked.client_functions() {
            if functions
                .insert(
                    function.id(),
                    ValidatedFunctionIdentity {
                        domain: FunctionDomain::Client,
                    },
                )
                .is_some()
            {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "duplicate checked function",
                });
            }
        }

        let mut order = Vec::with_capacity(functions.len());
        for declaration in &declarations.ordered {
            let Some(owner) = signature_owner(declaration.kind) else {
                continue;
            };
            if !functions.contains_key(&owner) {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "checked standard function owners do not match declaration evidence",
                });
            }
            if !order.contains(&owner) {
                order.push(owner);
            }
        }
        if order.len() != functions.len() {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "checked standard function owners do not match declaration evidence",
            });
        }
        Ok(Self { order, functions })
    }

    fn order(&self) -> &[CheckedFunctionId] {
        &self.order
    }

    fn domain(&self, owner: CheckedFunctionId) -> Result<FunctionDomain, PrepareError> {
        self.functions
            .get(&owner)
            .map(|identity| identity.domain)
            .ok_or(PrepareError::InvalidCheckedBundle {
                reason: "checked standard function owners do not match declaration evidence",
            })
    }
}

#[derive(Clone)]
struct StandardPreflight {
    clients: HashMap<CheckedFunctionId, ValidatedClient>,
    function_identities: ValidatedFunctionIdentities,
}

#[derive(Clone, Default)]
struct SignatureEvidence {
    ordered: Vec<SignatureSlot>,
}

impl SignatureEvidence {
    fn from_validated(
        declarations: &DeclarationEvidence,
        standard_type_references: &[crate::CheckedStandardTypeReference],
    ) -> Result<Self, PrepareStandardApplicationError> {
        let mut ordinals = HashMap::new();
        let mut standard_type_references = standard_type_references.iter();
        let mut ordered = Vec::new();

        for declaration in &declarations.ordered {
            let Some(owner) = signature_owner(declaration.kind) else {
                continue;
            };
            let ordinal = ordinals.entry(owner).or_insert(0_u32);
            let flattened_ordinal = *ordinal;
            *ordinal = ordinal.checked_add(1).ok_or(
                PrepareStandardApplicationError::FunctionTypeReferenceMismatch { function: owner },
            )?;

            match &declaration.target {
                EvidenceTarget::Value(target) => {
                    let Some(reference) = standard_type_references.next() else {
                        return Err(
                            PrepareStandardApplicationError::FunctionTypeReferenceMismatch {
                                function: owner,
                            },
                        );
                    };
                    if reference.owner() != owner
                        || reference.ordinal() != flattened_ordinal
                        || reference.target() != *target
                        || reference.location() != &declaration.location
                    {
                        return Err(
                            PrepareStandardApplicationError::FunctionTypeReferenceMismatch {
                                function: owner,
                            },
                        );
                    }
                }
                EvidenceTarget::Named(_) | EvidenceTarget::ObjectReference(_) => {}
                EvidenceTarget::Unknown => {
                    return Err(
                        PrepareStandardApplicationError::FunctionTypeReferenceMismatch {
                            function: owner,
                        },
                    );
                }
            }

            ordered.push(SignatureSlot {
                owner,
                flattened_ordinal,
                kind: declaration.kind,
                target: declaration.target.clone(),
                location: declaration.location.clone(),
            });
        }

        if let Some(reference) = standard_type_references.next() {
            return Err(
                PrepareStandardApplicationError::FunctionTypeReferenceMismatch {
                    function: reference.owner(),
                },
            );
        }

        Ok(Self { ordered })
    }
    fn function_slots(&self, owner: CheckedFunctionId) -> impl Iterator<Item = &SignatureSlot> {
        self.ordered.iter().filter(move |slot| slot.owner == owner)
    }

    fn materialise_client_returns(
        &self,
        checked: &CheckedBundle,
    ) -> Result<ValidatedClientReturns, PrepareStandardApplicationError> {
        let mut ordered = Vec::with_capacity(checked.client_functions().len());
        for function in checked.client_functions() {
            let owner = function.id();
            let slots = self.function_slots(owner).collect::<Vec<_>>();
            let Some(slot) = slots
                .iter()
                .find(|slot| matches!(slot.kind, crate::CheckedTypeUseKind::Return { .. }))
            else {
                return Err(
                    PrepareStandardApplicationError::FunctionTypeReferenceMismatch {
                        function: owner,
                    },
                );
            };
            let crate::CheckedTypeUseKind::Return {
                owner: slot_owner,
                ordinal,
            } = slot.kind
            else {
                return Err(
                    PrepareStandardApplicationError::FunctionTypeReferenceMismatch {
                        function: owner,
                    },
                );
            };
            if slot_owner != owner || ordinal != 0 {
                return Err(
                    PrepareStandardApplicationError::FunctionTypeReferenceMismatch {
                        function: owner,
                    },
                );
            }
            let expected_target = match function.return_type() {
                SemanticType::Scalar(_) => matches!(slot.target, EvidenceTarget::Value(_)),
                SemanticType::Named(_) => matches!(slot.target, EvidenceTarget::Named(_)),
                SemanticType::Reference { .. } => {
                    matches!(slot.target, EvidenceTarget::ObjectReference(_))
                }
            };
            if !expected_target {
                return Err(
                    PrepareStandardApplicationError::FunctionTypeReferenceMismatch {
                        function: owner,
                    },
                );
            }
            ordered.push(ValidatedClientReturn {
                owner,
                target: slot.target.clone(),
                semantic_type: function.return_type(),
                location: slot.location.clone(),
            });
        }
        Ok(ValidatedClientReturns { ordered })
    }
}

fn is_declaration_kind(kind: crate::CheckedTypeUseKind) -> bool {
    match kind {
        crate::CheckedTypeUseKind::Field { .. }
        | crate::CheckedTypeUseKind::Parameter { .. }
        | crate::CheckedTypeUseKind::State { .. }
        | crate::CheckedTypeUseKind::Return { .. } => true,
        crate::CheckedTypeUseKind::Expression { .. } | crate::CheckedTypeUseKind::Result { .. } => {
            false
        }
    }
}
fn body_type_evidence(
    expected: &[crate::CheckedApplicationTypeUse],
    actual: &[crate::CheckedApplicationTypeUse],
) -> Result<(), PrepareStandardApplicationError> {
    if expected == actual {
        return Ok(());
    }

    for (expected, actual) in expected.iter().zip(actual) {
        if expected != actual {
            return body_type_evidence_mismatch(expected, Some(actual));
        }
    }
    if expected.len() > actual.len() {
        return body_type_evidence_mismatch(&expected[actual.len()], None);
    }
    body_type_evidence_mismatch(&actual[expected.len()], None)
}

fn body_type_evidence_mismatch(
    mismatched: &crate::CheckedApplicationTypeUse,
    other: Option<&crate::CheckedApplicationTypeUse>,
) -> Result<(), PrepareStandardApplicationError> {
    let function = type_use_function(mismatched.kind())
        .or_else(|| other.and_then(|type_use| type_use_function(type_use.kind())));
    let Some(function) = function else {
        return Err(
            PrepareStandardApplicationError::DeclarationTypeEvidenceMismatch {
                kind: mismatched.kind(),
            },
        );
    };
    Err(PrepareStandardApplicationError::BodyTypeEvidenceMismatch { function })
}

fn type_use_function(kind: crate::CheckedTypeUseKind) -> Option<CheckedFunctionId> {
    match kind {
        crate::CheckedTypeUseKind::Field { .. } => None,
        crate::CheckedTypeUseKind::Parameter { owner, .. }
        | crate::CheckedTypeUseKind::Return { owner, .. }
        | crate::CheckedTypeUseKind::State { owner, .. }
        | crate::CheckedTypeUseKind::Expression { owner, .. }
        | crate::CheckedTypeUseKind::Result { owner, .. } => Some(owner),
    }
}

fn function_type_reference_evidence(
    declarations: &DeclarationEvidence,
    expected: &[crate::CheckedStandardTypeReference],
    actual: &[crate::CheckedStandardTypeReference],
) -> Result<SignatureEvidence, PrepareStandardApplicationError> {
    if expected == actual {
        return SignatureEvidence::from_validated(declarations, expected);
    }

    for (expected, actual) in expected.iter().zip(actual) {
        if expected != actual {
            return Err(
                PrepareStandardApplicationError::FunctionTypeReferenceMismatch {
                    function: expected.owner(),
                },
            );
        }
    }
    let function = if expected.len() > actual.len() {
        expected[actual.len()].owner()
    } else {
        actual[expected.len()].owner()
    };
    Err(PrepareStandardApplicationError::FunctionTypeReferenceMismatch { function })
}
/// A fail-closed error returned while preparing a durable candidate.
#[derive(Debug)]
pub enum PrepareError {
    /// Parsing or semantic checking did not produce one complete checked bundle.
    CheckNotComplete { diagnostic_count: usize },
    /// The requested source and catalogue base is not the active pair.
    ExpectedBaseMismatch {
        expected: RevisionPair,
        active: RevisionPair,
    },
    /// The checked bundle was resolved against a different catalogue revision.
    CheckedBaseMismatch {
        checked: CatalogueRevisionId,
        active: CatalogueRevisionId,
    },
    /// An existing checked identity does not match the active catalogue.
    ExistingDefinitionMismatch { definition: DefinitionIdentity },
    /// A retained source location does not identify a valid UTF-8 byte range.
    InvalidSourceLocation {
        logical_path: String,
        byte_start: usize,
        byte_end: usize,
    },
    /// One retained source unit is too large for durable byte offsets.
    SourceContentTooLarge { logical_path: String, bytes: usize },
    /// The number of source units does not fit the durable ordinal type.
    SourceUnitCountExceedsU32 { count: usize },
    /// The number of references for one function does not fit the durable ordinal type.
    ReferenceCountExceedsU32 {
        function: CheckedFunctionId,
        count: usize,
    },
    /// No later immutable revision number exists for this function.
    FunctionRevisionNumberExhausted { function: FunctionId },
    /// A checked result violates a compiler-internal preparation invariant.
    InvalidCheckedBundle { reason: &'static str },
    /// A constant-expression artifact could not be encoded.
    ConstantArtifact(ConstantExpressionError),
    /// A server-plan artifact could not be encoded.
    ServerPlanArtifact(ServerPlanError),
    /// A server-mutation-plan artifact could not be encoded.
    ServerMutationPlanArtifact(ServerMutationPlanError),
    /// A canonical version-1 digest could not be calculated.
    CanonicalHash(CanonicalHashError),
    /// The complete candidate catalogue is invalid.
    Catalogue(CatalogueSnapshotError),
    /// The complete durable revision envelope is invalid.
    Revision(RevisionInvariantError),
}

impl fmt::Display for PrepareError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CheckNotComplete { .. } => {
                formatter.write_str("compiler check did not produce a complete checked bundle")
            }
            Self::ExpectedBaseMismatch { .. } => {
                formatter.write_str("expected revision pair is not active")
            }
            Self::CheckedBaseMismatch { .. } => {
                formatter.write_str("checked catalogue base is not active")
            }
            Self::ExistingDefinitionMismatch { .. } => {
                formatter.write_str("existing checked definition differs from active catalogue")
            }
            Self::InvalidSourceLocation { .. } => {
                formatter.write_str("checked source location is invalid")
            }
            Self::SourceContentTooLarge { .. } => {
                formatter.write_str("source content exceeds durable byte-offset range")
            }
            Self::SourceUnitCountExceedsU32 { .. } => {
                formatter.write_str("source unit count exceeds durable ordinal range")
            }
            Self::ReferenceCountExceedsU32 { .. } => {
                formatter.write_str("function reference count exceeds durable ordinal range")
            }
            Self::FunctionRevisionNumberExhausted { .. } => {
                formatter.write_str("function revision number is exhausted")
            }
            Self::InvalidCheckedBundle { reason } => formatter.write_str(reason),
            Self::ConstantArtifact(error) => error.fmt(formatter),
            Self::ServerPlanArtifact(error) => error.fmt(formatter),
            Self::ServerMutationPlanArtifact(error) => error.fmt(formatter),
            Self::CanonicalHash(error) => error.fmt(formatter),
            Self::Catalogue(error) => error.fmt(formatter),
            Self::Revision(error) => error.fmt(formatter),
        }
    }
}

impl Error for PrepareError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ConstantArtifact(error) => Some(error),
            Self::ServerPlanArtifact(error) => Some(error),
            Self::ServerMutationPlanArtifact(error) => Some(error),
            Self::CanonicalHash(error) => Some(error),
            Self::Catalogue(error) => Some(error),
            Self::Revision(error) => Some(error),
            _ => None,
        }
    }
}

impl From<ConstantExpressionError> for PrepareError {
    fn from(error: ConstantExpressionError) -> Self {
        Self::ConstantArtifact(error)
    }
}

impl From<ServerPlanError> for PrepareError {
    fn from(error: ServerPlanError) -> Self {
        Self::ServerPlanArtifact(error)
    }
}

impl From<ServerMutationPlanError> for PrepareError {
    fn from(error: ServerMutationPlanError) -> Self {
        Self::ServerMutationPlanArtifact(error)
    }
}

impl From<CanonicalHashError> for PrepareError {
    fn from(error: CanonicalHashError) -> Self {
        Self::CanonicalHash(error)
    }
}

impl From<CatalogueSnapshotError> for PrepareError {
    fn from(error: CatalogueSnapshotError) -> Self {
        Self::Catalogue(error)
    }
}

impl From<RevisionInvariantError> for PrepareError {
    fn from(error: RevisionInvariantError) -> Self {
        Self::Revision(error)
    }
}

fn preflight(
    parse_report: &ParseReport,
    checked: &CheckedBundle,
    active: &ActiveDatabaseRevision,
) -> Result<(), PrepareError> {
    let locations = checked_locations(checked);
    validate_common_preflight(parse_report, checked, active, &locations)?;
    for function in checked.server_functions() {
        if u32::try_from(function.references().len()).is_err() {
            return Err(PrepareError::ReferenceCountExceedsU32 {
                function: function.id(),
                count: function.references().len(),
            });
        }
        if function
            .references()
            .iter()
            .any(|reference| !supports_definition_reference_kind(reference.kind()))
        {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "checked function contains an unsupported definition reference kind",
            });
        }
    }
    Ok(())
}

fn standard_preflight(
    parse_report: &ParseReport,
    checked: &CheckedBundle,
    active: &ActiveDatabaseRevision,
    standard: &crate::CheckedStandardLibrary,
    client_returns: &ValidatedClientReturns,
    declarations: &DeclarationEvidence,
) -> Result<StandardPreflight, PrepareError> {
    let locations = standard_checked_locations(checked, client_returns)?;
    validate_common_preflight(parse_report, checked, active, &locations)?;

    for function in checked.server_functions() {
        validate_server_function_preflight(function, active)?;
    }

    let mut ordered_clients = Vec::with_capacity(checked.client_functions().len());
    for (index, function) in checked.client_functions().iter().enumerate() {
        let return_evidence = client_returns.for_client(index, function.id())?;
        ordered_clients.push((
            function.id(),
            validate_client_function_preflight(function, active, standard, return_evidence)?,
        ));
    }
    let function_identities =
        ValidatedFunctionIdentities::from_declarations(declarations, checked)?;
    let clients = ordered_clients.into_iter().collect();
    Ok(StandardPreflight {
        clients,
        function_identities,
    })
}

fn validate_common_preflight(
    parse_report: &ParseReport,
    checked: &CheckedBundle,
    active: &ActiveDatabaseRevision,
    locations: &[&SourceLocation],
) -> Result<(), PrepareError> {
    let sources = validated_sources(parse_report)?;
    for location in locations {
        validate_location(location, sources.as_map())?;
    }
    validate_unique_fields(checked)?;
    validate_field_renames(checked, active)
}

struct ValidatedSources<'a> {
    by_logical_path: HashMap<&'a str, &'a str>,
}

impl ValidatedSources<'_> {
    fn as_map(&self) -> &HashMap<&str, &str> {
        &self.by_logical_path
    }
}

fn validated_sources(parse_report: &ParseReport) -> Result<ValidatedSources<'_>, PrepareError> {
    let units = parse_report.units();
    if u32::try_from(units.len()).is_err() {
        return Err(PrepareError::SourceUnitCountExceedsU32 { count: units.len() });
    }
    let mut by_logical_path = HashMap::with_capacity(units.len());
    for unit in units {
        if by_logical_path
            .insert(unit.logical_path(), unit.source_text())
            .is_some()
        {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "checked source bundle contains a duplicate logical path",
            });
        }
        if u32::try_from(unit.source_text().len()).is_err() {
            return Err(PrepareError::SourceContentTooLarge {
                logical_path: unit.logical_path().to_owned(),
                bytes: unit.source_text().len(),
            });
        }
    }
    Ok(ValidatedSources { by_logical_path })
}

fn validate_server_function_preflight(
    function: &crate::CheckedServerFunction,
    active: &ActiveDatabaseRevision,
) -> Result<(), PrepareError> {
    if u32::try_from(function.references().len()).is_err() {
        return Err(PrepareError::ReferenceCountExceedsU32 {
            function: function.id(),
            count: function.references().len(),
        });
    }
    if function
        .references()
        .iter()
        .any(|reference| !supports_definition_reference_kind(reference.kind()))
    {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "checked function contains an unsupported definition reference kind",
        });
    }
    validate_existing_function(
        function.id(),
        function.name(),
        FunctionDomain::Server,
        active,
    )?;
    validate_existing_server_parameters(function, active)
}

fn validate_existing_server_parameters(
    function: &crate::CheckedServerFunction,
    active: &ActiveDatabaseRevision,
) -> Result<(), PrepareError> {
    let CheckedFunctionId::Existing(owner) = function.id() else {
        if function
            .parameters()
            .iter()
            .any(|parameter| matches!(parameter.id(), CheckedParameterId::Existing(_)))
        {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "existing checked parameter belongs to a provisional function",
            });
        }
        return Ok(());
    };

    for parameter in function.parameters() {
        let CheckedParameterId::Existing(id) = parameter.id() else {
            continue;
        };
        let matches = active
            .catalogue()
            .function_by_id(owner)
            .and_then(|base| base.parameter_by_id(id))
            .is_some_and(|base| base.name() == parameter.name());
        if !matches {
            return Err(existing_mismatch(DefinitionIdentity::Parameter {
                owner,
                parameter: id,
            }));
        }
    }
    Ok(())
}

fn validate_client_function_preflight(
    function: &crate::CheckedClientFunction,
    active: &ActiveDatabaseRevision,
    standard: &crate::CheckedStandardLibrary,
    return_evidence: &ValidatedClientReturn,
) -> Result<ValidatedClient, PrepareError> {
    if function.domain() != FunctionDomain::Client {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "checked CLIENT function has an unsupported domain",
        });
    }
    let legacy_boolean_body = matches!(
        function.body(),
        CheckedClientFunctionBody::BooleanLiteral { .. }
    );
    if legacy_boolean_body && !function.parameters().is_empty() {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "checked CLIENT function declares parameters",
        });
    }
    if legacy_boolean_body
        && (function.return_type() != SemanticType::Scalar(StandardScalar::Boolean)
            || return_evidence.semantic_type != SemanticType::Scalar(StandardScalar::Boolean))
    {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "checked CLIENT function does not return BOOLEAN from the checked standard library",
        });
    }
    if !legacy_boolean_body && function.return_type() != return_evidence.semantic_type {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "checked CLIENT function return evidence does not match its return type",
        });
    }
    if function.security() != FunctionSecurity::Invoker {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "checked CLIENT function has an unsupported security mode",
        });
    }
    if function.transaction().is_some() {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "checked CLIENT function has an unsupported transaction mode",
        });
    }
    if function.volatility() != FunctionVolatility::Immutable {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "checked CLIENT function has an unsupported volatility mode",
        });
    }
    if u32::try_from(function.references().len()).is_err() {
        return Err(PrepareError::ReferenceCountExceedsU32 {
            function: function.id(),
            count: function.references().len(),
        });
    }
    if function
        .references()
        .iter()
        .any(|reference| !supports_definition_reference_kind(reference.kind()))
    {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "checked CLIENT function contains an unsupported definition reference kind",
        });
    }
    if legacy_boolean_body && !function.references().is_empty() {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "checked CLIENT function contains unsupported application definition references",
        });
    }

    let return_scalar = match (function.return_type(), &return_evidence.target) {
        (SemanticType::Scalar(scalar), EvidenceTarget::Value(type_id)) => {
            let Some(value_type) = standard
                .value_types()
                .iter()
                .find(|value| value.id() == *type_id)
            else {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "checked CLIENT function return value type has no standard definition",
                });
            };
            let expected = match value_type.representation_contract() {
                "orna.kernel.value.boolean@1" => StandardScalar::Boolean,
                "orna.kernel.value.integer@1" => StandardScalar::Integer,
                "orna.kernel.value.bigint@1" => StandardScalar::BigInt,
                "orna.kernel.value.float@1" => StandardScalar::Float,
                "orna.kernel.value.character-large-object@1" => {
                    StandardScalar::CharacterLargeObject
                }
                "orna.kernel.value.binary-large-object@1" => StandardScalar::BinaryLargeObject,
                _ => {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: "checked CLIENT function return type has an unsupported standard contract",
                    });
                }
            };
            if scalar != expected {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "checked CLIENT function return type disagrees with its standard contract",
                });
            }
            Some(expected)
        }
        (SemanticType::Named(target), EvidenceTarget::Named(evidence)) if target == *evidence => {
            None
        }
        (SemanticType::Reference { target }, EvidenceTarget::ObjectReference(evidence))
            if target == *evidence =>
        {
            None
        }
        _ => {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "checked CLIENT function return evidence has an unsupported target",
            });
        }
    };

    let body = match function.body() {
        CheckedClientFunctionBody::BooleanLiteral { value, .. } => {
            if !function.parameters().is_empty()
                || function.return_type() != SemanticType::Scalar(StandardScalar::Boolean)
                || return_scalar != Some(StandardScalar::Boolean)
                || !function.references().is_empty()
                || !function.capabilities().is_empty()
            {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "checked Boolean CLIENT function violates its closed body invariants",
                });
            }
            ValidatedClientBody::BooleanLiteral(*value)
        }
        CheckedClientFunctionBody::Expression { expression } => {
            ValidatedClientBody::Expression(expression.clone())
        }
        CheckedClientFunctionBody::Procedural {
            locals,
            statements,
            return_expression,
        } => ValidatedClientBody::Procedural {
            locals: locals.clone(),
            statements: statements.clone(),
            return_expression: return_expression.clone(),
        },
        CheckedClientFunctionBody::ControlFlow { locals, statements } => {
            ValidatedClientBody::ControlFlow {
                locals: locals.clone(),
                statements: statements.clone(),
            }
        }

        CheckedClientFunctionBody::ExternalContract { identity, .. } => {
            ValidatedClientBody::ExternalContract(identity.clone())
        }
        CheckedClientFunctionBody::StateBlock {
            states,
            return_expression,
        } => ValidatedClientBody::StateBlock {
            return_expression: return_expression.clone(),
            states: states.clone(),
        },
        #[cfg(test)]
        CheckedClientFunctionBody::Unsupported => {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "checked CLIENT function has an unsupported body",
            });
        }
    };

    validate_existing_function(
        function.id(),
        function.name(),
        FunctionDomain::Client,
        active,
    )?;
    Ok(ValidatedClient {
        id: function.id(),
        name: function.name().clone(),
        location: function.location().clone(),
        security: function.security(),
        transaction: function.transaction(),
        volatility: function.volatility(),
        parameters: function.parameters().to_vec(),
        return_target: return_evidence.target.clone(),
        return_semantic_type: return_evidence.semantic_type,
        return_shape: function.return_shape(),
        return_location: return_evidence.location.clone(),
        return_scalar,
        body,
        references: function.references().to_vec(),
        capabilities: function.capabilities().to_vec(),
    })
}
fn validate_generic_client_function(
    function: &crate::CheckedClientFunction,
    active: &ActiveDatabaseRevision,
) -> Result<ValidatedClient, PrepareError> {
    if function.domain() != FunctionDomain::Client {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "checked CLIENT function has an unsupported domain",
        });
    }
    if function.security() != FunctionSecurity::Invoker {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "checked CLIENT function has an unsupported security mode",
        });
    }
    if function.transaction().is_some() {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "checked CLIENT function has an unsupported transaction mode",
        });
    }
    if function.volatility() != FunctionVolatility::Immutable {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "checked CLIENT function has an unsupported volatility mode",
        });
    }
    validate_existing_function(
        function.id(),
        function.name(),
        FunctionDomain::Client,
        active,
    )?;
    let return_scalar = match function.return_type() {
        SemanticType::Scalar(scalar) => Some(scalar),
        SemanticType::Named(_) | SemanticType::Reference { .. } => None,
    };
    let body = match function.body() {
        CheckedClientFunctionBody::BooleanLiteral { value, .. } => {
            ValidatedClientBody::BooleanLiteral(*value)
        }
        CheckedClientFunctionBody::Expression { expression } => {
            ValidatedClientBody::Expression(expression.clone())
        }
        CheckedClientFunctionBody::Procedural {
            locals,
            statements,
            return_expression,
        } => ValidatedClientBody::Procedural {
            locals: locals.clone(),
            statements: statements.clone(),
            return_expression: return_expression.clone(),
        },
        CheckedClientFunctionBody::ControlFlow { locals, statements } => {
            ValidatedClientBody::ControlFlow {
                locals: locals.clone(),
                statements: statements.clone(),
            }
        }
        CheckedClientFunctionBody::StateBlock {
            states,
            return_expression,
        } => {
            if !states.is_empty() {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "checked CLIENT state declarations require standard-backed preparation",
                });
            }
            ValidatedClientBody::StateBlock {
                return_expression: return_expression.clone(),
                states: states.clone(),
            }
        }
        CheckedClientFunctionBody::ExternalContract { identity, .. } => {
            ValidatedClientBody::ExternalContract(identity.clone())
        }
        #[cfg(test)]
        CheckedClientFunctionBody::Unsupported => {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "checked CLIENT function has an unsupported body",
            });
        }
    };
    Ok(ValidatedClient {
        id: function.id(),
        name: function.name().clone(),
        location: function.location().clone(),
        security: function.security(),
        transaction: function.transaction(),
        volatility: function.volatility(),
        parameters: function.parameters().to_vec(),
        return_target: match function.return_type() {
            SemanticType::Scalar(_) => EvidenceTarget::Unknown,
            SemanticType::Named(target) => EvidenceTarget::Named(target),
            SemanticType::Reference { target } => EvidenceTarget::ObjectReference(target),
        },
        return_semantic_type: function.return_type(),
        return_shape: function.return_shape(),
        return_location: function.location().clone(),
        return_scalar,
        body,
        references: function.references().to_vec(),
        capabilities: function.capabilities().to_vec(),
    })
}

fn validate_existing_function(
    id: CheckedFunctionId,
    name: &orna_core::catalogue::QualifiedSemanticName,
    domain: FunctionDomain,
    active: &ActiveDatabaseRevision,
) -> Result<(), PrepareError> {
    let CheckedFunctionId::Existing(id) = id else {
        return Ok(());
    };
    let matches = active
        .catalogue()
        .function_by_id(id)
        .is_some_and(|function| function.name() == name && function.domain() == domain);
    if matches {
        Ok(())
    } else {
        Err(existing_mismatch(DefinitionIdentity::Function(id)))
    }
}

fn validate_unique_fields(checked: &CheckedBundle) -> Result<(), PrepareError> {
    for field in checked
        .object_types()
        .iter()
        .flat_map(|object_type| object_type.fields())
    {
        if field.unique()
            && !supports_unique_text_or_required_reference(field.semantic_type(), field.nullable())
        {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: UNIQUE_FIELD_MESSAGE,
            });
        }
    }
    Ok(())
}

fn validate_field_renames(
    checked: &CheckedBundle,
    active: &ActiveDatabaseRevision,
) -> Result<(), PrepareError> {
    let mut evidence = HashSet::new();
    let mut renamed_fields = HashSet::new();
    let mut consumed_names = HashSet::new();
    let mut produced_names = HashSet::new();
    for rename in checked.field_renames() {
        let CheckedTypeId::Existing(owner) = rename.owner else {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "field rename has a provisional owner",
            });
        };
        let CheckedFieldId::Existing(field) = rename.field else {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "field rename has a provisional field",
            });
        };
        if rename.old_name == rename.new_name
            || !evidence.insert((
                owner,
                field,
                rename.old_name.as_str(),
                rename.new_name.as_str(),
            ))
        {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "field rename evidence is duplicate or has equal names",
            });
        }
        if !renamed_fields.insert((owner, field)) {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "multiple field renames bind one checked field",
            });
        }
        if !consumed_names.insert((owner, rename.old_name.as_str())) {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "field rename consumes one old name more than once",
            });
        }
        if !produced_names.insert((owner, rename.new_name.as_str())) {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "field rename produces one new name more than once",
            });
        }
        let candidate_owner = checked
            .object_types()
            .iter()
            .find(|object_type| object_type.id() == rename.owner)
            .ok_or(PrepareError::InvalidCheckedBundle {
                reason: "field rename owner is absent from the candidate catalogue",
            })?;
        if candidate_owner
            .fields()
            .iter()
            .any(|value| value.name() == rename.old_name)
        {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "field rename candidate still declares its old field",
            });
        }
        let candidate = candidate_owner
            .fields()
            .iter()
            .find(|value| value.id() == rename.field);
        if candidate.is_none_or(|value| value.name() != rename.new_name) {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "field rename does not bind its candidate field",
            });
        }
        let active_type = active.catalogue().object_type_by_id(owner).ok_or(
            PrepareError::InvalidCheckedBundle {
                reason: "field rename owner is absent from the active catalogue",
            },
        )?;
        validate_active_field_rename(active_type, rename)?;
    }
    for rename in checked.field_renames() {
        if checked.field_renames().iter().any(|other| {
            other.owner == rename.owner
                && (other.new_name == rename.old_name || other.old_name == rename.new_name)
        }) {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "field rename chain or swap is not supported",
            });
        }
    }
    Ok(())
}

fn validate_active_field_rename(
    active: &ObjectTypeDefinition,
    rename: &CheckedFieldRename,
) -> Result<(), PrepareError> {
    let CheckedFieldId::Existing(field) = rename.field else {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "field rename has a provisional field",
        });
    };
    match (
        active.field_by_name(&rename.old_name),
        active.field_by_name(&rename.new_name),
    ) {
        (Some(old), None) if old.id() == field => Ok(()),
        (None, Some(new)) if new.id() == field => Ok(()),
        (Some(_), None) | (None, Some(_)) => Err(PrepareError::InvalidCheckedBundle {
            reason: "field rename names do not resolve to its checked field",
        }),
        (Some(_), Some(_)) => Err(PrepareError::InvalidCheckedBundle {
            reason: "field rename active catalogue contains both names",
        }),
        (None, None) => Err(PrepareError::InvalidCheckedBundle {
            reason: "field rename active catalogue contains neither name",
        }),
    }
}

fn supports_definition_reference_kind(kind: DefinitionReferenceKind) -> bool {
    SUPPORTED_DEFINITION_REFERENCE_KINDS.contains(&kind)
}

const SUPPORTED_DEFINITION_REFERENCE_KINDS: &[DefinitionReferenceKind] = &[
    DefinitionReferenceKind::FunctionCall,
    DefinitionReferenceKind::NamedType,
    DefinitionReferenceKind::ObjectReference,
    DefinitionReferenceKind::ParameterRead,
    DefinitionReferenceKind::QueryObject,
    DefinitionReferenceKind::QueryField,
    DefinitionReferenceKind::Expression,
    DefinitionReferenceKind::WriteObject,
    DefinitionReferenceKind::WriteField,
];

fn checked_locations(checked: &CheckedBundle) -> Vec<&SourceLocation> {
    let mut locations = Vec::new();
    for schema in checked.schemas() {
        locations.push(schema.location());
    }
    for object_type in checked.object_types() {
        locations.push(object_type.location());
        for field in object_type.fields() {
            locations.push(field.location());
            if let Some(default) = field.default() {
                locations.push(default.location());
            }
        }
    }
    for (_, _, _, location) in checked.enum_types() {
        locations.push(location);
    }
    for record_value_type in checked.record_value_types() {
        locations.push(record_value_type.location());
        locations.extend(
            record_value_type
                .fields()
                .iter()
                .map(|field| field.location()),
        );
    }
    for function in checked.server_functions() {
        locations.push(function.location());
        locations.extend(function.parameters().iter().map(|value| value.location()));
        locations.extend(
            function
                .return_columns()
                .iter()
                .map(|value| value.location()),
        );
        locations.extend(function.references().iter().map(|value| value.location()));
    }
    locations
}

fn standard_checked_locations<'a>(
    checked: &'a CheckedBundle,
    client_returns: &'a ValidatedClientReturns,
) -> Result<Vec<&'a SourceLocation>, PrepareError> {
    let mut locations = checked_locations(checked);
    for (index, function) in checked.client_functions().iter().enumerate() {
        locations.push(function.location());
        locations.extend(
            function
                .parameters()
                .iter()
                .map(|parameter| parameter.location()),
        );
        locations.push(&client_returns.for_client(index, function.id())?.location);
        match function.body() {
            CheckedClientFunctionBody::BooleanLiteral { location, .. }
            | CheckedClientFunctionBody::ExternalContract { location, .. } => {
                locations.push(location);
            }
            CheckedClientFunctionBody::Expression { expression } => {
                client_expression_locations(expression, &mut locations);
            }
            CheckedClientFunctionBody::Procedural {
                locals,
                statements,
                return_expression,
            } => {
                locations.extend(locals.iter().map(|local| local.location()));
                for statement in statements {
                    client_expression_locations(statement.expression(), &mut locations);
                }
                client_expression_locations(return_expression, &mut locations);
            }
            CheckedClientFunctionBody::ControlFlow { locals, statements } => {
                locations.extend(locals.iter().map(|local| local.location()));
                client_control_flow_statement_locations(statements, &mut locations);
            }

            CheckedClientFunctionBody::StateBlock {
                states,
                return_expression,
            } => {
                for state in states {
                    locations.push(state.location());
                    if let CheckedStateDefault::Expression(expression) = state.default() {
                        client_expression_locations(expression, &mut locations);
                    }
                }
                client_expression_locations(return_expression, &mut locations);
            }
            #[cfg(test)]
            CheckedClientFunctionBody::Unsupported => {}
        }
        locations.extend(
            function
                .references()
                .iter()
                .map(|reference| reference.location()),
        );
    }
    Ok(locations)
}

fn client_control_flow_statement_locations<'a>(
    statements: &'a [CheckedClientControlFlowStatement],
    locations: &mut Vec<&'a SourceLocation>,
) {
    for statement in statements {
        locations.push(statement.location());
        match statement {
            CheckedClientControlFlowStatement::Let { expression, .. }
            | CheckedClientControlFlowStatement::Assignment { expression, .. } => {
                client_expression_locations(expression, locations);
            }
            CheckedClientControlFlowStatement::Return { expression, .. } => {
                if let Some(expression) = expression {
                    client_expression_locations(expression, locations);
                }
            }
            CheckedClientControlFlowStatement::If {
                branches,
                else_statements,
                ..
            } => {
                for branch in branches {
                    locations.push(branch.location());
                    client_expression_locations(branch.condition(), locations);
                    client_control_flow_statement_locations(branch.statements(), locations);
                }
                if let Some(statements) = else_statements {
                    client_control_flow_statement_locations(statements, locations);
                }
            }
            CheckedClientControlFlowStatement::While {
                condition,
                statements,
                ..
            } => {
                client_expression_locations(condition, locations);
                client_control_flow_statement_locations(statements, locations);
            }
        }
    }
}

fn client_expression_locations<'a>(
    expression: &'a CheckedClientExpression,
    locations: &mut Vec<&'a SourceLocation>,
) {
    match expression {
        CheckedClientExpression::Call {
            arguments,
            location,
            ..
        } => {
            locations.push(location);
            for (_, argument) in arguments {
                client_expression_locations(argument, locations);
            }
        }
        CheckedClientExpression::Await {
            expression,
            location,
        } => {
            locations.push(location);
            client_expression_locations(expression, locations);
        }
        CheckedClientExpression::Resource { operation } => {
            locations.push(operation.location());
            for (_, argument) in operation.arguments() {
                client_expression_locations(argument, locations);
            }
        }
        CheckedClientExpression::Action { operation } => {
            locations.push(operation.location());
            for (_, argument) in operation.arguments() {
                client_expression_locations(argument, locations);
            }
        }
        CheckedClientExpression::Inspect { operation } => {
            locations.push(operation.location());
            match operation {
                CheckedInspectOperation::Snapshot {
                    target, options, ..
                } => {
                    client_expression_locations(target, locations);
                    if let Some(options) = options {
                        client_expression_locations(options, locations);
                    }
                }
                CheckedInspectOperation::Projection { snapshot, .. } => {
                    client_expression_locations(snapshot, locations);
                }
            }
        }
        CheckedClientExpression::SourceIntrospection { location }
        | CheckedClientExpression::Input { location } => locations.push(location),
        CheckedClientExpression::Evaluate {
            expression,
            location,
        } => {
            locations.push(location);
            client_expression_locations(expression, locations);
        }
        CheckedClientExpression::String { location, .. }
        | CheckedClientExpression::Integer { location, .. }
        | CheckedClientExpression::Boolean { location, .. }
        | CheckedClientExpression::ParameterRead { location, .. }
        | CheckedClientExpression::LocalRead { location, .. }
        | CheckedClientExpression::FieldPath { location, .. } => locations.push(location),
        CheckedClientExpression::Concat {
            left,
            right,
            location,
        }
        | CheckedClientExpression::Binary {
            left,
            right,
            location,
            ..
        } => {
            locations.push(location);
            client_expression_locations(left, locations);
            client_expression_locations(right, locations);
        }
        CheckedClientExpression::Unary {
            expression,
            location,
            ..
        }
        | CheckedClientExpression::Parenthesized {
            expression,
            location,
        } => {
            locations.push(location);
            client_expression_locations(expression, locations);
        }
    }
}

fn validate_location(
    location: &SourceLocation,
    sources: &HashMap<&str, &str>,
) -> Result<(), PrepareError> {
    let start = location.span().start();
    let end = location.span().end();
    let Some(source) = sources.get(location.logical_path()).copied() else {
        return Err(invalid_location(location));
    };
    if start > end
        || end > source.len()
        || !source.is_char_boundary(start)
        || !source.is_char_boundary(end)
    {
        return Err(invalid_location(location));
    }
    Ok(())
}

fn invalid_location(location: &SourceLocation) -> PrepareError {
    PrepareError::InvalidSourceLocation {
        logical_path: location.logical_path().to_owned(),
        byte_start: location.span().start(),
        byte_end: location.span().end(),
    }
}

#[derive(Default)]
pub(crate) struct ReservedStandardIds {
    catalogues: HashSet<CatalogueRevisionId>,
    source_bundles: HashSet<SourceBundleId>,
    source_revisions: HashSet<SourceRevisionId>,
    source_units: HashSet<SourceUnitId>,
    schemas: HashSet<SchemaId>,
    types: HashSet<TypeId>,
}

impl ReservedStandardIds {
    pub(crate) fn from_snapshot(snapshot: &VerifiedStandardLibrarySnapshot) -> Self {
        let mut result = Self::default();
        result.catalogues.insert(snapshot.catalogue().revision());
        result.source_bundles.insert(snapshot.source().bundle());
        result.source_revisions.insert(snapshot.source().id());
        result
            .source_units
            .extend(snapshot.source().units().iter().map(StoredSourceUnit::id));
        result.schemas.extend(
            snapshot
                .catalogue()
                .schemas()
                .iter()
                .map(SchemaDefinition::id),
        );
        result.types.extend(
            snapshot
                .catalogue()
                .object_types()
                .iter()
                .map(ObjectTypeDefinition::id)
                .chain(
                    snapshot
                        .catalogue()
                        .value_types()
                        .iter()
                        .map(|value| value.id()),
                ),
        );
        result
    }
}

#[derive(Clone, Copy)]
pub(crate) struct CandidateIdSource {
    pub(crate) catalogue_revision: fn() -> CatalogueRevisionId,
    pub(crate) source_bundle: fn() -> SourceBundleId,
    pub(crate) source_revision: fn() -> SourceRevisionId,
    pub(crate) source_unit: fn() -> SourceUnitId,
    pub(crate) schema: fn() -> SchemaId,
    pub(crate) type_id: fn() -> TypeId,
    pub(crate) function_revision: fn() -> FunctionRevisionId,
}

impl CandidateIdSource {
    const RANDOM: Self = Self {
        catalogue_revision: CatalogueRevisionId::new,
        source_bundle: SourceBundleId::new,
        source_revision: SourceRevisionId::new,
        source_unit: SourceUnitId::new,
        schema: SchemaId::new,
        type_id: TypeId::new,
        function_revision: FunctionRevisionId::new,
    };
}

pub(crate) struct CandidateAllocator {
    reserved: Option<ReservedStandardIds>,
    source: CandidateIdSource,
}

impl CandidateAllocator {
    const fn legacy() -> Self {
        Self {
            reserved: None,
            source: CandidateIdSource::RANDOM,
        }
    }

    #[cfg(test)]
    fn legacy_with_source(source: CandidateIdSource) -> Self {
        Self {
            reserved: None,
            source,
        }
    }

    fn standard(snapshot: &VerifiedStandardLibrarySnapshot) -> Self {
        Self::with_source(
            ReservedStandardIds::from_snapshot(snapshot),
            CandidateIdSource::RANDOM,
        )
    }

    pub(crate) fn with_source(reserved: ReservedStandardIds, source: CandidateIdSource) -> Self {
        Self {
            reserved: Some(reserved),
            source,
        }
    }

    fn catalogue_revision(&mut self) -> CatalogueRevisionId {
        loop {
            let id = (self.source.catalogue_revision)();
            if self
                .reserved
                .as_ref()
                .is_none_or(|reserved| !reserved.catalogues.contains(&id))
            {
                return id;
            }
        }
    }

    fn source_bundle(&mut self) -> SourceBundleId {
        loop {
            let id = (self.source.source_bundle)();
            if self
                .reserved
                .as_ref()
                .is_none_or(|reserved| !reserved.source_bundles.contains(&id))
            {
                return id;
            }
        }
    }

    fn source_revision(&mut self) -> SourceRevisionId {
        loop {
            let id = (self.source.source_revision)();
            if self
                .reserved
                .as_ref()
                .is_none_or(|reserved| !reserved.source_revisions.contains(&id))
            {
                return id;
            }
        }
    }

    fn source_unit(&mut self) -> SourceUnitId {
        loop {
            let id = (self.source.source_unit)();
            if self
                .reserved
                .as_ref()
                .is_none_or(|reserved| !reserved.source_units.contains(&id))
            {
                return id;
            }
        }
    }

    fn schema(&mut self) -> SchemaId {
        loop {
            let id = (self.source.schema)();
            if self
                .reserved
                .as_ref()
                .is_none_or(|reserved| !reserved.schemas.contains(&id))
            {
                return id;
            }
        }
    }

    fn type_id(&mut self) -> TypeId {
        loop {
            let id = (self.source.type_id)();
            if self
                .reserved
                .as_ref()
                .is_none_or(|reserved| !reserved.types.contains(&id))
                && INVOCATION_CARRIERS.iter().all(|carrier| carrier.id() != id)
            {
                return id;
            }
        }
    }

    fn function_revision(&mut self) -> FunctionRevisionId {
        (self.source.function_revision)()
    }
}

#[derive(Clone, Default)]
struct IdentityMap {
    schemas: HashMap<CheckedSchemaId, SchemaId>,
    types: HashMap<CheckedTypeId, TypeId>,
    fields: HashMap<CheckedFieldId, FieldId>,
    expressions: HashMap<CheckedExpressionId, ExpressionId>,
    functions: HashMap<CheckedFunctionId, FunctionId>,
    parameters: HashMap<CheckedParameterId, ParameterId>,
}

impl IdentityMap {
    fn build_legacy(
        checked: &CheckedBundle,
        active: &ActiveDatabaseRevision,
        allocations: &mut CandidateAllocator,
    ) -> Result<Self, PrepareError> {
        Self::build(checked, active, allocations, None, true)
    }
    fn build_generic(
        checked: &CheckedBundle,
        active: &ActiveDatabaseRevision,
        allocations: &mut CandidateAllocator,
    ) -> Result<Self, PrepareError> {
        Self::build(checked, active, allocations, None, true)
    }

    fn build_standard(
        checked: &CheckedBundle,
        active: &ActiveDatabaseRevision,
        allocations: &mut CandidateAllocator,
        function_identities: &ValidatedFunctionIdentities,
    ) -> Result<Self, PrepareError> {
        Self::build(
            checked,
            active,
            allocations,
            Some(function_identities),
            true,
        )
    }

    fn build_matching_active(
        checked: &CheckedBundle,
        active: &ActiveDatabaseRevision,
        function_identities: &ValidatedFunctionIdentities,
    ) -> Result<Self, PrepareError> {
        let mut no_allocations = CandidateAllocator::legacy();
        Self::build(
            checked,
            active,
            &mut no_allocations,
            Some(function_identities),
            false,
        )
    }

    fn build(
        checked: &CheckedBundle,
        active: &ActiveDatabaseRevision,
        allocations: &mut CandidateAllocator,
        function_identities: Option<&ValidatedFunctionIdentities>,
        allow_provisional: bool,
    ) -> Result<Self, PrepareError> {
        Self::validate_existing(checked, active, function_identities.is_none())?;
        let mut result = Self::default();
        for schema in checked.schemas() {
            let id = match schema.id() {
                CheckedSchemaId::Existing(id) => id,
                CheckedSchemaId::Provisional(_) if allow_provisional => allocations.schema(),
                CheckedSchemaId::Provisional(_) => {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: "matched active source contains a provisional schema",
                    });
                }
            };
            insert_unique(
                &mut result.schemas,
                schema.id(),
                id,
                "duplicate checked schema",
            )?;
        }

        for object_type in checked.object_types() {
            let type_id = match object_type.id() {
                CheckedTypeId::Existing(id) => id,
                CheckedTypeId::Provisional(_) if allow_provisional => allocations.type_id(),
                CheckedTypeId::Provisional(_) => {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: "matched active source contains a provisional object type",
                    });
                }
            };
            insert_unique(
                &mut result.types,
                object_type.id(),
                type_id,
                "duplicate checked object type",
            )?;

            for field in object_type.fields() {
                let field_id = match field.id() {
                    CheckedFieldId::Existing(id) => id,
                    CheckedFieldId::Provisional(_) if allow_provisional => FieldId::new(),
                    CheckedFieldId::Provisional(_) => {
                        return Err(PrepareError::InvalidCheckedBundle {
                            reason: "matched active source contains a provisional field",
                        });
                    }
                };
                insert_consistent(
                    &mut result.fields,
                    field.id(),
                    field_id,
                    "checked field identity maps inconsistently",
                )?;

                if let Some(default) = field.default() {
                    let expression_id = match default.id() {
                        CheckedExpressionId::Existing(id) => id,
                        CheckedExpressionId::Provisional(_) if allow_provisional => {
                            ExpressionId::new()
                        }
                        CheckedExpressionId::Provisional(_) => {
                            return Err(PrepareError::InvalidCheckedBundle {
                                reason: "matched active source contains a provisional expression",
                            });
                        }
                    };
                    insert_consistent(
                        &mut result.expressions,
                        default.id(),
                        expression_id,
                        "checked expression identity maps inconsistently",
                    )?;
                }
            }
        }

        for (checked_id, _, _, _) in checked.enum_types() {
            let type_id = match checked_id {
                CheckedTypeId::Existing(id) => id,
                CheckedTypeId::Provisional(_) if allow_provisional => allocations.type_id(),
                CheckedTypeId::Provisional(_) => {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: "matched active source contains a provisional enum type",
                    });
                }
            };
            insert_unique(
                &mut result.types,
                checked_id,
                type_id,
                "duplicate checked enum type",
            )?;
        }

        for record_value_type in checked.record_value_types() {
            let type_id = match record_value_type.id() {
                CheckedTypeId::Existing(id) => id,
                CheckedTypeId::Provisional(_) if allow_provisional => allocations.type_id(),
                CheckedTypeId::Provisional(_) => {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: "matched active source contains a provisional record value type",
                    });
                }
            };
            insert_unique(
                &mut result.types,
                record_value_type.id(),
                type_id,
                "duplicate checked record value type",
            )?;
            for field in record_value_type.fields() {
                let field_id = match field.id() {
                    CheckedFieldId::Existing(id) => id,
                    CheckedFieldId::Provisional(_) if allow_provisional => FieldId::new(),
                    CheckedFieldId::Provisional(_) => {
                        return Err(PrepareError::InvalidCheckedBundle {
                            reason: "matched active source contains a provisional record value field",
                        });
                    }
                };
                insert_consistent(
                    &mut result.fields,
                    field.id(),
                    field_id,
                    "checked record value field identity maps inconsistently",
                )?;
            }
        }

        match function_identities {
            None => {
                for function in checked.server_functions() {
                    Self::map_server_function(&mut result, function, true, allow_provisional)?;
                }
                for function in checked.client_functions() {
                    let function_id = match function.id() {
                        CheckedFunctionId::Existing(id) => id,
                        CheckedFunctionId::Provisional(_) if allow_provisional => FunctionId::new(),
                        CheckedFunctionId::Provisional(_) => {
                            return Err(PrepareError::InvalidCheckedBundle {
                                reason: "matched active source contains a provisional function",
                            });
                        }
                    };
                    insert_unique(
                        &mut result.functions,
                        function.id(),
                        function_id,
                        "duplicate checked function",
                    )?;
                    for parameter in function.parameters() {
                        let parameter_id = match parameter.id() {
                            CheckedParameterId::Existing(id) => id,
                            CheckedParameterId::Provisional(_) if allow_provisional => {
                                ParameterId::new()
                            }
                            CheckedParameterId::Provisional(_) => {
                                return Err(PrepareError::InvalidCheckedBundle {
                                    reason: "matched active source contains a provisional parameter",
                                });
                            }
                        };
                        insert_consistent(
                            &mut result.parameters,
                            parameter.id(),
                            parameter_id,
                            "checked parameter identity maps inconsistently",
                        )?;
                    }
                }
            }
            Some(function_identities) => {
                for owner in function_identities.order() {
                    match function_identities.domain(*owner)? {
                        FunctionDomain::Server => {
                            let function = checked
                                .server_functions()
                                .iter()
                                .find(|function| function.id() == *owner)
                                .ok_or(PrepareError::InvalidCheckedBundle {
                                    reason: "checked standard function owners do not match declaration evidence",
                                })?;
                            Self::map_server_function(
                                &mut result,
                                function,
                                false,
                                allow_provisional,
                            )?;
                        }
                        FunctionDomain::Client => {
                            let function = checked
                                .client_functions()
                                .iter()
                                .find(|function| function.id() == *owner)
                                .ok_or(PrepareError::InvalidCheckedBundle {
                                    reason: "checked standard function owners do not match declaration evidence",
                                })?;
                            let function_id = match function.id() {
                                CheckedFunctionId::Existing(id) => id,
                                CheckedFunctionId::Provisional(_) if allow_provisional => {
                                    FunctionId::new()
                                }
                                CheckedFunctionId::Provisional(_) => {
                                    return Err(PrepareError::InvalidCheckedBundle {
                                        reason: "matched active source contains a provisional function",
                                    });
                                }
                            };
                            result.functions.insert(function.id(), function_id);
                            for parameter in function.parameters() {
                                let parameter_id = match parameter.id() {
                                    CheckedParameterId::Existing(id) => id,
                                    CheckedParameterId::Provisional(_) if allow_provisional => {
                                        ParameterId::new()
                                    }
                                    CheckedParameterId::Provisional(_) => {
                                        return Err(PrepareError::InvalidCheckedBundle {
                                            reason: "matched active source contains a provisional parameter",
                                        });
                                    }
                                };
                                insert_consistent(
                                    &mut result.parameters,
                                    parameter.id(),
                                    parameter_id,
                                    "checked parameter identity maps inconsistently",
                                )?;
                            }
                        }
                    }
                }
            }
        }

        // CLIENT expressions may target functions that are already installed
        // and therefore are not part of this checked bundle.  Their stable
        // function and parameter identities still have to be present in the
        // map before resource (or ordinary CLIENT-call) lowering can emit a
        // durable artifact.
        for function in checked.client_functions() {
            for reference in function.references() {
                let CheckedDefinitionReferenceTarget::Function(CheckedFunctionId::Existing(
                    function_id,
                )) = reference.target()
                else {
                    continue;
                };
                let active_function = active
                    .catalogue()
                    .function_by_id(function_id)
                    .or_else(|| {
                        active
                            .catalogue_hash_context()
                            .standard()
                            .and_then(|standard| standard.catalogue().function_by_id(function_id))
                    })
                    .ok_or_else(|| existing_mismatch(DefinitionIdentity::Function(function_id)))?;
                result
                    .functions
                    .entry(CheckedFunctionId::Existing(function_id))
                    .or_insert(function_id);
                for parameter in active_function.parameters() {
                    result
                        .parameters
                        .entry(CheckedParameterId::Existing(parameter.id()))
                        .or_insert(parameter.id());
                }
            }
        }

        Ok(result)
    }

    fn map_server_function(
        result: &mut Self,
        function: &crate::CheckedServerFunction,
        reject_duplicate: bool,
        allow_provisional: bool,
    ) -> Result<(), PrepareError> {
        let function_id = match function.id() {
            CheckedFunctionId::Existing(id) => id,
            CheckedFunctionId::Provisional(_) if allow_provisional => FunctionId::new(),
            CheckedFunctionId::Provisional(_) => {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "matched active source contains a provisional function",
                });
            }
        };
        if reject_duplicate {
            insert_unique(
                &mut result.functions,
                function.id(),
                function_id,
                "duplicate checked function",
            )?;
        } else {
            result.functions.insert(function.id(), function_id);
        }

        for parameter in function.parameters() {
            let parameter_id = match parameter.id() {
                CheckedParameterId::Existing(id) => id,
                CheckedParameterId::Provisional(_) if allow_provisional => ParameterId::new(),
                CheckedParameterId::Provisional(_) => {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: "matched active source contains a provisional parameter",
                    });
                }
            };
            insert_consistent(
                &mut result.parameters,
                parameter.id(),
                parameter_id,
                "checked parameter identity maps inconsistently",
            )?;
        }
        Ok(())
    }

    fn validate_existing(
        checked: &CheckedBundle,
        active: &ActiveDatabaseRevision,
        validate_legacy_server_functions: bool,
    ) -> Result<(), PrepareError> {
        for schema in checked.schemas() {
            let CheckedSchemaId::Existing(id) = schema.id() else {
                continue;
            };
            let matches = active
                .catalogue()
                .schema_by_id(id)
                .is_some_and(|base| base.name() == schema.name());
            if !matches {
                return Err(existing_mismatch(DefinitionIdentity::Schema(id)));
            }
        }

        for object_type in checked.object_types() {
            let owner = match object_type.id() {
                CheckedTypeId::Existing(id) => {
                    let matches = active
                        .catalogue()
                        .object_type_by_id(id)
                        .is_some_and(|base| base.name() == object_type.name());
                    if !matches {
                        return Err(existing_mismatch(DefinitionIdentity::ObjectType(id)));
                    }
                    Some(id)
                }
                CheckedTypeId::Provisional(_) => None,
            };

            for field in object_type.fields() {
                let field_id = match field.id() {
                    CheckedFieldId::Existing(id) => {
                        let owner = owner.ok_or(PrepareError::InvalidCheckedBundle {
                            reason: "existing checked field belongs to a provisional object type",
                        })?;
                        let matches = active
                            .catalogue()
                            .object_type_by_id(owner)
                            .and_then(|base| base.field_by_id(id))
                            .is_some_and(|base| base.name() == field.name());
                        let renamed = active
                            .catalogue()
                            .object_type_by_id(owner)
                            .and_then(|base| base.field_by_id(id))
                            .is_some_and(|base| {
                                checked.field_renames().iter().any(|rename| {
                                    rename.owner == object_type.id()
                                        && rename.field == field.id()
                                        && rename.old_name == base.name()
                                        && rename.new_name == field.name()
                                })
                            });
                        if !matches && !renamed {
                            return Err(existing_mismatch(DefinitionIdentity::Field {
                                owner,
                                field: id,
                            }));
                        }
                        Some(id)
                    }
                    CheckedFieldId::Provisional(_) => None,
                };

                if let Some(default) = field.default()
                    && let CheckedExpressionId::Existing(id) = default.id()
                {
                    let owner = owner.ok_or(PrepareError::InvalidCheckedBundle {
                        reason: "existing checked expression belongs to a provisional object type",
                    })?;
                    let field_id = field_id.ok_or(PrepareError::InvalidCheckedBundle {
                        reason: "existing checked expression belongs to a provisional field",
                    })?;
                    let field_matches = active
                        .catalogue()
                        .object_type_by_id(owner)
                        .and_then(|base| base.field_by_id(field_id))
                        .is_some_and(|base| base.default_expression() == Some(id));
                    let artifact_exists = active.expressions().iter().any(|value| value.id() == id);
                    if !field_matches || !artifact_exists {
                        return Err(existing_mismatch(DefinitionIdentity::Expression(id)));
                    }
                }
            }
        }

        for (checked_id, name, _, _) in checked.enum_types() {
            let CheckedTypeId::Existing(id) = checked_id else {
                continue;
            };
            let matches = active
                .catalogue()
                .enum_type_by_id(id)
                .is_some_and(|base| base.name() == name);
            if !matches {
                return Err(existing_mismatch(DefinitionIdentity::ValueType(id)));
            }
        }

        for record_value_type in checked.record_value_types() {
            let owner = match record_value_type.id() {
                CheckedTypeId::Existing(id) => {
                    let matches = active
                        .catalogue()
                        .record_value_type_by_id(id)
                        .is_some_and(|base| base.name() == record_value_type.name());
                    if !matches {
                        return Err(existing_mismatch(DefinitionIdentity::ValueType(id)));
                    }
                    Some(id)
                }
                CheckedTypeId::Provisional(_) => None,
            };
            for field in record_value_type.fields() {
                let CheckedFieldId::Existing(id) = field.id() else {
                    continue;
                };
                let owner = owner.ok_or(PrepareError::InvalidCheckedBundle {
                    reason: "existing checked record value field belongs to a provisional type",
                })?;
                let matches = active
                    .catalogue()
                    .record_value_type_by_id(owner)
                    .and_then(|base| base.field_by_id(id))
                    .is_some_and(|base| base.name() == field.name());
                if !matches {
                    return Err(existing_mismatch(DefinitionIdentity::Field {
                        owner,
                        field: id,
                    }));
                }
            }
        }

        if validate_legacy_server_functions {
            for function in checked.server_functions() {
                if let CheckedFunctionId::Existing(id) = function.id() {
                    let matches = active
                        .catalogue()
                        .function_by_id(id)
                        .is_some_and(|base| base.name() == function.name());
                    if !matches {
                        return Err(existing_mismatch(DefinitionIdentity::Function(id)));
                    }
                }
                validate_existing_server_parameters(function, active)?;
            }
        }
        Ok(())
    }

    fn schema(&self, id: CheckedSchemaId) -> Result<SchemaId, PrepareError> {
        copied(&self.schemas, id, "checked schema has no durable identity")
    }

    fn type_id(&self, id: CheckedTypeId) -> Result<TypeId, PrepareError> {
        if let CheckedTypeId::Existing(type_id) = id
            && (is_sealed_inspect_type_id(type_id)
                || type_id == orna_core::system::SYS_SOURCE_FUNCTION_TYPE_ID)
        {
            // Sealed system carriers use fixed identities and do not belong to
            // the application catalogue.
            return Ok(type_id);
        }
        copied(&self.types, id, "checked type has no durable identity")
    }

    fn field(&self, id: CheckedFieldId) -> Result<FieldId, PrepareError> {
        copied(&self.fields, id, "checked field has no durable identity")
    }

    fn expression(&self, id: CheckedExpressionId) -> Result<ExpressionId, PrepareError> {
        copied(
            &self.expressions,
            id,
            "checked expression has no durable identity",
        )
    }

    fn function(&self, id: CheckedFunctionId) -> Result<FunctionId, PrepareError> {
        copied(
            &self.functions,
            id,
            "checked function has no durable identity",
        )
    }

    fn parameter(&self, id: CheckedParameterId) -> Result<ParameterId, PrepareError> {
        copied(
            &self.parameters,
            id,
            "checked parameter has no durable identity",
        )
    }

    fn resolved_type(
        &self,
        semantic_type: SemanticType<CheckedTypeId>,
    ) -> Result<ResolvedType, PrepareError> {
        Ok(match semantic_type {
            SemanticType::Scalar(scalar) => ResolvedType::Scalar(scalar),
            SemanticType::Named(id) => ResolvedType::Named(self.type_id(id)?),
            SemanticType::Reference { target } => ResolvedType::Reference {
                target: self.type_id(target)?,
            },
        })
    }

    fn reference_target(
        &self,
        target: CheckedDefinitionReferenceTarget,
    ) -> Result<DefinitionReferenceTarget, PrepareError> {
        Ok(match target {
            CheckedDefinitionReferenceTarget::ObjectType(id) => {
                DefinitionReferenceTarget::ObjectType(self.type_id(id)?)
            }
            CheckedDefinitionReferenceTarget::ValueType(id) => {
                DefinitionReferenceTarget::ValueType(self.type_id(id)?)
            }
            CheckedDefinitionReferenceTarget::Field { owner, field } => {
                DefinitionReferenceTarget::Field {
                    owner: self.type_id(owner)?,
                    field: self.field(field)?,
                }
            }
            CheckedDefinitionReferenceTarget::Function(id) => {
                DefinitionReferenceTarget::Function(self.function(id)?)
            }
            CheckedDefinitionReferenceTarget::Parameter { owner, parameter } => {
                DefinitionReferenceTarget::Parameter {
                    owner: self.function(owner)?,
                    parameter: self.parameter(parameter)?,
                }
            }
            CheckedDefinitionReferenceTarget::Expression(id) => {
                DefinitionReferenceTarget::Expression(self.expression(id)?)
            }
        })
    }
}

fn existing_mismatch(definition: DefinitionIdentity) -> PrepareError {
    PrepareError::ExistingDefinitionMismatch { definition }
}

fn insert_unique<K: Eq + std::hash::Hash, V>(
    values: &mut HashMap<K, V>,
    key: K,
    value: V,
    reason: &'static str,
) -> Result<(), PrepareError> {
    if values.insert(key, value).is_some() {
        Err(PrepareError::InvalidCheckedBundle { reason })
    } else {
        Ok(())
    }
}

fn insert_consistent<K: Eq + std::hash::Hash, V: Copy + Eq>(
    values: &mut HashMap<K, V>,
    key: K,
    value: V,
    reason: &'static str,
) -> Result<(), PrepareError> {
    if values.get(&key).is_some_and(|existing| *existing != value) {
        return Err(PrepareError::InvalidCheckedBundle { reason });
    }
    values.insert(key, value);
    Ok(())
}

fn copied<K: Eq + std::hash::Hash + Copy, V: Copy>(
    values: &HashMap<K, V>,
    key: K,
    reason: &'static str,
) -> Result<V, PrepareError> {
    values
        .get(&key)
        .copied()
        .ok_or(PrepareError::InvalidCheckedBundle { reason })
}

struct PreparedSource {
    revision: StoredSourceRevision,
    unit_ids: HashMap<String, SourceUnitId>,
}

#[derive(Debug)]
struct PreparedSourceIds {
    bundle: SourceBundleId,
    revision: SourceRevisionId,
    units: Vec<SourceUnitId>,
}

struct CandidateMaterial {
    source: StoredSourceRevision,
    catalogue: CatalogueSnapshot,
    origins: Vec<DefinitionOrigin>,
    expressions: Vec<ExpressionArtifact>,
    current_function_revisions: Vec<FunctionRevisionRecord>,
    new_function_revisions: Vec<FunctionRevisionRecord>,
    references: Vec<DefinitionReference>,
}

impl AllocatedStandardUpgradeFunctionPlan {
    fn revision_id(&self) -> FunctionRevisionId {
        match &self.revision {
            AllocatedStandardUpgradeFunctionRevision::Reused(revision) => revision.id(),
            AllocatedStandardUpgradeFunctionRevision::New { id, .. } => *id,
        }
    }
}

impl AllocatedStandardUpgradePlan {
    /// Gate 8 constructs and validates only the candidate catalogue.
    fn into_catalogue(self) -> Result<StandardUpgradeCatalogueCandidate, CatalogueSnapshotError> {
        let functions = self
            .functions
            .iter()
            .map(|function| {
                rebind_function_definition_revision(&function.definition, function.revision_id())
            })
            .collect();
        let catalogue = CatalogueSnapshot::new_with_functions(
            self.catalogue_revision,
            self.schemas.clone(),
            self.object_types.clone(),
            functions,
        )?;
        Ok(StandardUpgradeCatalogueCandidate {
            plan: self,
            catalogue,
        })
    }
}

impl StandardUpgradeCatalogueCandidate {
    /// Gate 9 constructs every typed candidate record. It does not calculate
    /// a canonical hash.
    fn into_candidate_records(
        self,
    ) -> Result<StandardUpgradeCandidateRecords, RevisionInvariantError> {
        let StandardUpgradeCatalogueCandidate { plan, catalogue } = self;
        let PreparedSourceIds {
            bundle,
            revision,
            units: source_unit_ids,
        } = plan.source_ids;
        let units = plan
            .source_template
            .units()
            .iter()
            .zip(source_unit_ids)
            .map(|(template, id)| {
                StoredSourceUnit::new(
                    id,
                    template.ordinal(),
                    template.logical_path(),
                    template.content(),
                    template.content_hash(),
                )
            })
            .collect::<Result<Vec<_>, RevisionInvariantError>>()?;
        let zero_hash = Sha256Digest::from_bytes([0; 32]);
        let source = StoredSourceRevision::new(
            bundle,
            revision,
            Some(plan.source_template.id()),
            units,
            zero_hash,
            zero_hash,
        )?;

        let origins = plan
            .origin_templates
            .iter()
            .map(|origin| {
                Ok(DefinitionOrigin::new(
                    origin.identity(),
                    rebase_standard_upgrade_origin(
                        &plan.source_template,
                        &source,
                        origin.source(),
                    )?,
                ))
            })
            .collect::<Result<Vec<_>, RevisionInvariantError>>()?;
        let mut current_function_revisions = Vec::with_capacity(plan.functions.len());
        let mut new_function_revisions = Vec::new();
        let mut references = Vec::new();
        for function in plan.functions {
            let function_id = function.definition.id();
            let revision_id = function.revision_id();
            let current = match function.revision {
                AllocatedStandardUpgradeFunctionRevision::Reused(revision) => *revision,
                AllocatedStandardUpgradeFunctionRevision::New {
                    id,
                    revision_number,
                } => {
                    let declaration_origin = rebase_standard_upgrade_origin(
                        &plan.source_template,
                        &source,
                        function.declaration_origin,
                    )?;
                    let revision = FunctionRevisionRecord::new(
                        function_id,
                        id,
                        revision_number,
                        declaration_origin,
                        function.declaration_content_hash,
                        function.semantic_hash,
                        function.language_version,
                        function.artifact,
                    )?
                    .with_semantic_hash_version(function.semantic_hash_version);
                    new_function_revisions.push(revision.clone());
                    revision
                }
            };
            for reference in function.references {
                references.push(DefinitionReference::new(
                    function_id,
                    revision_id,
                    reference.ordinal(),
                    reference.target(),
                    reference.kind(),
                    rebase_standard_upgrade_origin(
                        &plan.source_template,
                        &source,
                        reference.source_origin(),
                    )?,
                ));
            }
            current_function_revisions.push(current);
        }
        Ok(StandardUpgradeCandidateRecords {
            source,
            catalogue,
            origins,
            expressions: plan.expressions,
            current_function_revisions,
            new_function_revisions,
            references,
        })
    }
}

impl StandardUpgradeCandidateRecords {
    /// Gate 10 is the only standard-upgrade canonical encoder authority.
    fn canonicalise(
        self,
        context: &CatalogueHashContext,
    ) -> Result<CanonicalStandardUpgradeCandidate, CanonicalHashError> {
        let source_bundle_hash = source_bundle_digest(self.source.units())?;
        let source_revision_hash = source_revision_record_digest(
            self.source.bundle(),
            self.source.parent(),
            source_bundle_hash,
        )?;
        let catalogue_hash = catalogue_digest_with_context(
            context,
            &self.catalogue,
            &self.current_function_revisions,
            &self.expressions,
            &self.origins,
            &self.references,
        )?;
        Ok(CanonicalStandardUpgradeCandidate {
            records: self,
            source_bundle_hash,
            source_revision_hash,
            catalogue_hash,
        })
    }
}

impl CanonicalStandardUpgradeCandidate {
    /// Gate 11 rebuilds the hashed source and constructs the final revision.
    fn into_deployable(
        self,
        active: &ActiveDatabaseRevision,
        context: CatalogueHashContext,
    ) -> Result<DeployableRevision, RevisionInvariantError> {
        let records = self.records;
        let source = StoredSourceRevision::new(
            records.source.bundle(),
            records.source.id(),
            records.source.parent(),
            records.source.units().to_vec(),
            self.source_bundle_hash,
            self.source_revision_hash,
        )?;
        DeployableRevision::new_with_catalogue_hash_context(
            DeployableRevisionInput::new(
                active.pair(),
                source,
                active.pair().catalogue(),
                records.catalogue,
                self.catalogue_hash,
                DeployableRevisionContent::new(
                    records.origins,
                    records.expressions,
                    records.new_function_revisions,
                    records.references,
                )
                .with_current_function_revisions(records.current_function_revisions),
            ),
            context,
        )
    }
}

fn rebase_standard_upgrade_origin(
    source_template: &StoredSourceRevision,
    source: &StoredSourceRevision,
    origin: SourceOrigin,
) -> Result<SourceOrigin, RevisionInvariantError> {
    let index = source_template
        .units()
        .iter()
        .position(|unit| unit.id() == origin.source_unit())
        .ok_or(RevisionInvariantError::SourceOriginUnitNotInRevision {
            source_unit: origin.source_unit(),
        })?;
    let source_unit =
        source
            .units()
            .get(index)
            .ok_or(RevisionInvariantError::SourceOriginUnitNotInRevision {
                source_unit: origin.source_unit(),
            })?;
    SourceOrigin::new(source_unit.id(), origin.byte_start(), origin.byte_end())
}

impl CandidateMaterial {
    fn matches_active(&self, active: &ActiveDatabaseRevision) -> Result<bool, PrepareError> {
        if self.source != *active.source()
            || !catalogue_matches(&self.catalogue, active.catalogue())
            || !same_member_multiset(&self.origins, active.origins())
            || !same_member_multiset(&self.expressions, active.expressions())
            || !same_member_multiset(
                &self.current_function_revisions,
                active.function_revisions(),
            )
            || !self.new_function_revisions.is_empty()
            || !same_member_multiset(&self.references, active.references())
        {
            return Ok(false);
        }
        Ok(catalogue_digest_with_context_and_parent(
            active.catalogue_hash_context(),
            &self.catalogue,
            &self.current_function_revisions,
            &self.expressions,
            &self.origins,
            &self.references,
            Some(active.catalogue()),
        )? == active.catalogue_hash())
    }

    fn into_deployable(
        self,
        active: &ActiveDatabaseRevision,
        context: CatalogueHashContext,
    ) -> Result<DeployableRevision, PrepareError> {
        let catalogue_hash = self.catalogue_hash_with_parent(&context, Some(active.catalogue()))?;
        self.into_deployable_with_catalogue_hash(active, context, catalogue_hash)
    }

    fn catalogue_hash_with_parent(
        &self,
        context: &CatalogueHashContext,
        parent: Option<&CatalogueSnapshot>,
    ) -> Result<Sha256Digest, PrepareError> {
        Ok(catalogue_digest_with_context_and_parent(
            context,
            &self.catalogue,
            &self.current_function_revisions,
            &self.expressions,
            &self.origins,
            &self.references,
            parent,
        )?)
    }

    fn into_deployable_with_catalogue_hash(
        self,
        active: &ActiveDatabaseRevision,
        context: CatalogueHashContext,
        catalogue_hash: Sha256Digest,
    ) -> Result<DeployableRevision, PrepareError> {
        if context.standard().is_none() {
            return Ok(
                DeployableRevision::new_with_catalogue_hash_context_and_parent(
                    DeployableRevisionInput::new(
                        active.pair(),
                        self.source,
                        active.pair().catalogue(),
                        self.catalogue,
                        catalogue_hash,
                        DeployableRevisionContent::new(
                            self.origins,
                            self.expressions,
                            self.new_function_revisions,
                            self.references,
                        ),
                    ),
                    context,
                    Some(active.catalogue()),
                )?,
            );
        }
        Ok(
            DeployableRevision::new_with_catalogue_hash_context_and_parent(
                DeployableRevisionInput::new(
                    active.pair(),
                    self.source,
                    active.pair().catalogue(),
                    self.catalogue,
                    catalogue_hash,
                    DeployableRevisionContent::new(
                        self.origins,
                        self.expressions,
                        self.new_function_revisions,
                        self.references,
                    )
                    .with_current_function_revisions(self.current_function_revisions),
                ),
                context,
                Some(active.catalogue()),
            )?,
        )
    }
}

fn catalogue_matches(left: &CatalogueSnapshot, right: &CatalogueSnapshot) -> bool {
    left.revision() == right.revision()
        && same_member_multiset(left.schemas(), right.schemas())
        && same_member_multiset(left.object_types(), right.object_types())
        && same_member_multiset(left.value_types(), right.value_types())
        && same_member_multiset(left.enum_types(), right.enum_types())
        && same_member_multiset(left.record_value_types(), right.record_value_types())
        && same_member_multiset(left.type_bindings(), right.type_bindings())
        && same_member_multiset(left.functions(), right.functions())
}

fn same_member_multiset<T: Eq>(left: &[T], right: &[T]) -> bool {
    left.len() == right.len()
        && left.iter().all(|member| {
            left.iter().filter(|candidate| *candidate == member).count()
                == right
                    .iter()
                    .filter(|candidate| *candidate == member)
                    .count()
        })
}

impl PreparedSourceIds {
    fn allocate(
        parse_report: &ParseReport,
        allocations: &mut CandidateAllocator,
    ) -> Result<Self, PrepareError> {
        let bundle = allocations.source_bundle();
        let revision = allocations.source_revision();
        let mut units = Vec::with_capacity(parse_report.units().len());
        for _ in parse_report.units() {
            units.push(allocations.source_unit());
        }
        Ok(Self {
            bundle,
            revision,
            units,
        })
    }
}

impl PreparedSource {
    fn from_active(revision: &StoredSourceRevision) -> Result<Self, PrepareError> {
        let mut unit_ids = HashMap::with_capacity(revision.units().len());
        for unit in revision.units() {
            if unit_ids
                .insert(unit.logical_path().to_owned(), unit.id())
                .is_some()
            {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "active source revision contains a duplicate logical path",
                });
            }
        }
        Ok(Self {
            revision: revision.clone(),
            unit_ids,
        })
    }

    fn new(
        parse_report: &ParseReport,
        parent: SourceRevisionId,
        allocations: &mut CandidateAllocator,
    ) -> Result<Self, PrepareError> {
        let ids = PreparedSourceIds::allocate(parse_report, allocations)?;
        Self::from_ids(parse_report, parent, ids)
    }

    fn from_ids(
        parse_report: &ParseReport,
        parent: SourceRevisionId,
        ids: PreparedSourceIds,
    ) -> Result<Self, PrepareError> {
        let PreparedSourceIds {
            bundle,
            revision: revision_id,
            units: allocated_units,
        } = ids;
        if allocated_units.len() != parse_report.units().len() {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "checked source bundle has inconsistent preallocated unit identities",
            });
        }
        let mut unit_ids = HashMap::new();
        let mut units = Vec::with_capacity(parse_report.units().len());
        for (ordinal, (unit, id)) in parse_report.units().iter().zip(allocated_units).enumerate() {
            if unit_ids
                .insert(unit.logical_path().to_owned(), id)
                .is_some()
            {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "checked source bundle contains a duplicate logical path",
                });
            }
            units.push(StoredSourceUnit::new(
                id,
                u32::try_from(ordinal).map_err(|_| PrepareError::SourceUnitCountExceedsU32 {
                    count: parse_report.units().len(),
                })?,
                unit.logical_path(),
                unit.source_text(),
                source_unit_content_digest(unit.source_text())?,
            )?);
        }
        let bundle_hash = source_bundle_digest(&units)?;
        let revision_hash = source_revision_record_digest(bundle, Some(parent), bundle_hash)?;
        let revision = StoredSourceRevision::new(
            bundle,
            revision_id,
            Some(parent),
            units,
            bundle_hash,
            revision_hash,
        )?;
        Ok(Self { revision, unit_ids })
    }

    fn origin(&self, location: &SourceLocation) -> Result<SourceOrigin, PrepareError> {
        let source_unit = self
            .unit_ids
            .get(location.logical_path())
            .copied()
            .ok_or_else(|| invalid_location(location))?;
        Ok(SourceOrigin::new(
            source_unit,
            u32::try_from(location.span().start()).map_err(|_| invalid_location(location))?,
            u32::try_from(location.span().end()).map_err(|_| invalid_location(location))?,
        )?)
    }

    fn declaration<'a>(
        &self,
        parse_report: &'a ParseReport,
        location: &SourceLocation,
    ) -> Result<&'a [u8], PrepareError> {
        let unit = parse_report
            .units()
            .iter()
            .find(|unit| unit.logical_path() == location.logical_path())
            .ok_or_else(|| invalid_location(location))?;
        unit.source_text()
            .as_bytes()
            .get(location.span().start()..location.span().end())
            .ok_or_else(|| invalid_location(location))
    }
}

struct CandidateBuilder<'a> {
    checked: &'a CheckedBundle,
    parse_report: &'a ParseReport,
    active: &'a ActiveDatabaseRevision,
    mode: PreparationMode<'a>,
    identities: IdentityMap,
    source: PreparedSource,
    catalogue_revision: CatalogueRevisionId,
    origins: Vec<DefinitionOrigin>,
    expressions: Vec<ExpressionArtifact>,
    functions: Vec<FunctionDefinition>,
    current_function_revisions: Vec<FunctionRevisionRecord>,
    new_function_revisions: Vec<FunctionRevisionRecord>,
    references: Vec<DefinitionReference>,
    declaration_evidence: Option<RefCell<DeclarationEvidence>>,
}

#[cfg(test)]
mod tests;
