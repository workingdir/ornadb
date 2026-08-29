//! Construction of complete durable revisions from successful compiler checks.

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

/// A compiler-prepared standard upgrade that remains non-installable until the
/// standard-library wrapper accepts it.
#[derive(Clone, Debug)]
pub struct PreparedStandardUpgrade {
    standard: crate::CheckedStandardLibrary,
    application: DeployableRevision,
}

impl PreparedStandardUpgrade {
    /// Returns the checked standard library used to prepare this upgrade.
    pub fn standard_library(&self) -> &crate::CheckedStandardLibrary {
        &self.standard
    }

    /// Returns the prepared application revision for normal kernel input.
    pub fn application_revision(&self) -> &DeployableRevision {
        &self.application
    }
}

/// One durable identity reserved for the checked standard-library upgrade.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StandardUpgradeIdentity {
    /// A standard-library revision identity.
    StandardLibraryRevision(StandardLibraryRevisionId),
    /// A catalogue revision identity.
    CatalogueRevision(CatalogueRevisionId),
    /// A source bundle identity.
    SourceBundle(SourceBundleId),
    /// A source revision identity.
    SourceRevision(SourceRevisionId),
    /// A source unit identity.
    SourceUnit(SourceUnitId),
    /// A schema identity.
    Schema(SchemaId),
    /// A type identity.
    Type(TypeId),
    /// A type-binding identity.
    TypeBinding(TypeBindingId),
}

/// A fail-closed error returned while preparing a checked standard upgrade.
#[non_exhaustive]
#[derive(Debug)]
pub enum PrepareStandardUpgradeError {
    /// The active revision already contains a standard library.
    StandardLibraryAlreadyInstalled {
        /// The installed standard-library revision.
        revision: StandardLibraryRevisionId,
    },
    /// The active catalogue owns the reserved standard namespace.
    NamespaceOccupied {
        /// The first conflicting semantic name.
        name: QualifiedSemanticName,
    },
    /// The active revision contains a reserved durable identity.
    ReservedIdentity {
        /// The first conflicting reserved identity.
        identity: StandardUpgradeIdentity,
    },
    /// The checked standard library cannot form application-checking context.
    Context {
        /// The application-context failure.
        source: StandardApplicationContextError,
    },
    /// The reconstructed active source has compiler diagnostics.
    ActiveSourceDiagnostics {
        /// The ordered active-source diagnostics.
        diagnostics: Vec<CompilerDiagnostic>,
    },
    /// The active source does not match its active catalogue.
    ActiveSourceMismatch,
    /// The next immutable function revision number cannot be allocated.
    FunctionRevisionNumberExhausted {
        /// The exhausted durable function identity.
        function: FunctionId,
    },
    /// The candidate catalogue is invalid.
    Catalogue {
        /// The catalogue validation failure.
        source: CatalogueSnapshotError,
    },
    /// The typed candidate records are invalid.
    CandidateRecords {
        /// The candidate-record invariant failure.
        source: RevisionInvariantError,
    },
    /// The candidate canonical hashes are invalid.
    CanonicalHash {
        /// The canonical-hash validation failure.
        source: CanonicalHashError,
    },
    /// The candidate revision invariants are invalid.
    Revision {
        /// The revision validation failure.
        source: RevisionInvariantError,
    },
}

impl fmt::Display for PrepareStandardUpgradeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StandardLibraryAlreadyInstalled { revision } => {
                write!(
                    formatter,
                    "standard library {revision} is already installed"
                )
            }
            Self::NamespaceOccupied { .. } => formatter
                .write_str("the application catalogue already uses the reserved std namespace"),
            Self::ReservedIdentity { .. } => formatter.write_str(
                "the application state conflicts with a reserved standard library identity",
            ),
            Self::Context { source } => write!(
                formatter,
                "the checked standard library cannot form an application context: {source}"
            ),
            Self::ActiveSourceDiagnostics { .. } => {
                formatter.write_str("the active application source has compiler diagnostics")
            }
            Self::ActiveSourceMismatch => formatter
                .write_str("the active application source does not match the active catalogue"),
            Self::FunctionRevisionNumberExhausted { .. } => {
                formatter.write_str("function revision number is exhausted")
            }
            Self::Catalogue { source } => {
                write!(
                    formatter,
                    "the standard upgrade catalogue is invalid: {source}"
                )
            }
            Self::CandidateRecords { source } => write!(
                formatter,
                "the standard upgrade candidate records are invalid: {source}"
            ),
            Self::CanonicalHash { source } => write!(
                formatter,
                "the standard upgrade canonical hashes are invalid: {source}"
            ),
            Self::Revision { source } => {
                write!(
                    formatter,
                    "the standard upgrade revision is invalid: {source}"
                )
            }
        }
    }
}

impl Error for PrepareStandardUpgradeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Context { source } => Some(source),
            Self::Catalogue { source } => Some(source),
            Self::CandidateRecords { source } => Some(source),
            Self::CanonicalHash { source } => Some(source),
            Self::Revision { source } => Some(source),
            Self::StandardLibraryAlreadyInstalled { .. }
            | Self::NamespaceOccupied { .. }
            | Self::ReservedIdentity { .. }
            | Self::ActiveSourceDiagnostics { .. }
            | Self::ActiveSourceMismatch
            | Self::FunctionRevisionNumberExhausted { .. } => None,
        }
    }
}

/// Prepares a checked standard library for a later standard-library upgrade.
pub fn prepare_checked_standard_upgrade(
    standard: &crate::CheckedStandardLibrary,
    active: &ActiveDatabaseRevision,
) -> Result<PreparedStandardUpgrade, PrepareStandardUpgradeError> {
    prepare_checked_standard_upgrade_with_allocator(
        standard,
        active,
        CandidateAllocator::standard(standard.verified_snapshot()),
    )
}

/// Runs the production upgrade pipeline with an injected private allocator.
///
/// The public entry point always supplies the reserved-aware random allocator;
/// compiler tests use this seam to prove that every gate before allocation is
/// allocation-free and that the final companion identities retry collisions.
/// Returns whether an installed standard admits the prepared standard as the
/// exact append-only source child (work ADR 0059).
///
/// The append-only upgrade rule admits exactly one base shape: the prepared
/// standard must be a different revision whose source revision descends
/// directly from the installed standard's source revision. Every other
/// installed base — a repeated install of the same revision, a prepared
/// standard with no parent or a different parent, or any other revision — is
/// not the append-only child and must close the upgrade.
fn admits_append_only_standard_child(
    installed_revision: StandardLibraryRevisionId,
    installed_source: SourceRevisionId,
    prepared_revision: StandardLibraryRevisionId,
    prepared_parent: Option<SourceRevisionId>,
) -> bool {
    prepared_revision != installed_revision && prepared_parent == Some(installed_source)
}

pub(crate) fn prepare_checked_standard_upgrade_with_allocator(
    standard: &crate::CheckedStandardLibrary,
    active: &ActiveDatabaseRevision,
    mut allocations: CandidateAllocator,
) -> Result<PreparedStandardUpgrade, PrepareStandardUpgradeError> {
    if let Some(installed) = active.catalogue_hash_context().standard() {
        let prepared = standard.verified_snapshot();
        if !admits_append_only_standard_child(
            installed.revision(),
            installed.source().id(),
            prepared.revision(),
            prepared.source().parent(),
        ) {
            return Err(
                PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled {
                    revision: installed.revision(),
                },
            );
        }
    }

    if let Some(name) = active_std_namespace_occupant(active.catalogue()) {
        return Err(PrepareStandardUpgradeError::NamespaceOccupied { name });
    }
    if let Some(identity) = active_reserved_standard_identity(standard, active) {
        return Err(PrepareStandardUpgradeError::ReservedIdentity { identity });
    }
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), standard)
        .map_err(|source| PrepareStandardUpgradeError::Context { source })?;
    let source = SourceBundle::new(
        active
            .source()
            .units()
            .iter()
            .map(|unit| SourceUnit::new(unit.logical_path(), unit.content())),
    )
    .map_err(|_| PrepareStandardUpgradeError::ActiveSourceMismatch)?;
    let report = check_standard_application(&source, &context);
    if !report.diagnostics().is_empty() {
        return Err(PrepareStandardUpgradeError::ActiveSourceDiagnostics {
            diagnostics: report.diagnostics().to_vec(),
        });
    }

    let matched = match_active_standard_source(report, active, standard)?;
    let Some(view) = matched.report.preparation_view() else {
        return Err(PrepareStandardUpgradeError::ActiveSourceMismatch);
    };

    let lowering_plan = CandidateBuilder::new(
        matched.report.parse_report(),
        view.checked(),
        active,
        matched.identities.clone(),
        PreparedSource::from_active(active.source())
            .map_err(|_| PrepareStandardUpgradeError::ActiveSourceMismatch)?,
        PreparationMode::StandardV2Plan {
            standard,
            declaration_evidence: matched.declaration_evidence.clone(),
            signature_evidence: matched.signature_evidence.clone(),
            standard_preflight: Box::new(matched.standard_preflight.clone()),
        },
        active.catalogue().revision(),
    )
    .plan_standard_upgrade_lowering()
    .map_err(map_upgrade_revision_plan_error)?;

    let allocated = allocate_standard_upgrade_plan(lowering_plan, &mut allocations)?;
    let candidate = allocated
        .into_catalogue()
        .map_err(|source| PrepareStandardUpgradeError::Catalogue { source })?;
    let records = candidate
        .into_candidate_records()
        .map_err(|source| PrepareStandardUpgradeError::CandidateRecords { source })?;
    let context = CatalogueHashContext::version_two(standard.verified_snapshot().clone());
    let canonical = records
        .canonicalise(&context)
        .map_err(|source| PrepareStandardUpgradeError::CanonicalHash { source })?;
    let application = canonical
        .into_deployable(active, context)
        .map_err(|source| PrepareStandardUpgradeError::Revision { source })?;
    Ok(PreparedStandardUpgrade {
        standard: standard.clone(),
        application,
    })
}

fn allocate_standard_upgrade_plan(
    plan: StandardUpgradeLoweringPlan,
    allocations: &mut CandidateAllocator,
) -> Result<AllocatedStandardUpgradePlan, PrepareStandardUpgradeError> {
    for function in &plan.functions {
        match (
            function.revision.reusable.is_some(),
            function.revision.next_revision_number.is_some(),
        ) {
            (true, false) | (false, true) => {}
            (true, true) | (false, false) => {
                return Err(PrepareStandardUpgradeError::ActiveSourceMismatch);
            }
        }
    }

    let mut functions = Vec::with_capacity(plan.functions.len());
    for function in plan.functions {
        let FunctionRevisionPlan {
            definition,
            semantic_hash_version,
            semantic_hash,
            language_version,
            artifact,
            references,
            reusable,
            next_revision_number,
        } = function.revision;
        let revision = match (reusable, next_revision_number) {
            (Some(revision), None) => {
                AllocatedStandardUpgradeFunctionRevision::Reused(Box::new(revision))
            }
            (None, Some(revision_number)) => AllocatedStandardUpgradeFunctionRevision::New {
                id: allocations.function_revision(),
                revision_number,
            },
            (Some(_), Some(_)) | (None, None) => {
                return Err(PrepareStandardUpgradeError::ActiveSourceMismatch);
            }
        };
        functions.push(AllocatedStandardUpgradeFunctionPlan {
            definition,
            semantic_hash_version,
            semantic_hash,
            language_version,
            artifact,
            references,
            declaration_origin: function.declaration_origin,
            declaration_content_hash: function.declaration_content_hash,
            revision,
        });
    }

    let catalogue_revision = allocations.catalogue_revision();
    let source_ids = PreparedSourceIds {
        bundle: allocations.source_bundle(),
        revision: allocations.source_revision(),
        units: plan
            .source_template
            .units()
            .iter()
            .map(|_| allocations.source_unit())
            .collect(),
    };
    Ok(AllocatedStandardUpgradePlan {
        source_template: plan.source_template,
        source_ids,
        catalogue_revision,
        schemas: plan.schemas,
        object_types: plan.object_types,
        expressions: plan.expressions,
        origin_templates: plan.origin_templates,
        functions,
    })
}

/// Gate-six output. This keeps the single standard resolver result, its exact
/// sealed evidence, and the allocation-free active identity reconciliation
/// together until the V2 candidate consumes them.
struct MatchedActiveStandardSource {
    report: StandardApplicationCheckReport,
    declaration_evidence: DeclarationEvidence,
    signature_evidence: SignatureEvidence,
    standard_preflight: StandardPreflight,
    identities: IdentityMap,
}

fn match_active_standard_source(
    report: StandardApplicationCheckReport,
    active: &ActiveDatabaseRevision,
    standard: &crate::CheckedStandardLibrary,
) -> Result<MatchedActiveStandardSource, PrepareStandardUpgradeError> {
    let Some(view) = report.preparation_view() else {
        return Err(PrepareStandardUpgradeError::ActiveSourceMismatch);
    };
    let evidence = view.evidence();
    let declaration_evidence = declaration_type_evidence(evidence.declaration_uses(), view.uses())
        .map_err(|_| PrepareStandardUpgradeError::ActiveSourceMismatch)?;
    body_type_evidence(evidence.type_uses(), view.uses())
        .map_err(|_| PrepareStandardUpgradeError::ActiveSourceMismatch)?;
    let signature_evidence = function_type_reference_evidence(
        &declaration_evidence,
        evidence.standard_type_references(),
        view.standard_type_references(),
    )
    .map_err(|_| PrepareStandardUpgradeError::ActiveSourceMismatch)?;
    let client_returns = signature_evidence
        .materialise_client_returns(view.checked())
        .map_err(|_| PrepareStandardUpgradeError::ActiveSourceMismatch)?;
    let standard_preflight = standard_preflight(
        report.parse_report(),
        view.checked(),
        active,
        standard,
        &client_returns,
        &declaration_evidence,
    )
    .map_err(|_| PrepareStandardUpgradeError::ActiveSourceMismatch)?;
    let identities = IdentityMap::build_matching_active(
        view.checked(),
        active,
        &standard_preflight.function_identities,
    )
    .map_err(|_| PrepareStandardUpgradeError::ActiveSourceMismatch)?;
    let material = CandidateBuilder::new(
        report.parse_report(),
        view.checked(),
        active,
        identities.clone(),
        PreparedSource::from_active(active.source())
            .map_err(|_| PrepareStandardUpgradeError::ActiveSourceMismatch)?,
        PreparationMode::StandardV1Match {
            declaration_evidence: declaration_evidence.clone(),
            standard_preflight: Box::new(standard_preflight.clone()),
        },
        active.catalogue().revision(),
    )
    .materialise()
    .map_err(|_| PrepareStandardUpgradeError::ActiveSourceMismatch)?;
    if !material
        .matches_active(active)
        .map_err(|_| PrepareStandardUpgradeError::ActiveSourceMismatch)?
    {
        return Err(PrepareStandardUpgradeError::ActiveSourceMismatch);
    }
    Ok(MatchedActiveStandardSource {
        report,
        declaration_evidence,
        signature_evidence,
        standard_preflight,
        identities,
    })
}

fn map_upgrade_revision_plan_error(error: PrepareError) -> PrepareStandardUpgradeError {
    match error {
        PrepareError::FunctionRevisionNumberExhausted { function } => {
            PrepareStandardUpgradeError::FunctionRevisionNumberExhausted { function }
        }
        _ => PrepareStandardUpgradeError::ActiveSourceMismatch,
    }
}

fn active_std_namespace_occupant(catalogue: &CatalogueSnapshot) -> Option<QualifiedSemanticName> {
    catalogue
        .schemas()
        .iter()
        .map(SchemaDefinition::name)
        .chain(
            catalogue
                .object_types()
                .iter()
                .map(ObjectTypeDefinition::name),
        )
        .chain(catalogue.value_types().iter().map(|value| value.name()))
        .chain(
            catalogue
                .enum_types()
                .iter()
                .map(|enum_type| enum_type.name()),
        )
        .chain(
            catalogue
                .record_value_types()
                .iter()
                .map(|record_value_type| record_value_type.name()),
        )
        .find(|name| name_starts_std(name))
        .cloned()
        .or_else(|| {
            catalogue.type_bindings().iter().find_map(|binding| {
                let orna_core::catalogue::TypeLookupName::Qualified(name) = binding.name() else {
                    return None;
                };
                name_starts_std(name).then(|| name.clone())
            })
        })
        .or_else(|| {
            catalogue.functions().iter().find_map(|function| {
                name_starts_std(function.name()).then(|| function.name().clone())
            })
        })
}

fn name_starts_std(name: &QualifiedSemanticName) -> bool {
    name.parts().first().is_some_and(|part| part == "std")
}

pub(crate) fn active_reserved_standard_identity(
    standard: &crate::CheckedStandardLibrary,
    active: &ActiveDatabaseRevision,
) -> Option<StandardUpgradeIdentity> {
    let snapshot = standard.verified_snapshot();
    let catalogue = active.catalogue();
    if catalogue.revision() == snapshot.catalogue().revision() {
        return Some(StandardUpgradeIdentity::CatalogueRevision(
            catalogue.revision(),
        ));
    }
    if active.source().bundle() == snapshot.source().bundle() {
        return Some(StandardUpgradeIdentity::SourceBundle(
            active.source().bundle(),
        ));
    }
    if active.source().id() == snapshot.source().id() {
        return Some(StandardUpgradeIdentity::SourceRevision(
            active.source().id(),
        ));
    }
    for unit in active.source().units() {
        if snapshot
            .source()
            .units()
            .iter()
            .any(|reserved| reserved.id() == unit.id())
        {
            return Some(StandardUpgradeIdentity::SourceUnit(unit.id()));
        }
    }
    for schema in catalogue.schemas() {
        if snapshot
            .catalogue()
            .schemas()
            .iter()
            .any(|reserved| reserved.id() == schema.id())
        {
            return Some(StandardUpgradeIdentity::Schema(schema.id()));
        }
    }
    for object in catalogue.object_types() {
        if snapshot
            .catalogue()
            .object_types()
            .iter()
            .any(|reserved| reserved.id() == object.id())
            || snapshot
                .catalogue()
                .value_types()
                .iter()
                .any(|reserved| reserved.id() == object.id())
        {
            return Some(StandardUpgradeIdentity::Type(object.id()));
        }
    }
    for value in catalogue.value_types() {
        if snapshot
            .catalogue()
            .object_types()
            .iter()
            .any(|reserved| reserved.id() == value.id())
            || snapshot
                .catalogue()
                .value_types()
                .iter()
                .any(|reserved| reserved.id() == value.id())
        {
            return Some(StandardUpgradeIdentity::Type(value.id()));
        }
    }
    for enum_type in catalogue.enum_types() {
        if snapshot
            .catalogue()
            .object_types()
            .iter()
            .any(|reserved| reserved.id() == enum_type.id())
            || snapshot
                .catalogue()
                .value_types()
                .iter()
                .any(|reserved| reserved.id() == enum_type.id())
        {
            return Some(StandardUpgradeIdentity::Type(enum_type.id()));
        }
    }
    for record_value_type in catalogue.record_value_types() {
        if snapshot
            .catalogue()
            .type_definition_by_id(record_value_type.id())
            .is_some()
        {
            return Some(StandardUpgradeIdentity::Type(record_value_type.id()));
        }
    }
    for binding in catalogue.type_bindings() {
        if snapshot
            .catalogue()
            .type_bindings()
            .iter()
            .any(|reserved| reserved.id() == binding.id())
        {
            return Some(StandardUpgradeIdentity::TypeBinding(binding.id()));
        }
    }
    None
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
            Self::Generic | Self::LegacyV1 | Self::StandardV1Match { .. } => CatalogueHashContext::version_one(),
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

fn server_mutation_plan(
    plan: &MutationPlanIr<TypeId, FieldId, FunctionId, ParameterId>,
    function: &FunctionDefinition,
    object_types: &[ObjectTypeDefinition],
    enum_types: &[EnumTypeDefinition],
    record_value_types: &[RecordValueTypeDefinition],
    standard: Option<&CatalogueSnapshot>,
    references: &[(DefinitionReferenceKind, DefinitionReferenceTarget)],
) -> Result<ServerMutationPlan, PrepareError> {
    let target = object_types
        .iter()
        .find(|object_type| object_type.id() == plan.target_object())
        .ok_or(PrepareError::InvalidCheckedBundle {
            reason: "mutation target object is absent from the candidate catalogue",
        })?;
    if plan.target_object() != plan.returned_object() {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "mutation returned object differs from its target object",
        });
    }

    validate_mutation_parameters_with_catalogue(function, object_types, enum_types, standard)?;
    let assignments = validate_mutation_assignments_with_catalogue(
        plan.assignments(),
        target,
        function,
        enum_types,
        record_value_types,
        standard,
        matches!(plan.operation(), MutationOperation::Insert),
    )?;
    validate_reference_sequence(
        &mutation_reference_sequence(plan, function),
        references,
        "mutation definition references differ from the checked body",
    )?;
    Ok(match plan.operation() {
        MutationOperation::Insert => ServerMutationPlan::new_insert(
            plan.target_object(),
            assignments,
            plan.returned_object(),
        )?,
        MutationOperation::Update {
            selector_owner,
            selector_parameter,
        } => {
            validate_mutation_selector(
                *selector_owner,
                *selector_parameter,
                plan.target_object(),
                function,
            )?;
            ServerMutationPlan::new_update(
                plan.target_object(),
                MutationSelector::new(*selector_owner, *selector_parameter),
                assignments,
                plan.returned_object(),
            )?
        }
    })
}

/// Adapts scalar and STREAM functions to the row-shaped view required by the
/// relational artifact validators. The durable catalogue keeps their exact
/// return shape.
fn query_planning_function(function: &FunctionDefinition) -> FunctionDefinition {
    let result_type = match function.return_type() {
        FunctionReturn::Single(result_type) | FunctionReturn::Stream(result_type) => result_type,
        FunctionReturn::Rows(_) => return function.clone(),
    };
    FunctionDefinition::new(
        function.id(),
        function.name().clone(),
        function.domain(),
        function.parameters().to_vec(),
        FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
            "value",
            0,
            *result_type,
        )]),
        function.current_revision(),
        function.security(),
        function.transaction(),
        function.volatility(),
    )
}

fn identity_selected_query_plan(
    plan: &crate::relational::IdentitySelectedQueryIr<TypeId, FieldId, FunctionId, ParameterId>,
    function: &FunctionDefinition,
    object_types: &[ObjectTypeDefinition],
    references: &[(DefinitionReferenceKind, DefinitionReferenceTarget)],
) -> Result<crate::relational::EncodedServerPlan, PrepareError> {
    let scan = object_types
        .iter()
        .find(|object_type| object_type.id() == plan.scan().object_type())
        .ok_or(PrepareError::InvalidCheckedBundle {
            reason: "identity-selected query scan object is absent from the candidate catalogue",
        })?;
    if function.domain() != FunctionDomain::Server
        || function.security() != FunctionSecurity::Invoker
        || function.transaction() != Some(FunctionTransaction::ReadOnly)
        || function.volatility() != FunctionVolatility::Stable
    {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "identity-selected query function has unsupported execution modes",
        });
    }
    let FunctionReturn::Rows(columns) = function.return_type() else {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "identity-selected query function does not return ROWS",
        });
    };
    if function.parameters().len() != 1 {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "identity-selected query function does not declare exactly one parameter",
        });
    }
    let selector = function.parameters()[0].clone();
    if selector.default_expression().is_some() {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "identity-selected query selector parameter has an unsupported default expression",
        });
    }
    if selector.resolved_type() != ResolvedType::reference(scan.id()) {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "identity-selected query selector parameter does not reference its scan object",
        });
    }
    if plan.selector().owner() != function.id() {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "identity-selected query selector owner differs from its enclosing function",
        });
    }
    if plan.selector().parameter() != selector.id() {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "identity-selected query selector parameter is not its enclosing function parameter",
        });
    }
    if columns.len() != plan.projections().len() {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "identity-selected query projection count differs from its function return",
        });
    }
    for (projection, column) in plan.projections().iter().zip(columns) {
        validate_query_expression_facts(
            projection,
            scan,
            plan.scan().input(),
            object_types,
            IDENTITY_SELECTED_QUERY_FACTS,
        )?;
        let value_type = projection.value_type();
        if resolved_type_from_semantic(value_type.semantic_type()) != column.resolved_type() {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "identity-selected query projection differs from its function return",
            });
        }
    }
    validate_reference_sequence(
        &identity_selected_query_reference_sequence(plan, function),
        references,
        "parameterised SELECT definition references differ from the checked function body",
    )?;
    plan.encode_identity_selected_server_plan()
        .map_err(PrepareError::from)
}

fn unique_text_selected_query_plan(
    plan: &crate::relational::UniqueTextSelectedQueryIr<TypeId, FieldId, FunctionId, ParameterId>,
    function: &FunctionDefinition,
    object_types: &[ObjectTypeDefinition],
    standard: Option<&CatalogueSnapshot>,
    references: &[(DefinitionReferenceKind, DefinitionReferenceTarget)],
) -> Result<crate::relational::EncodedServerPlan, PrepareError> {
    let scan = object_types
        .iter()
        .find(|object_type| object_type.id() == plan.scan().object_type())
        .ok_or(PrepareError::InvalidCheckedBundle {
            reason: "unique-Text-selected query scan object is absent from the candidate catalogue",
        })?;
    if function.domain() != FunctionDomain::Server
        || function.security() != FunctionSecurity::Invoker
        || function.transaction() != Some(FunctionTransaction::ReadOnly)
        || function.volatility() != FunctionVolatility::Stable
    {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "unique-Text-selected query function has unsupported execution modes",
        });
    }
    let FunctionReturn::Rows(columns) = function.return_type() else {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "unique-Text-selected query function does not return ROWS",
        });
    };
    if columns.is_empty() {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "unique-Text-selected query function returns empty ROWS",
        });
    }
    if function.parameters().len() != 1 {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "unique-Text-selected query function does not declare exactly one parameter",
        });
    }
    let parameter = &function.parameters()[0];
    let selector = plan.selector();
    if parameter.default_expression().is_some()
        || !selector.parameter_required_non_null()
        || selector.parameter_owner() != function.id()
        || selector.parameter() != parameter.id()
    {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "unique-Text-selected query selector parameter differs from its enclosing function parameter",
        });
    }
    if selector.scan_object_type() != scan.id() || selector.field_owner() != scan.id() {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "unique-Text-selected query selector object identities differ from its scan",
        });
    }
    let field = scan
        .field_by_id(selector.field())
        .ok_or(PrepareError::InvalidCheckedBundle {
            reason: "unique-Text-selected query selector field is absent from its scan object",
        })?;
    let selector_type = selector
        .text_type()
        .standard_value_type()
        .map(ResolvedType::value)
        .unwrap_or_else(|| resolved_type_from_semantic(selector.text_type().semantic_type()));
    let compatibility_selector_type =
        resolved_type_from_semantic(selector.text_type().semantic_type());
    if !field.unique()
        || field.resolved_type() != compatibility_selector_type
        || field.nullable() != selector.field_nullable()
        || parameter.resolved_type() != compatibility_selector_type
        || !supports_durable_unique_field(selector_type, selector.field_nullable(), standard)
    {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "unique-Text-selected query selector does not retain exact unique Text authority",
        });
    }
    if columns.len() != plan.projections().len() {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "unique-Text-selected query projection count differs from its function return",
        });
    }
    for (projection, column) in plan.projections().iter().zip(columns) {
        validate_query_expression_facts(
            projection,
            scan,
            plan.scan().input(),
            object_types,
            UNIQUE_TEXT_SELECTED_QUERY_FACTS,
        )?;
        if resolved_type_from_semantic(projection.value_type().semantic_type())
            != column.resolved_type()
        {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "unique-Text-selected query projection differs from its function return",
            });
        }
    }
    validate_reference_sequence(
        &unique_text_selected_query_reference_sequence(plan, function),
        references,
        "unique-Text-selected SELECT definition references differ from the checked function body",
    )?;
    plan.encode_unique_text_selected_server_plan()
        .map_err(PrepareError::from)
}

fn version_one_query_plan(
    plan: &crate::relational::RelationalQueryIr<TypeId, FieldId>,
    function: &FunctionDefinition,
    object_types: &[ObjectTypeDefinition],
    references: &[(DefinitionReferenceKind, DefinitionReferenceTarget)],
) -> Result<Vec<u8>, PrepareError> {
    let scan = object_types
        .iter()
        .find(|object_type| object_type.id() == plan.scan().object_type())
        .ok_or(PrepareError::InvalidCheckedBundle {
            reason: "SERVER SELECT query scan object is absent from the candidate catalogue",
        })?;
    if function.domain() != FunctionDomain::Server
        || function.security() != FunctionSecurity::Invoker
        || !matches!(
            function.transaction(),
            None | Some(FunctionTransaction::Atomic | FunctionTransaction::ReadOnly)
        )
    {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "SERVER SELECT query function has unsupported execution modes",
        });
    }
    if !function.parameters().is_empty() {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "SERVER SELECT query function declares parameters",
        });
    }
    let FunctionReturn::Rows(columns) = function.return_type() else {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "SERVER SELECT query function does not return ROWS",
        });
    };
    if columns.is_empty() {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "SERVER SELECT query function returns empty ROWS",
        });
    }
    if columns.len() != plan.projections().len() {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "SERVER SELECT query projection count differs from its function return",
        });
    }
    for (projection, column) in plan.projections().iter().zip(columns) {
        validate_query_expression_facts(
            projection,
            scan,
            plan.scan().input(),
            object_types,
            VERSION_ONE_QUERY_FACTS,
        )?;
        if resolved_type_from_semantic(projection.value_type().semantic_type())
            != column.resolved_type()
        {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "SERVER SELECT query projection differs from its function return",
            });
        }
    }
    if let Some(selection) = plan.selection() {
        validate_query_expression_facts(
            selection,
            scan,
            plan.scan().input(),
            object_types,
            VERSION_ONE_QUERY_FACTS,
        )?;
        if selection.value_type().semantic_type() != SemanticType::Scalar(StandardScalar::Boolean) {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "SERVER SELECT query selection is not BOOLEAN",
            });
        }
    }
    for ordering in plan.ordering() {
        validate_query_expression_facts(
            ordering.expression(),
            scan,
            plan.scan().input(),
            object_types,
            VERSION_ONE_QUERY_FACTS,
        )?;
    }
    validate_reference_sequence(
        &version_one_query_reference_sequence(plan, function),
        references,
        "SERVER SELECT definition references differ from the checked function body",
    )?;
    plan.encode_server_plan().map_err(PrepareError::from)
}

fn distinct_query_plan(
    plan: &crate::relational::DistinctQueryIr<TypeId, FieldId>,
    function: &FunctionDefinition,
    object_types: &[ObjectTypeDefinition],
    references: &[(DefinitionReferenceKind, DefinitionReferenceTarget)],
) -> Result<crate::relational::EncodedServerPlan, PrepareError> {
    let scan = object_types
        .iter()
        .find(|object_type| object_type.id() == plan.scan().object_type())
        .ok_or(PrepareError::InvalidCheckedBundle {
            reason: "SELECT DISTINCT query scan object is absent from the candidate catalogue",
        })?;
    if function.domain() != FunctionDomain::Server
        || function.security() != FunctionSecurity::Invoker
        || function.transaction() != Some(FunctionTransaction::ReadOnly)
        || function.volatility() != FunctionVolatility::Stable
    {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "SELECT DISTINCT query function has unsupported execution modes",
        });
    }
    if !function.parameters().is_empty() {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "SELECT DISTINCT query function declares parameters",
        });
    }
    let FunctionReturn::Rows(columns) = function.return_type() else {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "SELECT DISTINCT query function does not return ROWS",
        });
    };
    if columns.is_empty() {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "SELECT DISTINCT query function returns empty ROWS",
        });
    }
    if columns.len() != plan.projections().len() {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "SELECT DISTINCT query projection count differs from its function return",
        });
    }
    for (projection, column) in plan.projections().iter().zip(columns) {
        validate_query_expression_facts(
            projection,
            scan,
            plan.scan().input(),
            object_types,
            DISTINCT_QUERY_FACTS,
        )?;
        if !supports_server_select_distinct(projection.value_type().semantic_type()) {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "SELECT DISTINCT query projection has an unsupported type",
            });
        }
        if resolved_type_from_semantic(projection.value_type().semantic_type())
            != column.resolved_type()
        {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "SELECT DISTINCT query projection differs from its function return",
            });
        }
    }
    if let Some(selection) = plan.selection() {
        validate_query_expression_facts(
            selection,
            scan,
            plan.scan().input(),
            object_types,
            DISTINCT_QUERY_FACTS,
        )?;
        if selection.value_type().semantic_type() != SemanticType::Scalar(StandardScalar::Boolean) {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "SELECT DISTINCT query selection is not BOOLEAN",
            });
        }
    }
    validate_reference_sequence(
        &distinct_query_reference_sequence(plan, function),
        references,
        "SELECT DISTINCT definition references differ from the checked function body",
    )?;
    plan.encode_distinct_server_plan()
        .map_err(PrepareError::from)
}

#[derive(Clone, Copy)]
struct QueryExpressionFactAdapter {
    object_reference: &'static str,
    field_path_input: &'static str,
    field_path_owner: &'static str,
    field_path_field: &'static str,
    field_path_type: &'static str,
    field_path_continuation: &'static str,
    field_path_target: &'static str,
    boolean: &'static str,
    equality: &'static str,
    require_final_reference_target: bool,
}

const IDENTITY_SELECTED_QUERY_FACTS: QueryExpressionFactAdapter = QueryExpressionFactAdapter {
    object_reference: "identity-selected query object reference has inconsistent facts",
    field_path_input: "identity-selected query field path has an invalid input or is empty",
    field_path_owner: "identity-selected query field path owner differs from its source object",
    field_path_field: "identity-selected query field path field is absent from its source object",
    field_path_type: "identity-selected query field path type differs from its source field",
    field_path_continuation: "identity-selected query field path continues through a non-reference field",
    field_path_target: "identity-selected query field path target is absent from the candidate catalogue",
    boolean: "identity-selected query BOOLEAN expression has inconsistent type facts",
    equality: "identity-selected query equality expression has inconsistent type facts",
    require_final_reference_target: false,
};

const UNIQUE_TEXT_SELECTED_QUERY_FACTS: QueryExpressionFactAdapter = QueryExpressionFactAdapter {
    object_reference: "unique-Text-selected query object reference has inconsistent facts",
    field_path_input: "unique-Text-selected query field path has an invalid input or is empty",
    field_path_owner: "unique-Text-selected query field path owner differs from its source object",
    field_path_field: "unique-Text-selected query field path field is absent from its source object",
    field_path_type: "unique-Text-selected query field path type differs from its source field",
    field_path_continuation: "unique-Text-selected query field path continues through a non-reference field",
    field_path_target: "unique-Text-selected query field path target is absent from the candidate catalogue",
    boolean: "unique-Text-selected query BOOLEAN expression has inconsistent type facts",
    equality: "unique-Text-selected query equality expression has inconsistent type facts",
    require_final_reference_target: true,
};

const DISTINCT_QUERY_FACTS: QueryExpressionFactAdapter = QueryExpressionFactAdapter {
    object_reference: "SELECT DISTINCT query object reference has inconsistent facts",
    field_path_input: "SELECT DISTINCT query field path has an invalid input or is empty",
    field_path_owner: "SELECT DISTINCT query field path owner differs from its source object",
    field_path_field: "SELECT DISTINCT query field path field is absent from its source object",
    field_path_type: "SELECT DISTINCT query field path type differs from its source field",
    field_path_continuation: "SELECT DISTINCT query field path continues through a non-reference field",
    field_path_target: "SELECT DISTINCT query field path target is absent from the candidate catalogue",
    boolean: "SELECT DISTINCT query BOOLEAN expression has inconsistent type facts",
    equality: "SELECT DISTINCT query equality expression has inconsistent type facts",
    require_final_reference_target: true,
};

const VERSION_ONE_QUERY_FACTS: QueryExpressionFactAdapter = QueryExpressionFactAdapter {
    object_reference: "SERVER SELECT query object reference has inconsistent facts",
    field_path_input: "SERVER SELECT query field path has an invalid input or is empty",
    field_path_owner: "SERVER SELECT query field path owner differs from its source object",
    field_path_field: "SERVER SELECT query field path field is absent from its source object",
    field_path_type: "SERVER SELECT query field path type differs from its source field",
    field_path_continuation: "SERVER SELECT query field path continues through a non-reference field",
    field_path_target: "SERVER SELECT query field path target is absent from the candidate catalogue",
    boolean: "SERVER SELECT query BOOLEAN expression has inconsistent type facts",
    equality: "SERVER SELECT query equality expression has inconsistent type facts",
    require_final_reference_target: true,
};

fn validate_query_expression_facts(
    expression: &crate::relational::ExpressionIr<TypeId, FieldId>,
    scan: &ObjectTypeDefinition,
    scan_input: crate::relational::InputSlot,
    object_types: &[ObjectTypeDefinition],
    facts: QueryExpressionFactAdapter,
) -> Result<(), PrepareError> {
    use crate::relational::ExpressionKind;

    match expression.kind() {
        ExpressionKind::ObjectReference { input } => {
            if *input != scan_input
                || expression.value_type().semantic_type() != SemanticType::reference(scan.id())
                || expression.value_type().nullable()
            {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: facts.object_reference,
                });
            }
        }
        ExpressionKind::FieldPath { input, steps } => {
            if *input != scan_input || steps.is_empty() {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: facts.field_path_input,
                });
            }
            let mut owner = scan;
            let mut nullable = false;
            for (index, step) in steps.iter().enumerate() {
                if step.owner() != owner.id() {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: facts.field_path_owner,
                    });
                }
                let field =
                    owner
                        .field_by_id(step.field())
                        .ok_or(PrepareError::InvalidCheckedBundle {
                            reason: facts.field_path_field,
                        })?;
                nullable |= field.nullable();
                if index + 1 == steps.len() {
                    let matching_type = match (
                        field.resolved_type(),
                        expression.value_type().semantic_type(),
                    ) {
                        (ResolvedType::Scalar(left), SemanticType::Scalar(right)) => left == right,
                        (ResolvedType::Named(left), SemanticType::Named(right)) => left == right,
                        (
                            ResolvedType::Reference { target: left },
                            SemanticType::Reference { target: right },
                        ) => left == right,
                        _ => false,
                    };
                    if !matching_type || expression.value_type().nullable() != nullable {
                        return Err(PrepareError::InvalidCheckedBundle {
                            reason: facts.field_path_type,
                        });
                    }
                    if facts.require_final_reference_target
                        && matches!(field.resolved_type(), ResolvedType::Reference { target } if !object_types.iter().any(|candidate| candidate.id() == target))
                    {
                        return Err(PrepareError::InvalidCheckedBundle {
                            reason: facts.field_path_target,
                        });
                    }
                } else {
                    let ResolvedType::Reference { target } = field.resolved_type() else {
                        return Err(PrepareError::InvalidCheckedBundle {
                            reason: facts.field_path_continuation,
                        });
                    };
                    owner = object_types
                        .iter()
                        .find(|candidate| candidate.id() == target)
                        .ok_or(PrepareError::InvalidCheckedBundle {
                            reason: facts.field_path_target,
                        })?;
                }
            }
        }
        ExpressionKind::BooleanLiteral { .. } => {
            if expression.value_type().semantic_type()
                != SemanticType::Scalar(StandardScalar::Boolean)
                || expression.value_type().nullable()
            {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: facts.boolean,
                });
            }
        }
        ExpressionKind::Equality { left, right } => {
            validate_query_expression_facts(left, scan, scan_input, object_types, facts)?;
            validate_query_expression_facts(right, scan, scan_input, object_types, facts)?;
            if left.value_type().semantic_type() != right.value_type().semantic_type()
                || !supports_server_select_equality(left.value_type().semantic_type())
                || expression.value_type().semantic_type()
                    != SemanticType::Scalar(StandardScalar::Boolean)
                || expression.value_type().nullable()
                    != (left.value_type().nullable() || right.value_type().nullable())
            {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: facts.equality,
                });
            }
        }
    }
    Ok(())
}

fn identity_selected_query_reference_sequence(
    plan: &crate::relational::IdentitySelectedQueryIr<TypeId, FieldId, FunctionId, ParameterId>,
    function: &FunctionDefinition,
) -> Vec<(DefinitionReferenceKind, DefinitionReferenceTarget)> {
    let mut references = signature_reference_sequence(function);
    references.push((
        DefinitionReferenceKind::QueryObject,
        DefinitionReferenceTarget::ObjectType(plan.scan().object_type()),
    ));
    for projection in plan.projections() {
        query_expression_references(projection, plan.scan().object_type(), &mut references);
    }
    references.extend([
        (
            DefinitionReferenceKind::ObjectReference,
            DefinitionReferenceTarget::ObjectType(plan.scan().object_type()),
        ),
        (
            DefinitionReferenceKind::ParameterRead,
            DefinitionReferenceTarget::Parameter {
                owner: plan.selector().owner(),
                parameter: plan.selector().parameter(),
            },
        ),
    ]);
    references
}

fn unique_text_selected_query_reference_sequence(
    plan: &crate::relational::UniqueTextSelectedQueryIr<TypeId, FieldId, FunctionId, ParameterId>,
    function: &FunctionDefinition,
) -> Vec<(DefinitionReferenceKind, DefinitionReferenceTarget)> {
    let mut references = signature_reference_sequence(function);
    references.push((
        DefinitionReferenceKind::QueryObject,
        DefinitionReferenceTarget::ObjectType(plan.scan().object_type()),
    ));
    for projection in plan.projections() {
        query_expression_references(projection, plan.scan().object_type(), &mut references);
    }
    references.extend([
        (
            DefinitionReferenceKind::QueryField,
            DefinitionReferenceTarget::Field {
                owner: plan.selector().field_owner(),
                field: plan.selector().field(),
            },
        ),
        (
            DefinitionReferenceKind::ParameterRead,
            DefinitionReferenceTarget::Parameter {
                owner: plan.selector().parameter_owner(),
                parameter: plan.selector().parameter(),
            },
        ),
    ]);
    references
}

fn distinct_query_reference_sequence(
    plan: &crate::relational::DistinctQueryIr<TypeId, FieldId>,
    function: &FunctionDefinition,
) -> Vec<(DefinitionReferenceKind, DefinitionReferenceTarget)> {
    let mut references = signature_reference_sequence(function);
    references.push((
        DefinitionReferenceKind::QueryObject,
        DefinitionReferenceTarget::ObjectType(plan.scan().object_type()),
    ));
    for projection in plan.projections() {
        query_expression_references(projection, plan.scan().object_type(), &mut references);
    }
    if let Some(selection) = plan.selection() {
        query_expression_references(selection, plan.scan().object_type(), &mut references);
    }
    references
}

fn version_one_query_reference_sequence(
    plan: &crate::relational::RelationalQueryIr<TypeId, FieldId>,
    function: &FunctionDefinition,
) -> Vec<(DefinitionReferenceKind, DefinitionReferenceTarget)> {
    let mut references = signature_reference_sequence(function);
    references.push((
        DefinitionReferenceKind::QueryObject,
        DefinitionReferenceTarget::ObjectType(plan.scan().object_type()),
    ));
    for projection in plan.projections() {
        query_expression_references(projection, plan.scan().object_type(), &mut references);
    }
    if let Some(selection) = plan.selection() {
        query_expression_references(selection, plan.scan().object_type(), &mut references);
    }
    for ordering in plan.ordering() {
        query_expression_references(
            ordering.expression(),
            plan.scan().object_type(),
            &mut references,
        );
    }
    references
}

fn query_expression_references(
    expression: &crate::relational::ExpressionIr<TypeId, FieldId>,
    scan: TypeId,
    references: &mut Vec<(DefinitionReferenceKind, DefinitionReferenceTarget)>,
) {
    use crate::relational::ExpressionKind;

    match expression.kind() {
        ExpressionKind::ObjectReference { .. } => references.push((
            DefinitionReferenceKind::ObjectReference,
            DefinitionReferenceTarget::ObjectType(scan),
        )),
        ExpressionKind::FieldPath { steps, .. } => references.extend(steps.iter().map(|step| {
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: step.owner(),
                    field: step.field(),
                },
            )
        })),
        ExpressionKind::BooleanLiteral { .. } => {}
        ExpressionKind::Equality { left, right } => {
            query_expression_references(left, scan, references);
            query_expression_references(right, scan, references);
        }
    }
}

fn server_delete_plan(
    plan: &DeletePlanIr<TypeId, FunctionId, ParameterId>,
    function: &FunctionDefinition,
    object_types: &[ObjectTypeDefinition],
    references: &[(DefinitionReferenceKind, DefinitionReferenceTarget)],
) -> Result<ServerDeletePlan, PrepareError> {
    if !object_types
        .iter()
        .any(|object_type| object_type.id() == plan.target_object())
    {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "DELETE target object is absent from the candidate catalogue",
        });
    }
    if function.domain() != FunctionDomain::Server
        || function.security() != FunctionSecurity::Invoker
        || function.transaction() != Some(FunctionTransaction::Atomic)
        || function.volatility() != FunctionVolatility::Volatile
    {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "DELETE function has unsupported execution modes",
        });
    }
    let FunctionReturn::Rows(columns) = function.return_type() else {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "DELETE function does not return ROWS",
        });
    };
    if columns.len() != 1
        || columns[0].resolved_type() != ResolvedType::Scalar(StandardScalar::Boolean)
    {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "DELETE function does not return exactly one BOOLEAN column",
        });
    }

    validate_mutation_parameters(function, object_types)?;
    validate_mutation_selector(
        plan.selector_owner(),
        plan.selector_parameter(),
        plan.target_object(),
        function,
    )?;
    validate_reference_sequence(
        &delete_reference_sequence(plan, function),
        references,
        "mutation definition references differ from the checked body",
    )?;

    Ok(ServerDeletePlan::new(
        plan.target_object(),
        MutationSelector::new(plan.selector_owner(), plan.selector_parameter()),
    ))
}

fn validate_mutation_selector(
    owner: FunctionId,
    parameter: ParameterId,
    target: TypeId,
    function: &FunctionDefinition,
) -> Result<(), PrepareError> {
    if owner != function.id() {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "mutation selector owner differs from its enclosing function",
        });
    }
    let selector =
        function
            .parameter_by_id(parameter)
            .ok_or(PrepareError::InvalidCheckedBundle {
                reason: "mutation selector parameter is not declared by its enclosing function",
            })?;
    if selector.default_expression().is_some() {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "mutation selector parameter has an unsupported default expression",
        });
    }
    if selector.resolved_type() != (ResolvedType::Reference { target }) {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "mutation selector parameter does not reference its target object",
        });
    }
    Ok(())
}

fn validate_mutation_parameters(
    function: &FunctionDefinition,
    object_types: &[ObjectTypeDefinition],
) -> Result<(), PrepareError> {
    validate_mutation_parameters_with_catalogue(function, object_types, &[], None)
}

fn validate_mutation_parameters_with_catalogue(
    function: &FunctionDefinition,
    object_types: &[ObjectTypeDefinition],
    enum_types: &[EnumTypeDefinition],
    standard: Option<&CatalogueSnapshot>,
) -> Result<(), PrepareError> {
    for parameter in function.parameters() {
        if parameter.default_expression().is_some() {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "mutation parameter has an unsupported default expression",
            });
        }
        let resolved_type = parameter.resolved_type();
        if let Some(scalar) = resolved_type.legacy_scalar() {
            if matches!(
                scalar,
                StandardScalar::Boolean
                    | StandardScalar::Integer
                    | StandardScalar::BigInt
                    | StandardScalar::Float
                    | StandardScalar::CharacterLargeObject
                    | StandardScalar::BinaryLargeObject
            ) {
                continue;
            }
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "mutation parameter has an unsupported runtime type",
            });
        }
        if let Some(target) = resolved_type.reference_target() {
            if object_types
                .iter()
                .any(|object_type| object_type.id() == target)
            {
                continue;
            }
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "mutation parameter REF target is absent from the candidate catalogue",
            });
        }
        if let Some(type_id) = resolved_type.named_type() {
            if enum_types.iter().any(|enum_type| enum_type.id() == type_id)
                || standard.is_some_and(|standard| standard.enum_type_by_id(type_id).is_some())
            {
                continue;
            }
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "mutation parameter named type is not an active enum",
            });
        }
        if resolved_type.value_type().is_some() {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "mutation parameter has an unsupported runtime type",
            });
        }
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "mutation parameter has an unsupported runtime type",
        });
    }
    Ok(())
}

#[cfg(test)]
fn validate_mutation_assignments(
    assignments: &[crate::mutation::MutationAssignment<TypeId, FieldId, FunctionId, ParameterId>],
    target: &ObjectTypeDefinition,
    function: &FunctionDefinition,
    require_all_non_nullable_fields: bool,
) -> Result<Vec<ServerMutationFieldAssignment>, PrepareError> {
    validate_mutation_assignments_with_catalogue(
        assignments,
        target,
        function,
        &[],
        &[],
        None,
        require_all_non_nullable_fields,
    )
}

fn validate_mutation_assignments_with_catalogue(
    assignments: &[crate::mutation::MutationAssignment<TypeId, FieldId, FunctionId, ParameterId>],
    target: &ObjectTypeDefinition,
    function: &FunctionDefinition,
    enum_types: &[EnumTypeDefinition],
    record_value_types: &[RecordValueTypeDefinition],
    standard: Option<&CatalogueSnapshot>,
    require_all_non_nullable_fields: bool,
) -> Result<Vec<ServerMutationFieldAssignment>, PrepareError> {
    let mut durable_assignments = Vec::with_capacity(assignments.len());
    let mut assigned_fields = HashSet::with_capacity(assignments.len());
    for assignment in assignments {
        if assignment.owner() != target.id() {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "mutation assignment owner differs from its target object",
            });
        }
        let field =
            target
                .field_by_id(assignment.field())
                .ok_or(PrepareError::InvalidCheckedBundle {
                    reason: "mutation field is absent from its target object",
                })?;
        if !assigned_fields.insert(assignment.field()) {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "mutation assigns one target field more than once",
            });
        }
        let expression = server_mutation_expression(
            assignment.expression(),
            function,
            field,
            enum_types,
            record_value_types,
            standard,
        )?;
        durable_assignments.push(ServerMutationFieldAssignment::new(
            assignment.owner(),
            assignment.field(),
            expression,
        ));
    }
    if require_all_non_nullable_fields
        && target
            .fields()
            .iter()
            .any(|field| !field.nullable() && !assigned_fields.contains(&field.id()))
    {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "mutation omits a non-nullable target field",
        });
    }
    Ok(durable_assignments)
}

fn server_mutation_expression(
    expression: &crate::mutation::MutationExpression<TypeId, FunctionId, ParameterId, FieldId>,
    function: &FunctionDefinition,
    field: &FieldDefinition,
    enum_types: &[EnumTypeDefinition],
    record_value_types: &[RecordValueTypeDefinition],
    standard: Option<&CatalogueSnapshot>,
) -> Result<ServerMutationExpression, PrepareError> {
    let expected_type = resolved_type_from_semantic(expression.value_type().semantic_type());
    let expected_nullable = expression.value_type().nullable();
    if expected_type != field.resolved_type() {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "mutation expression type differs from its target field",
        });
    }
    if expected_nullable && !field.nullable() {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "nullable mutation expression targets a non-nullable field",
        });
    }
    let artifact_expression = match expression.kind() {
        MutationExpressionKind::ParameterRead { owner, parameter } => {
            if *owner != function.id() {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "mutation parameter owner differs from its enclosing function",
                });
            }
            let parameter =
                function
                    .parameter_by_id(*parameter)
                    .ok_or(PrepareError::InvalidCheckedBundle {
                        reason: "mutation parameter is not declared by its enclosing function",
                    })?;
            if parameter.default_expression().is_some() {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "mutation parameter has an unsupported default expression",
                });
            }
            if parameter.resolved_type() != expected_type {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "mutation parameter type differs from its expression",
                });
            }
            if expected_nullable {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "mutation parameter expression is nullable",
                });
            }
            ServerMutationExpression::parameter(*owner, parameter.id(), expected_type)?
        }
        MutationExpressionKind::BooleanLiteral { value } => {
            if expected_type != ResolvedType::Scalar(orna_core::types::StandardScalar::Boolean)
                || expected_nullable
            {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "mutation BOOLEAN expression has inconsistent type facts",
                });
            }
            ServerMutationExpression::boolean_literal(*value)
        }
        MutationExpressionKind::TypedNull => {
            if !expected_nullable {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "mutation typed NULL expression is not nullable",
                });
            }
            ServerMutationExpression::typed_null(expected_type)?
        }
        MutationExpressionKind::RecordConstructor {
            record_type,
            fields,
        } => {
            if field.nullable() {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "record constructor targets a nullable object field",
                });
            }
            if expected_nullable || expected_type != ResolvedType::Named(*record_type) {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "record constructor type facts differ from its target field",
                });
            }
            let definition = record_value_types
                .iter()
                .find(|definition| definition.id() == *record_type)
                .ok_or(PrepareError::InvalidCheckedBundle {
                    reason: "record constructor type is absent from the candidate catalogue",
                })?;
            if fields.len() != definition.fields().len() {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "record constructor field count differs from its candidate definition",
                });
            }
            let fields = fields
                .iter()
                .zip(definition.fields())
                .map(|(checked, durable)| {
                    server_record_field_expression(
                        checked,
                        durable,
                        *record_type,
                        function,
                        enum_types,
                        standard,
                    )
                })
                .collect::<Result<Vec<_>, PrepareError>>()?;
            ServerMutationExpression::record_constructor(*record_type, fields)?
        }
    };

    if artifact_expression.resolved_type() != expected_type
        || artifact_expression.nullable() != expected_nullable
    {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "mutation artifact expression differs from checked type facts",
        });
    }
    match artifact_expression.kind() {
        ServerMutationExpressionKind::Parameter { owner, parameter } => {
            if !matches!(
                expression.kind(),
                MutationExpressionKind::ParameterRead { .. }
            ) || *owner != function.id()
                || function.parameter_by_id(*parameter).is_none()
            {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "mutation artifact parameter expression differs from checked facts",
                });
            }
        }
        ServerMutationExpressionKind::BooleanLiteral { .. } => {
            if !matches!(
                expression.kind(),
                MutationExpressionKind::BooleanLiteral { .. }
            ) {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "mutation artifact BOOLEAN expression differs from checked facts",
                });
            }
        }
        ServerMutationExpressionKind::TypedNull => {
            if !matches!(expression.kind(), MutationExpressionKind::TypedNull) {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "mutation artifact NULL expression differs from checked facts",
                });
            }
        }
        ServerMutationExpressionKind::RecordConstructor { fields } => {
            let MutationExpressionKind::RecordConstructor {
                record_type,
                fields: checked_fields,
            } = expression.kind()
            else {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "mutation artifact record expression differs from checked facts",
                });
            };
            if artifact_expression.resolved_type() != ResolvedType::Named(*record_type)
                || fields.len() != checked_fields.len()
            {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "mutation artifact record expression differs from checked facts",
                });
            }
            for (artifact, checked) in fields.iter().zip(checked_fields) {
                if artifact.owner() != checked.owner()
                    || artifact.field() != checked.field()
                    || artifact.resolved_type()
                        != resolved_type_from_semantic(checked.value_type().semantic_type())
                {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: "mutation artifact record field differs from checked facts",
                    });
                }
                let kind_matches = match (artifact.kind(), checked.kind()) {
                    (
                        ServerRecordFieldExpressionKind::Parameter {
                            owner: artifact_owner,
                            parameter: artifact_parameter,
                        },
                        MutationRecordFieldExpressionKind::ParameterRead {
                            owner: checked_owner,
                            parameter: checked_parameter,
                        },
                    ) => artifact_owner == checked_owner && artifact_parameter == checked_parameter,
                    (
                        ServerRecordFieldExpressionKind::BooleanLiteral {
                            value: artifact_value,
                        },
                        MutationRecordFieldExpressionKind::BooleanLiteral {
                            value: checked_value,
                        },
                    ) => artifact_value == checked_value,
                    _ => false,
                };
                if !kind_matches {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: "mutation artifact record field expression differs from checked facts",
                    });
                }
            }
        }
        _ => {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "mutation artifact has an unsupported expression kind",
            });
        }
    }
    Ok(artifact_expression)
}

fn server_record_field_expression(
    checked: &crate::mutation::MutationRecordFieldExpression<
        TypeId,
        FieldId,
        FunctionId,
        ParameterId,
    >,
    durable: &RecordValueFieldDefinition,
    record_type: TypeId,
    function: &FunctionDefinition,
    enum_types: &[EnumTypeDefinition],
    standard: Option<&CatalogueSnapshot>,
) -> Result<ServerRecordFieldExpression, PrepareError> {
    if checked.owner() != record_type || checked.field() != durable.id() {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "record constructor field identity or order differs from its candidate definition",
        });
    }
    let checked_semantic = checked.value_type().semantic_type();
    let artifact = match checked.kind() {
        MutationRecordFieldExpressionKind::ParameterRead { owner, parameter } => {
            if *owner != function.id() {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "record constructor parameter owner differs from its enclosing function",
                });
            }
            let parameter = function.parameter_by_id(*parameter).ok_or(
                PrepareError::InvalidCheckedBundle {
                    reason: "record constructor parameter is absent from its enclosing function",
                },
            )?;
            if parameter.default_expression().is_some()
                || parameter.resolved_type() != resolved_type_from_semantic(checked_semantic)
            {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "record constructor parameter type differs from checked facts",
                });
            }
            validate_record_child_type(
                checked_semantic,
                checked.value_type().standard_value_type(),
                durable.descriptor(),
                enum_types,
                standard,
            )?;
            ServerRecordFieldExpression::parameter(
                record_type,
                durable.id(),
                *owner,
                parameter.id(),
                parameter.resolved_type(),
            )?
        }
        MutationRecordFieldExpressionKind::BooleanLiteral { value } => {
            validate_record_child_type(
                checked_semantic,
                checked.value_type().standard_value_type(),
                durable.descriptor(),
                enum_types,
                standard,
            )?;
            if checked_semantic != SemanticType::Scalar(StandardScalar::Boolean) {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "record constructor Boolean child has inconsistent type facts",
                });
            }
            ServerRecordFieldExpression::boolean_literal(record_type, durable.id(), *value)
        }
    };
    if artifact.owner() != record_type
        || artifact.field() != durable.id()
        || artifact.resolved_type() != resolved_type_from_semantic(checked_semantic)
    {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "record constructor artifact field differs from checked facts",
        });
    }
    Ok(artifact)
}

fn validate_record_child_type(
    checked_semantic: SemanticType<TypeId>,
    checked_standard: Option<TypeId>,
    durable: &TypeDescriptor,
    enum_types: &[EnumTypeDefinition],
    standard: Option<&CatalogueSnapshot>,
) -> Result<(), PrepareError> {
    let TypeDescriptorKind::Named(durable_id) = durable.kind() else {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "record constructor child type differs from its durable candidate field",
        });
    };
    match (checked_semantic, checked_standard) {
        (SemanticType::Scalar(scalar), Some(checked_standard))
            if checked_standard == durable_id
                && standard.is_some_and(|standard| {
                    standard
                        .value_type_by_id(durable_id)
                        .and_then(accepted_record_standard_scalar)
                        == Some(scalar)
                }) =>
        {
            Ok(())
        }
        (SemanticType::Named(checked), None)
            if checked == durable_id
                && (enum_types
                    .iter()
                    .any(|enum_type| enum_type.id() == durable_id)
                    || standard.is_some_and(|standard| {
                        standard.enum_type_by_id(durable_id).is_some()
                    })) =>
        {
            Ok(())
        }
        _ => Err(PrepareError::InvalidCheckedBundle {
            reason: "record constructor child type differs from its durable candidate field",
        }),
    }
}

fn accepted_record_standard_scalar(
    value_type: &orna_core::catalogue::ValueTypeDefinition,
) -> Option<StandardScalar> {
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

fn mutation_reference_sequence(
    plan: &MutationPlanIr<TypeId, FieldId, FunctionId, ParameterId>,
    function: &FunctionDefinition,
) -> Vec<(DefinitionReferenceKind, DefinitionReferenceTarget)> {
    let mut references = signature_reference_sequence(function);
    references.push((
        DefinitionReferenceKind::WriteObject,
        DefinitionReferenceTarget::ObjectType(plan.target_object()),
    ));
    for assignment in plan.assignments() {
        references.push((
            DefinitionReferenceKind::WriteField,
            DefinitionReferenceTarget::Field {
                owner: assignment.owner(),
                field: assignment.field(),
            },
        ));
        match assignment.expression().kind() {
            MutationExpressionKind::ParameterRead { owner, parameter } => {
                references.push((
                    DefinitionReferenceKind::ParameterRead,
                    DefinitionReferenceTarget::Parameter {
                        owner: *owner,
                        parameter: *parameter,
                    },
                ));
            }
            MutationExpressionKind::RecordConstructor {
                record_type,
                fields,
            } => {
                references.push((
                    DefinitionReferenceKind::NamedType,
                    DefinitionReferenceTarget::ValueType(*record_type),
                ));
                for field in fields {
                    references.push((
                        DefinitionReferenceKind::WriteField,
                        DefinitionReferenceTarget::Field {
                            owner: field.owner(),
                            field: field.field(),
                        },
                    ));
                    if let MutationRecordFieldExpressionKind::ParameterRead { owner, parameter } =
                        field.kind()
                    {
                        references.push((
                            DefinitionReferenceKind::ParameterRead,
                            DefinitionReferenceTarget::Parameter {
                                owner: *owner,
                                parameter: *parameter,
                            },
                        ));
                    }
                }
            }
            MutationExpressionKind::BooleanLiteral { .. } | MutationExpressionKind::TypedNull => {}
        }
    }
    if let MutationOperation::Update {
        selector_owner,
        selector_parameter,
    } = plan.operation()
    {
        references.push((
            DefinitionReferenceKind::ObjectReference,
            DefinitionReferenceTarget::ObjectType(plan.target_object()),
        ));
        references.push((
            DefinitionReferenceKind::ParameterRead,
            DefinitionReferenceTarget::Parameter {
                owner: *selector_owner,
                parameter: *selector_parameter,
            },
        ));
    }
    references.push((
        DefinitionReferenceKind::ObjectReference,
        DefinitionReferenceTarget::ObjectType(plan.returned_object()),
    ));
    references
}

fn delete_reference_sequence(
    plan: &DeletePlanIr<TypeId, FunctionId, ParameterId>,
    function: &FunctionDefinition,
) -> Vec<(DefinitionReferenceKind, DefinitionReferenceTarget)> {
    let mut references = signature_reference_sequence(function);
    references.extend([
        (
            DefinitionReferenceKind::WriteObject,
            DefinitionReferenceTarget::ObjectType(plan.target_object()),
        ),
        (
            DefinitionReferenceKind::ObjectReference,
            DefinitionReferenceTarget::ObjectType(plan.target_object()),
        ),
        (
            DefinitionReferenceKind::ParameterRead,
            DefinitionReferenceTarget::Parameter {
                owner: plan.selector_owner(),
                parameter: plan.selector_parameter(),
            },
        ),
    ]);
    references
}

fn signature_reference_sequence(
    function: &FunctionDefinition,
) -> Vec<(DefinitionReferenceKind, DefinitionReferenceTarget)> {
    let mut references = Vec::new();
    for parameter in function.parameters() {
        if let ResolvedType::Reference { target } = parameter.resolved_type() {
            references.push((
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(target),
            ));
        }
    }
    match function.return_type() {
        FunctionReturn::Rows(columns) => {
            for column in columns {
                if let ResolvedType::Reference { target } = column.resolved_type() {
                    references.push((
                        DefinitionReferenceKind::ObjectReference,
                        DefinitionReferenceTarget::ObjectType(target),
                    ));
                }
            }
        }
        FunctionReturn::Stream(element) => {
            if let ResolvedType::Reference { target } = element {
                references.push((
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(*target),
                ));
            }
        }
        FunctionReturn::Single(_) => {}
    }
    references
}

fn validate_reference_sequence(
    expected: &[(DefinitionReferenceKind, DefinitionReferenceTarget)],
    actual: &[(DefinitionReferenceKind, DefinitionReferenceTarget)],
    reason: &'static str,
) -> Result<(), PrepareError> {
    if expected == actual {
        Ok(())
    } else {
        Err(PrepareError::InvalidCheckedBundle { reason })
    }
}

fn is_sealed_inspect_type_id(id: TypeId) -> bool {
    [
        orna_core::system::SYS_INSPECT_INVOCATION_TYPE_ID,
        orna_core::system::SYS_INSPECT_SNAPSHOT_TYPE_ID,
        orna_core::system::SYS_INSPECT_SNAPSHOT_OPTIONS_TYPE_ID,
        orna_core::system::SYS_INSPECT_TRACE_EVENT_TYPE_ID,
        orna_core::system::SYS_INSPECT_INVOCATION_NODES_TYPE_ID,
        orna_core::system::SYS_INSPECT_CALLS_TYPE_ID,
        orna_core::system::SYS_INSPECT_RESOURCES_TYPE_ID,
        orna_core::system::SYS_INSPECT_STATE_CELLS_TYPE_ID,
        orna_core::system::SYS_INSPECT_UI_NODES_TYPE_ID,
        orna_core::system::SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID,
        orna_core::system::SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID,
        orna_core::system::SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID,
    ]
    .contains(&id)
}

fn resolved_type_from_semantic(semantic_type: SemanticType<TypeId>) -> ResolvedType {
    match semantic_type {
        SemanticType::Scalar(scalar) => ResolvedType::Scalar(scalar),
        SemanticType::Named(id) => ResolvedType::Named(id),
        SemanticType::Reference { target } => ResolvedType::Reference { target },
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

/// Maps one checked CLIENT capability requirement to its artifact carrier.
///
/// The checked name is the closed vocabulary name and the argument source is
/// the declaration's literal scope text or parameter reference. The
/// version-5 envelope carries both as plain text so it stays independent of
/// the client grant model.
fn client_capability_requirement(
    capability: &crate::CheckedClientCapability,
) -> CapabilityRequirement {
    let argument = match capability.argument() {
        crate::CheckedClientCapabilityArgument::Text(text) => {
            CapabilityArgumentSource::Text(text.clone())
        }
        crate::CheckedClientCapabilityArgument::Parameter(parameter) => {
            CapabilityArgumentSource::Parameter(parameter.clone())
        }
    };
    CapabilityRequirement::new(capability.name().to_owned(), argument)
}

fn client_expression_contains_action(expression: &CheckedClientExpression) -> bool {
    match expression {
        CheckedClientExpression::Action { .. } => true,
        CheckedClientExpression::Await { expression, .. } => {
            client_expression_contains_action(expression)
        }
        CheckedClientExpression::Resource { operation } => operation
            .arguments()
            .iter()
            .any(|(_, value)| client_expression_contains_action(value)),
        CheckedClientExpression::Call { arguments, .. } => arguments
            .iter()
            .any(|(_, value)| client_expression_contains_action(value)),
        CheckedClientExpression::Inspect { operation } => match operation {
            CheckedInspectOperation::Snapshot { target, .. } => {
                client_expression_contains_action(target)
            }
            CheckedInspectOperation::Projection { snapshot, .. } => {
                client_expression_contains_action(snapshot)
            }
        },
        CheckedClientExpression::Evaluate { expression, .. } => {
            client_expression_contains_action(expression)
        }
        CheckedClientExpression::Concat { left, right, .. }
        | CheckedClientExpression::Binary { left, right, .. } => {
            client_expression_contains_action(left) || client_expression_contains_action(right)
        }
        CheckedClientExpression::Unary { expression, .. }
        | CheckedClientExpression::Parenthesized { expression, .. } => {
            client_expression_contains_action(expression)
        }
        CheckedClientExpression::Input { .. }
        | CheckedClientExpression::SourceIntrospection { .. }
        | CheckedClientExpression::String { .. }
        | CheckedClientExpression::Integer { .. }
        | CheckedClientExpression::Boolean { .. }
        | CheckedClientExpression::ParameterRead { .. }
        | CheckedClientExpression::LocalRead { .. }
        | CheckedClientExpression::FieldPath { .. } => false,
    }
}

fn client_expression_contains_inspect(expression: &CheckedClientExpression) -> bool {
    match expression {
        CheckedClientExpression::Inspect { .. } => true,
        CheckedClientExpression::Await { expression, .. }
        | CheckedClientExpression::Unary { expression, .. }
        | CheckedClientExpression::Parenthesized { expression, .. }
        | CheckedClientExpression::Evaluate { expression, .. } => client_expression_contains_inspect(expression),
        CheckedClientExpression::Call { arguments, .. } => arguments.iter().any(|(_, value)| client_expression_contains_inspect(value)),
        CheckedClientExpression::Resource { operation } => operation.arguments().iter().any(|(_, value)| client_expression_contains_inspect(value)),
        CheckedClientExpression::Action { operation } => operation.arguments().iter().any(|(_, value)| client_expression_contains_inspect(value)),
        CheckedClientExpression::Concat { left, right, .. } | CheckedClientExpression::Binary { left, right, .. } => client_expression_contains_inspect(left) || client_expression_contains_inspect(right),
        CheckedClientExpression::SourceIntrospection { .. } | CheckedClientExpression::Input { .. } | CheckedClientExpression::String { .. } | CheckedClientExpression::Integer { .. } | CheckedClientExpression::Boolean { .. } | CheckedClientExpression::ParameterRead { .. } | CheckedClientExpression::LocalRead { .. } | CheckedClientExpression::FieldPath { .. } => false,
    }
}

fn client_expression_contains_resource(expression: &CheckedClientExpression) -> bool {
    match expression {
        CheckedClientExpression::Await { expression, .. }
        | CheckedClientExpression::Unary { expression, .. }
        | CheckedClientExpression::Parenthesized { expression, .. } => {
            client_expression_contains_resource(expression)
        }
        CheckedClientExpression::Inspect { operation } => match operation {
            CheckedInspectOperation::Snapshot { target, options, .. } => {
                client_expression_contains_resource(target)
                    || options
                        .as_deref()
                        .is_some_and(client_expression_contains_resource)
            }
            CheckedInspectOperation::Projection { snapshot, .. } => {
                client_expression_contains_resource(snapshot)
            }
        },
        CheckedClientExpression::Call { arguments, .. } => arguments
            .iter()
            .any(|(_, argument)| client_expression_contains_resource(argument)),
        CheckedClientExpression::Evaluate { expression, .. } => {
            client_expression_contains_resource(expression)
        }
        CheckedClientExpression::Resource { .. } | CheckedClientExpression::Action { .. } => true,
        CheckedClientExpression::Concat { left, right, .. }
        | CheckedClientExpression::Binary { left, right, .. } => {
            client_expression_contains_resource(left) || client_expression_contains_resource(right)
        }
        CheckedClientExpression::SourceIntrospection { .. }
        | CheckedClientExpression::Input { .. }
        | CheckedClientExpression::String { .. }
        | CheckedClientExpression::Integer { .. }
        | CheckedClientExpression::Boolean { .. }
        | CheckedClientExpression::ParameterRead { .. }
        | CheckedClientExpression::LocalRead { .. }
        | CheckedClientExpression::FieldPath { .. } => false,
    }
}
fn client_control_flow_scalar_type_id(scalar: StandardScalar) -> Option<TypeId> {
    match scalar {
        StandardScalar::Boolean => Some(STD_BOOLEAN_TYPE_ID),
        StandardScalar::Integer => Some(STD_INTEGER_TYPE_ID),
        StandardScalar::CharacterLargeObject => Some(STD_CHARACTER_LARGE_OBJECT_TYPE_ID),
        StandardScalar::BigInt
        | StandardScalar::Float
        | StandardScalar::Decimal
        | StandardScalar::BinaryLargeObject
        | StandardScalar::Uuid
        | StandardScalar::Date
        | StandardScalar::Time
        | StandardScalar::Timestamp
        | StandardScalar::Duration
        | StandardScalar::Void => None,
    }
}
fn durable_client_local_id(function: FunctionId, ordinal: u32) -> LocalId {
    let mut payload = function.to_bytes().to_vec();
    payload.extend_from_slice(&ordinal.to_be_bytes());
    let digest =
        artifact_payload_digest(&payload).expect("client local identity payload is bounded");
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest.to_bytes()[..16]);
    LocalId::from_bytes(bytes)
}

impl<'a> CandidateBuilder<'a> {
    fn new(
        parse_report: &'a ParseReport,
        checked: &'a CheckedBundle,
        active: &'a ActiveDatabaseRevision,
        identities: IdentityMap,
        source: PreparedSource,
        mode: PreparationMode<'a>,
        catalogue_revision: CatalogueRevisionId,
    ) -> Self {
        let declaration_evidence = match &mode {
            PreparationMode::Generic | PreparationMode::LegacyV1 => None,
            PreparationMode::StandardV1Match {
                declaration_evidence,
                ..
            }
            | PreparationMode::StandardV2Plan {
                declaration_evidence,
                ..
            }
            | PreparationMode::StandardV2 {
                declaration_evidence,
                ..
            } => Some(RefCell::new(declaration_evidence.clone())),
        };
        Self {
            checked,
            parse_report,
            active,
            mode,
            identities,
            source,
            catalogue_revision,
            origins: Vec::new(),
            expressions: Vec::new(),
            functions: Vec::new(),
            current_function_revisions: Vec::new(),
            new_function_revisions: Vec::new(),
            references: Vec::new(),
            declaration_evidence,
        }
    }

    fn build(self) -> Result<DeployableRevision, PrepareError> {
        let active = self.active;
        let context = self.mode.catalogue_hash_context();
        self.materialise()?.into_deployable(active, context)
    }

    fn materialise(mut self) -> Result<CandidateMaterial, PrepareError> {
        let schemas = self.build_schemas()?;
        let object_types = self.build_object_types()?;
        let enum_types = self.build_enum_types()?;
        let record_value_types = self.build_record_value_types()?;
        self.validate_record_value_evolution(&record_value_types)?;
        self.build_functions(
            &object_types.compatibility,
            &enum_types,
            &record_value_types,
        )?;
        if self
            .declaration_evidence
            .as_ref()
            .is_some_and(|evidence| !evidence.borrow().is_empty())
        {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "checked standard declaration type evidence was not consumed",
            });
        }

        let catalogue = CatalogueSnapshot::new_with_functions_and_record_value_types(
            self.catalogue_revision,
            schemas,
            object_types.durable,
            Vec::new(),
            enum_types,
            record_value_types,
            Vec::new(),
            self.functions,
        )?;
        Ok(CandidateMaterial {
            source: self.source.revision,
            catalogue,
            origins: self.origins,
            expressions: self.expressions,
            current_function_revisions: self.current_function_revisions,
            new_function_revisions: self.new_function_revisions,
            references: self.references,
        })
    }

    fn candidate_resolved_type(
        &self,
        semantic_type: SemanticType<CheckedTypeId>,
        kind: crate::CheckedTypeUseKind,
        consume_evidence: bool,
    ) -> Result<CandidateResolvedType, PrepareError> {
        let compatibility = self.identities.resolved_type(semantic_type)?;
        let evidence = self
            .declaration_evidence
            .as_ref()
            .map(|declaration_evidence| {
                if consume_evidence {
                    declaration_evidence.borrow_mut().consume(kind)
                } else {
                    declaration_evidence.borrow().lookup(kind)
                }
            })
            .transpose()?;
        let evidence = evidence
            .map(|evidence| self.mapped_evidence_target(evidence.target))
            .transpose()?;
        candidate_from_mapped_evidence(compatibility, evidence)
    }

    fn mapped_evidence_target(
        &self,
        evidence: EvidenceTarget,
    ) -> Result<MappedEvidenceTarget, PrepareError> {
        match evidence {
            EvidenceTarget::Value(type_id) => Ok(MappedEvidenceTarget::Value(type_id)),
            EvidenceTarget::Named(target) => self
                .identities
                .type_id(target)
                .map(MappedEvidenceTarget::Named),
            EvidenceTarget::ObjectReference(target) => self
                .identities
                .type_id(target)
                .map(MappedEvidenceTarget::ObjectReference),
            EvidenceTarget::Unknown => Ok(MappedEvidenceTarget::Unknown),
        }
    }

    fn declaration_type(
        &self,
        semantic_type: SemanticType<CheckedTypeId>,
        kind: crate::CheckedTypeUseKind,
        consume_evidence: bool,
        projection: CandidateTypeProjection,
    ) -> Result<ResolvedType, PrepareError> {
        Ok(self.mode.lower_candidate_type(
            self.candidate_resolved_type(semantic_type, kind, consume_evidence)?,
            projection,
        ))
    }

    fn build_schemas(&mut self) -> Result<Vec<SchemaDefinition>, PrepareError> {
        let schemas = self.catalogue_schemas()?;
        for checked in self.checked.schemas() {
            self.push_origin(
                DefinitionIdentity::Schema(self.identities.schema(checked.id())?),
                checked.location(),
            )?;
        }
        Ok(schemas)
    }

    fn catalogue_schemas(&self) -> Result<Vec<SchemaDefinition>, PrepareError> {
        let mut schemas = Vec::with_capacity(self.checked.schemas().len());
        for checked in self.checked.schemas() {
            let id = self.identities.schema(checked.id())?;
            schemas.push(SchemaDefinition::new(id, checked.name().clone()));
        }
        Ok(schemas)
    }

    fn build_object_types(&mut self) -> Result<ObjectTypeProjections, PrepareError> {
        let mut compatibility = Vec::with_capacity(self.checked.object_types().len());
        let mut durable = Vec::with_capacity(self.checked.object_types().len());
        for checked_type in self.checked.object_types() {
            let type_id = self.identities.type_id(checked_type.id())?;
            let mut compatibility_fields = Vec::with_capacity(checked_type.fields().len());
            let mut durable_fields = Vec::with_capacity(checked_type.fields().len());
            for checked_field in checked_type.fields() {
                let field_id = self.identities.field(checked_field.id())?;
                let default_expression = checked_field
                    .default()
                    .map(|default| self.identities.expression(default.id()))
                    .transpose()?;
                let kind = crate::CheckedTypeUseKind::Field {
                    owner: checked_type.id(),
                    field: checked_field.id(),
                };
                let compatibility_type = self.declaration_type(
                    checked_field.semantic_type(),
                    kind,
                    false,
                    CandidateTypeProjection::Compatibility,
                )?;
                let durable_type = self.declaration_type(
                    checked_field.semantic_type(),
                    kind,
                    true,
                    CandidateTypeProjection::Durable,
                )?;
                if checked_field.unique()
                    && !supports_durable_unique_field(
                        durable_type,
                        checked_field.nullable(),
                        self.mode.durable_standard_catalogue(),
                    )
                {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: UNIQUE_FIELD_MESSAGE,
                    });
                }
                compatibility_fields.push(FieldDefinition::new(
                    field_id,
                    checked_field.name(),
                    checked_field.ordinal(),
                    compatibility_type,
                    checked_field.nullable(),
                    checked_field.unique(),
                    default_expression,
                    checked_field.on_delete(),
                ));
                durable_fields.push(FieldDefinition::new(
                    field_id,
                    checked_field.name(),
                    checked_field.ordinal(),
                    durable_type,
                    checked_field.nullable(),
                    checked_field.unique(),
                    default_expression,
                    checked_field.on_delete(),
                ));
            }
            compatibility.push(ObjectTypeDefinition::new(
                type_id,
                checked_type.name().clone(),
                compatibility_fields,
            ));
            durable.push(ObjectTypeDefinition::new(
                type_id,
                checked_type.name().clone(),
                durable_fields,
            ));
        }
        self.record_object_type_metadata()?;
        Ok(ObjectTypeProjections {
            compatibility,
            durable,
        })
    }

    fn build_enum_types(&mut self) -> Result<Vec<EnumTypeDefinition>, PrepareError> {
        let checked = self
            .checked
            .enum_types()
            .map(|(id, name, labels, location)| {
                Ok((
                    EnumTypeDefinition::new(
                        self.identities.type_id(id)?,
                        name.clone(),
                        labels.iter().cloned(),
                    ),
                    location.clone(),
                ))
            })
            .collect::<Result<Vec<_>, PrepareError>>()?;
        for (enum_type, location) in &checked {
            self.push_origin(DefinitionIdentity::ValueType(enum_type.id()), location)?;
        }
        Ok(checked
            .into_iter()
            .map(|(enum_type, _)| enum_type)
            .collect())
    }

    fn build_record_value_types(&mut self) -> Result<Vec<RecordValueTypeDefinition>, PrepareError> {
        let mut record_value_types = Vec::with_capacity(self.checked.record_value_types().len());
        for checked_type in self.checked.record_value_types() {
            let type_id = self.identities.type_id(checked_type.id())?;
            let mut fields = Vec::with_capacity(checked_type.fields().len());
            for checked_field in checked_type.fields() {
                let field_id = self.identities.field(checked_field.id())?;
                let resolved_type = self.declaration_type(
                    checked_field.semantic_type(),
                    crate::CheckedTypeUseKind::Field {
                        owner: checked_type.id(),
                        field: checked_field.id(),
                    },
                    true,
                    CandidateTypeProjection::Durable,
                )?;
                let descriptor = match resolved_type {
                    ResolvedType::Named(type_id) | ResolvedType::Value(type_id) => {
                        TypeDescriptor::named(type_id)
                    }
                    ResolvedType::Reference { target } => TypeDescriptor::reference(target),
                    ResolvedType::Scalar(_) => {
                        return Err(PrepareError::InvalidCheckedBundle {
                            reason: "checked record field has no catalogue identity",
                        });
                    }
                };
                fields.push(
                    RecordValueFieldDefinition::try_new_descriptor(
                        field_id,
                        checked_field.name(),
                        checked_field.ordinal(),
                        descriptor,
                    )
                    .map_err(|_| PrepareError::InvalidCheckedBundle {
                        reason: "checked record field has no catalogue identity",
                    })?,
                );
                self.push_origin(
                    DefinitionIdentity::Field {
                        owner: type_id,
                        field: field_id,
                    },
                    checked_field.location(),
                )?;
            }
            record_value_types.push(RecordValueTypeDefinition::new(
                type_id,
                checked_type.name().clone(),
                fields,
            ));
            self.push_origin(
                DefinitionIdentity::ValueType(type_id),
                checked_type.location(),
            )?;
        }
        Ok(record_value_types)
    }

    fn validate_record_value_evolution(
        &self,
        candidate: &[RecordValueTypeDefinition],
    ) -> Result<(), PrepareError> {
        for active in self.active.catalogue().record_value_types() {
            let candidate = candidate
                .iter()
                .find(|record_value_type| record_value_type.id() == active.id())
                .ok_or(PrepareError::InvalidCheckedBundle {
                    reason: "existing record value type is absent from the candidate catalogue",
                })?;
            if candidate.name() != active.name() {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "record value type rename is not supported",
                });
            }
            if candidate.fields().len() != active.fields().len() {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "record value field addition or removal is not supported",
                });
            }
            for active_field in active.fields() {
                let candidate_field = candidate.field_by_id(active_field.id()).ok_or(
                    PrepareError::InvalidCheckedBundle {
                        reason: "record value field replacement is not supported",
                    },
                )?;
                if candidate_field.name() != active_field.name() {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: "record value field rename is not supported",
                    });
                }
                if candidate_field.ordinal() != active_field.ordinal() {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: "record value field reordering is not supported",
                    });
                }
                if candidate_field.descriptor() != active_field.descriptor() {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: "record value field type change is not supported",
                    });
                }
            }
        }
        Ok(())
    }

    fn record_object_type_metadata(&mut self) -> Result<(), PrepareError> {
        for checked_type in self.checked.object_types() {
            let type_id = self.identities.type_id(checked_type.id())?;
            for checked_field in checked_type.fields() {
                let field_id = self.identities.field(checked_field.id())?;
                if let Some(default) = checked_field.default() {
                    let expression_id = self.identities.expression(default.id())?;
                    let value = match default.value() {
                        ConstantValue::Null => ConstantExpression::Null,
                        ConstantValue::Boolean(value) => ConstantExpression::Boolean(*value),
                        ConstantValue::Integer(value) => ConstantExpression::Integer(*value),
                        ConstantValue::Text(value) => ConstantExpression::Text(value.clone()),
                    };
                    let payload = value.encode()?;
                    let hash = artifact_payload_digest(&payload)?;
                    if let Some(existing) = self
                        .expressions
                        .iter()
                        .find(|artifact| artifact.id() == expression_id)
                    {
                        if existing.payload() != payload || existing.content_hash() != hash {
                            return Err(PrepareError::InvalidCheckedBundle {
                                reason: "shared checked expression has inconsistent values",
                            });
                        }
                    } else {
                        self.expressions.push(ExpressionArtifact::new(
                            expression_id,
                            CONSTANT_FORMAT,
                            CONSTANT_VERSION,
                            payload,
                            hash,
                        )?);
                        self.push_origin(
                            DefinitionIdentity::Expression(expression_id),
                            default.location(),
                        )?;
                    }
                }
                self.push_origin(
                    DefinitionIdentity::Field {
                        owner: type_id,
                        field: field_id,
                    },
                    checked_field.location(),
                )?;
            }
            self.push_origin(
                DefinitionIdentity::ObjectType(type_id),
                checked_type.location(),
            )?;
        }
        Ok(())
    }

    fn build_functions(
        &mut self,
        object_types: &[ObjectTypeDefinition],
        enum_types: &[EnumTypeDefinition],
        record_value_types: &[RecordValueTypeDefinition],
    ) -> Result<(), PrepareError> {
        let standard_owners = self
            .mode
            .standard_preflight()
            .map(|standard_preflight| {
                standard_preflight
                    .function_identities
                    .order()
                    .iter()
                    .map(|owner| {
                        Ok((
                            *owner,
                            standard_preflight.function_identities.domain(*owner)?,
                        ))
                    })
                    .collect::<Result<Vec<_>, PrepareError>>()
            })
            .transpose()?;

        let Some(standard_owners) = standard_owners else {
            for checked in self.checked.server_functions() {
                self.build_server_function(checked, object_types, enum_types, record_value_types)?;
            }
            for checked in self.checked.client_functions() {
                let validated = validate_generic_client_function(checked, self.active)?;
                self.build_client_function(&validated)?;
            }
            return Ok(());
        };

        for (owner, domain) in standard_owners {
            match domain {
                FunctionDomain::Server => {
                    let checked = self
                        .checked
                        .server_functions()
                        .iter()
                        .find(|function| function.id() == owner)
                        .cloned()
                        .ok_or(PrepareError::InvalidCheckedBundle {
                            reason: "checked standard function owners do not match declaration evidence",
                        })?;
                    self.build_server_function(
                        &checked,
                        object_types,
                        enum_types,
                        record_value_types,
                    )?;
                }
                FunctionDomain::Client => {
                    let validated = self.validated_client(owner)?;
                    self.build_client_function(&validated)?;
                }
            }
        }
        Ok(())
    }

    fn plan_standard_upgrade_lowering(
        mut self,
    ) -> Result<StandardUpgradeLoweringPlan, PrepareError> {
        let schemas = self.build_schemas()?;
        let object_types = self.build_object_types()?;
        let standard_preflight =
            self.mode
                .standard_preflight()
                .ok_or(PrepareError::InvalidCheckedBundle {
                    reason: "checked standard function requires standard preparation evidence",
                })?;
        let owners = standard_preflight
            .function_identities
            .order()
            .iter()
            .map(|owner| {
                Ok((
                    *owner,
                    standard_preflight.function_identities.domain(*owner)?,
                ))
            })
            .collect::<Result<Vec<_>, PrepareError>>()?;
        let mut plans = HashMap::with_capacity(owners.len());
        for (owner, domain) in owners {
            let function_plan = match domain {
                FunctionDomain::Server => {
                    let checked = self
                        .checked
                        .server_functions()
                        .iter()
                        .find(|function| function.id() == owner)
                        .ok_or(PrepareError::InvalidCheckedBundle {
                            reason: "checked standard function owners do not match declaration evidence",
                        })?
                        .clone();
                    let function = self.identities.function(checked.id())?;
                    let revision = self.initial_function_revision(checked.id(), function)?;
                    let compatibility_definition = self.function_definition(
                        &checked,
                        revision,
                        false,
                        CandidateTypeProjection::Compatibility,
                    )?;
                    let artifact = self.server_artifact(
                        &checked,
                        &compatibility_definition,
                        &object_types.compatibility,
                        &[],
                        &[],
                    )?;
                    let definition = self.function_definition(
                        &checked,
                        revision,
                        true,
                        CandidateTypeProjection::Durable,
                    )?;
                    let references = self.function_references(&checked, function, revision)?;
                    let semantic_hash_version = self.mode.semantic_hash_version(&references);
                    let plan = FunctionRevisionPlan::new(
                        self.active,
                        function,
                        FunctionRevisionPlanInput {
                            semantic_hash_version,
                            definition: &definition,
                            language_version: &artifact.language_version,
                            artifact: &artifact.artifact,
                            expressions: &self.expressions,
                            references: &references,
                            current_only: standard_upgrade_reuse_is_current_only(
                                semantic_hash_version,
                            ),
                            reuse_policy: FunctionRevisionReusePolicy::Complete,
                        },
                    )?;
                    let declaration_origin = self.source.origin(checked.location())?;
                    let declaration_content_hash = function_declaration_digest(
                        self.source
                            .declaration(self.parse_report, checked.location())?,
                    )?;
                    self.push_function_origins(&checked, function)?;
                    StandardUpgradeFunctionPlan {
                        revision: plan,
                        declaration_origin,
                        declaration_content_hash,
                    }
                }
                FunctionDomain::Client => {
                    let client = self.validated_client(owner)?.clone();
                    let function = self.identities.function(client.id)?;
                    let revision = self.initial_function_revision(client.id, function)?;
                    let definition = self.client_function_definition(
                        &client,
                        revision,
                        true,
                        CandidateTypeProjection::Durable,
                    )?;
                    let artifact = self.client_artifact(&client)?;
                    let references =
                        self.client_function_references(function, revision, &client)?;
                    let semantic_hash_version = self.mode.semantic_hash_version(&references);
                    let plan = FunctionRevisionPlan::new(
                        self.active,
                        function,
                        FunctionRevisionPlanInput {
                            semantic_hash_version,
                            definition: &definition,
                            language_version: &artifact.language_version,
                            artifact: &artifact.artifact,
                            expressions: &self.expressions,
                            references: &references,
                            current_only: standard_upgrade_reuse_is_current_only(
                                semantic_hash_version,
                            ),
                            reuse_policy: FunctionRevisionReusePolicy::Complete,
                        },
                    )?;
                    let declaration_origin = self.source.origin(&client.location)?;
                    let declaration_content_hash = function_declaration_digest(
                        self.source
                            .declaration(self.parse_report, &client.location)?,
                    )?;
                    self.push_client_function_origins(&client, function)?;
                    StandardUpgradeFunctionPlan {
                        revision: plan,
                        declaration_origin,
                        declaration_content_hash,
                    }
                }
            };
            let function = function_plan.revision.definition.id();
            if plans.insert(function, function_plan).is_some() {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "duplicate checked function",
                });
            }
        }
        if self
            .declaration_evidence
            .as_ref()
            .is_some_and(|evidence| !evidence.borrow().is_empty())
        {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "checked standard declaration type evidence was not consumed",
            });
        }

        let mut functions = Vec::with_capacity(plans.len());
        for definition in self.active.catalogue().functions() {
            let plan = plans.remove(&definition.id()).ok_or(
                PrepareError::InvalidCheckedBundle {
                    reason: "checked standard function owners do not match the active catalogue",
                },
            )?;
            functions.push(plan);
        }
        if !plans.is_empty() {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "checked standard function owners do not match the active catalogue",
            });
        }
        Ok(StandardUpgradeLoweringPlan {
            source_template: self.source.revision,
            schemas,
            object_types: object_types.durable,
            expressions: self.expressions,
            origin_templates: self.origins,
            functions,
        })
    }

    fn build_server_function(
        &mut self,
        checked: &crate::CheckedServerFunction,
        object_types: &[ObjectTypeDefinition],
        enum_types: &[EnumTypeDefinition],
        record_value_types: &[RecordValueTypeDefinition],
    ) -> Result<(), PrepareError> {
        let function_id = self.identities.function(checked.id())?;
        let initial_revision = self.initial_function_revision(checked.id(), function_id)?;
        let compatibility_definition = self.function_definition(
            checked,
            initial_revision,
            false,
            CandidateTypeProjection::Compatibility,
        )?;
        let initial_definition = self.function_definition(
            checked,
            initial_revision,
            false,
            CandidateTypeProjection::Durable,
        )?;
        let prepared_artifact = self.server_artifact(
            checked,
            &compatibility_definition,
            object_types,
            enum_types,
            record_value_types,
        )?;
        let initial_references =
            self.function_references(checked, function_id, initial_revision)?;
        let (revision_id, current_revision) =
            self.finalise_function_revision(FunctionFinalisation {
                checked: checked.id(),
                location: checked.location(),
                function: function_id,
                initial_revision,
                definition: &initial_definition,
                prepared_artifact,
                references: &initial_references,
            })?;
        let definition =
            self.function_definition(checked, revision_id, true, CandidateTypeProjection::Durable)?;
        let references =
            self.rebind_function_references(function_id, revision_id, &initial_references);
        self.push_function_origins(checked, function_id)?;
        self.functions.push(definition);
        self.current_function_revisions.push(current_revision);
        self.references.extend(references);
        Ok(())
    }

    fn build_client_function(&mut self, validated: &ValidatedClient) -> Result<(), PrepareError> {
        if matches!(&self.mode, PreparationMode::LegacyV1)
            && matches!(
                &validated.body,
                ValidatedClientBody::StateBlock { states, .. } if !states.is_empty()
            )
        {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "checked CLIENT state declarations require standard-backed preparation",
            });
        }

        let function_id = self.identities.function(validated.id)?;
        let initial_revision = self.initial_function_revision(validated.id, function_id)?;
        let initial_definition = self.client_function_definition(
            validated,
            initial_revision,
            false,
            CandidateTypeProjection::Durable,
        )?;
        let prepared_artifact = self.client_artifact(validated)?;
        let initial_references = if self.mode.signature_evidence().is_some()
            || matches!(self.mode, PreparationMode::Generic)
        {
            self.client_function_references(function_id, initial_revision, validated)?
        } else {
            Vec::new()
        };
        let (revision_id, current_revision) =
            self.finalise_function_revision(FunctionFinalisation {
                checked: validated.id,
                location: &validated.location,
                function: function_id,
                initial_revision,
                definition: &initial_definition,
                prepared_artifact,
                references: &initial_references,
            })?;

        let definition = self.client_function_definition(
            validated,
            revision_id,
            true,
            CandidateTypeProjection::Durable,
        )?;
        let references =
            self.rebind_function_references(function_id, revision_id, &initial_references);
        self.push_client_function_origins(validated, function_id)?;
        self.functions.push(definition);
        self.current_function_revisions.push(current_revision);
        self.references.extend(references);
        Ok(())
    }

    fn client_function_definition(
        &self,
        validated: &ValidatedClient,
        current_revision: FunctionRevisionId,
        consume_evidence: bool,
        projection: CandidateTypeProjection,
    ) -> Result<FunctionDefinition, PrepareError> {
        let return_type = self.client_return_type(validated, consume_evidence, projection)?;
        if let ValidatedClientBody::StateBlock { states, .. } = &validated.body {
            for (ordinal, state) in states.iter().enumerate() {
                let _ = self.declaration_type(
                    state.semantic_type(),
                    crate::CheckedTypeUseKind::State {
                        owner: validated.id,
                        ordinal: ordinal as u32,
                    },
                    consume_evidence,
                    projection,
                )?;
            }
        }
        let return_type = match validated.return_shape {
            CheckedClientReturnShape::Single => FunctionReturn::Single(return_type),
            CheckedClientReturnShape::Stream => FunctionReturn::Stream(return_type),
        };
        Ok(FunctionDefinition::new(
            self.identities.function(validated.id)?,
            validated.name.clone(),
            FunctionDomain::Client,
            validated
                .parameters
                .iter()
                .map(|parameter| {
                    Ok(ParameterDefinition::new(
                        self.identities.parameter(parameter.id())?,
                        parameter.name(),
                        parameter.ordinal(),
                        self.declaration_type(
                            parameter.semantic_type(),
                            crate::CheckedTypeUseKind::Parameter {
                                owner: validated.id,
                                parameter: parameter.id(),
                            },
                            consume_evidence,
                            projection,
                        )?,
                        None,
                    ))
                })
                .collect::<Result<Vec<_>, PrepareError>>()?,
            return_type,
            current_revision,
            validated.security,
            validated.transaction,
            validated.volatility,
        ))
    }
    fn client_artifact(
        &self,
        validated: &ValidatedClient,
    ) -> Result<PreparedFunctionArtifact, PrepareError> {
        let (version, payload, inner) = match &validated.body {
            ValidatedClientBody::BooleanLiteral(value) => {
                let plan = ClientPlan::return_boolean(*value);
                (
                    CLIENT_PLAN_VERSION,
                    plan.encode(),
                    InnerClientPlan::Boolean(plan),
                )
            }
            ValidatedClientBody::Expression(expression) => {
                let contains_action = client_expression_contains_action(expression);
                let contains_resource = client_expression_contains_resource(expression);
                let expression = self.client_expression_node(expression)?;
                if contains_action {
                    let plan = ActionClientPlan::new(match expression {
                        ClientExpressionNode::Action { operation } => operation,
                        _ => {
                            return Err(PrepareError::InvalidCheckedBundle {
                                reason: "checked CLIENT action expression is not a root action",
                            });
                        }
                    });
                    let payload =
                        plan.encode()
                            .map_err(|_| PrepareError::InvalidCheckedBundle {
                                reason: "checked CLIENT action plan exceeds client-plan limits",
                            })?;
                    (
                        CLIENT_PLAN_ACTION_VERSION,
                        payload,
                        InnerClientPlan::Action(plan),
                    )
                } else if contains_resource {
                    let plan = ResourceClientPlan::new(expression);
                    let payload =
                        plan.encode()
                            .map_err(|_| PrepareError::InvalidCheckedBundle {
                                reason: "checked CLIENT resource plan exceeds client-plan limits",
                            })?;
                    (
                        CLIENT_PLAN_RESOURCE_VERSION,
                        payload,
                        InnerClientPlan::Resource(plan),
                    )
                } else {
                    let plan = ExpressionClientPlan::new(expression);
                    let payload =
                        plan.encode()
                            .map_err(|_| PrepareError::InvalidCheckedBundle {
                                reason: "checked CLIENT expression exceeds client-plan limits",
                            })?;
                    (
                        plan.format_version(),
                        payload,
                        InnerClientPlan::Expression(plan),
                    )
                }
            }
            ValidatedClientBody::Procedural {
                locals,
                statements,
                return_expression,
            } => {
                if client_expression_contains_action(return_expression)
                    || statements
                        .iter()
                        .any(|statement| client_expression_contains_action(statement.expression()))
                {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: "checked CLIENT action is only supported in expression bodies",
                    });
                }
                let function_id = self.identities.function(validated.id)?;
                let local_ids: HashMap<u32, LocalId> = locals
                    .iter()
                    .map(|local| {
                        (
                            local.ordinal(),
                            durable_client_local_id(function_id, local.ordinal()),
                        )
                    })
                    .collect();
                let artifact_locals = locals
                    .iter()
                    .map(|local| {
                        let type_id = local
                            .standard_value_type()
                            .or_else(|| match local.semantic_type() {
                                SemanticType::Named(id)
                                | SemanticType::Reference { target: id } => {
                                    self.identities.type_id(id).ok()
                                }
                                SemanticType::Scalar(_) => None,
                            })
                            .ok_or(PrepareError::InvalidCheckedBundle {
                                reason: "checked CLIENT local has no durable value type identity",
                            })?;
                        let local_id = *local_ids.get(&local.ordinal()).ok_or(
                            PrepareError::InvalidCheckedBundle {
                                reason: "checked CLIENT local identity map is incomplete",
                            },
                        )?;
                        let kind = match local.kind() {
                            CheckedClientLocalKind::Value => ClientLocalKind::Value,
                            CheckedClientLocalKind::Resource(kind) => {
                                ClientLocalKind::Resource(kind)
                            }
                        };
                        Ok(ClientLocal::new(local_id, type_id, kind))
                    })
                    .collect::<Result<Vec<_>, PrepareError>>()?;
                let artifact_statements = statements
                    .iter()
                    .map(|statement| {
                        let local_id = *local_ids.get(&statement.local()).ok_or(
                            PrepareError::InvalidCheckedBundle {
                                reason: "checked CLIENT statement targets an unknown local",
                            },
                        )?;
                        let expression = self.client_expression_node_with_locals(
                            statement.expression(),
                            &local_ids,
                        )?;
                        Ok(match statement {
                            CheckedClientStatement::Let { .. } => {
                                ClientStatement::let_(local_id, expression)
                            }
                            CheckedClientStatement::Assignment { .. } => {
                                ClientStatement::assignment(local_id, expression)
                            }
                        })
                    })
                    .collect::<Result<Vec<_>, PrepareError>>()?;
                let return_expression =
                    self.client_expression_node_with_locals(return_expression, &local_ids)?;
                let plan = ProceduralClientPlan::new(
                    artifact_locals,
                    artifact_statements,
                    return_expression,
                );
                let payload = plan
                    .encode()
                    .map_err(|_| PrepareError::InvalidCheckedBundle {
                        reason: "checked CLIENT procedural plan exceeds client-plan limits",
                    })?;
                (
                    CLIENT_PLAN_PROCEDURAL_VERSION,
                    payload,
                    InnerClientPlan::Procedural(plan),
                )
            }
            ValidatedClientBody::ControlFlow { locals, statements } => {
                let function_id = self.identities.function(validated.id)?;
                let local_ids: HashMap<u32, LocalId> = locals
                    .iter()
                    .map(|local| {
                        (
                            local.ordinal(),
                            durable_client_local_id(function_id, local.ordinal()),
                        )
                    })
                    .collect();
                let artifact_locals = locals
                    .iter()
                    .map(|local| {
                        let type_id = local
                            .standard_value_type()
                            .or_else(|| match local.semantic_type() {
                                SemanticType::Named(id)
                                | SemanticType::Reference { target: id } => {
                                    self.client_named_type_id(id).ok()
                                }
                                SemanticType::Scalar(scalar) => {
                                    client_control_flow_scalar_type_id(scalar)
                                }
                            })
                            .ok_or(PrepareError::InvalidCheckedBundle {
                                reason: "checked CLIENT control-flow local has no durable value type identity",
                            })?;
                        let local_id = *local_ids.get(&local.ordinal()).ok_or(
                            PrepareError::InvalidCheckedBundle {
                                reason: "checked CLIENT control-flow local identity map is incomplete",
                            },
                        )?;
                        let kind = match local.kind() {
                            CheckedClientLocalKind::Value => ClientLocalKind::Value,
                            CheckedClientLocalKind::Resource(kind) => {
                                ClientLocalKind::Resource(kind)
                            }
                        };
                        Ok(ClientLocal::new(local_id, type_id, kind))
                    })
                    .collect::<Result<Vec<_>, PrepareError>>()?;
                let artifact_statements =
                    self.client_control_flow_statements(statements, &local_ids)?;
                let plan = ControlFlowClientPlan::new(artifact_locals, artifact_statements);
                let payload = plan
                    .encode()
                    .map_err(|_| PrepareError::InvalidCheckedBundle {
                        reason: "checked CLIENT control-flow plan exceeds client-plan limits",
                    })?;
                (
                    CLIENT_PLAN_CONTROL_FLOW_VERSION,
                    payload,
                    InnerClientPlan::ControlFlow(plan),
                )
            }

            ValidatedClientBody::StateBlock {
                return_expression,
                states,
            } => {
                let contains_action = client_expression_contains_action(return_expression);
                let contains_resource = client_expression_contains_resource(return_expression);
                let contains_inspect = client_expression_contains_inspect(return_expression);
                let expression = self.client_expression_node(return_expression)?;
                if states.is_empty() {
                    if contains_action {
                        let plan = ActionClientPlan::new(match expression {
                            ClientExpressionNode::Action { operation } => operation,
                            _ => {
                                return Err(PrepareError::InvalidCheckedBundle {
                                    reason: "checked CLIENT action expression is not a root action",
                                });
                            }
                        });
                        let payload =
                            plan.encode()
                                .map_err(|_| PrepareError::InvalidCheckedBundle {
                                    reason: "checked CLIENT action plan exceeds client-plan limits",
                                })?;
                        (
                            CLIENT_PLAN_ACTION_VERSION,
                            payload,
                            InnerClientPlan::Action(plan),
                        )
                    } else if contains_resource {
                        let plan = ResourceClientPlan::new(expression);
                        let payload = plan.encode().map_err(|_| {
                            PrepareError::InvalidCheckedBundle {
                                reason: "checked CLIENT resource plan exceeds client-plan limits",
                            }
                        })?;
                        (
                            CLIENT_PLAN_RESOURCE_VERSION,
                            payload,
                            InnerClientPlan::Resource(plan),
                        )
                    } else {
                        let plan = ExpressionClientPlan::new(expression);
                        let payload =
                            plan.encode()
                                .map_err(|_| PrepareError::InvalidCheckedBundle {
                                    reason: "checked CLIENT expression exceeds client-plan limits",
                                })?;
                        (
                            plan.format_version(),
                            payload,
                            InnerClientPlan::Expression(plan),
                        )
                    }
                } else {
                    if contains_action {
                        return Err(PrepareError::InvalidCheckedBundle {
                            reason: "checked CLIENT action is only supported in expression bodies",
                        });
                    }
                    if contains_inspect {
                        return Err(PrepareError::InvalidCheckedBundle {
                            reason: "checked CLIENT state block cannot contain Inspector expressions",
                        });
                    }
                    let function_id = self.identities.function(validated.id)?;
                    let slots = states
                        .iter()
                        .enumerate()
                        .map(|(ordinal, state)| {
                            self.client_state_slot(state, function_id, validated.id, ordinal as u32)
                        })
                        .collect::<Result<Vec<_>, PrepareError>>()?;
                    let plan = StateClientPlan::new(expression, slots);
                    let payload =
                        plan.encode()
                            .map_err(|_| PrepareError::InvalidCheckedBundle {
                                reason: "checked CLIENT state plan exceeds client-plan limits",
                            })?;
                    (
                        CLIENT_PLAN_STATE_VERSION,
                        payload,
                        InnerClientPlan::State(plan),
                    )
                }
            }
            ValidatedClientBody::ExternalContract(identity) => {
                let plan = ExpressionClientPlan::new(ClientExpressionNode::ExternalContract {
                    identity: identity.clone(),
                });
                let payload = plan
                    .encode()
                    .map_err(|_| PrepareError::InvalidCheckedBundle {
                        reason: "checked CLIENT external contract exceeds client-plan limits",
                    })?;
                (
                    CLIENT_PLAN_EXPRESSION_VERSION,
                    payload,
                    InnerClientPlan::Expression(plan),
                )
            }
        };
        // A function with checked capability requirements persists them in the
        // version-5 envelope around its inner plan; a function with none keeps
        // the exact version 1-4 artefact.
        let (version, payload) = if validated.capabilities.is_empty() {
            (version, payload)
        } else {
            let requirements = validated
                .capabilities
                .iter()
                .map(client_capability_requirement)
                .collect();
            let payload = CapabilityClientPlan::new(inner, requirements)
                .encode()
                .map_err(|_| PrepareError::InvalidCheckedBundle {
                    reason: "checked CLIENT capability plan exceeds client-plan limits",
                })?;
            (CLIENT_PLAN_CAPABILITY_VERSION, payload)
        };
        let hash = artifact_payload_digest(&payload)?;
        Ok(PreparedFunctionArtifact {
            artifact: ExecutableArtifact::new(
                ExecutableArtifactKind::Client,
                CLIENT_PLAN_FORMAT,
                version,
                payload,
                hash,
            )?,
            language_version: CLIENT_PLAN_LANGUAGE_VERSION.to_owned(),
        })
    }

    fn client_state_slot(
        &self,
        state: &CheckedClientStateSlot,
        function_id: FunctionId,
        owner: CheckedFunctionId,
        ordinal: u32,
    ) -> Result<StateSlot, PrepareError> {
        let expected_state_slot_id = durable_state_slot_id(function_id, state.name());
        let state_slot_id = match state.id() {
            crate::resolver::CheckedStateSlotId::Existing(id) if id == expected_state_slot_id => id,
            crate::resolver::CheckedStateSlotId::Existing(_) => {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "checked CLIENT state slot identity does not match its function and name",
                });
            }
            crate::resolver::CheckedStateSlotId::Provisional(_) => expected_state_slot_id,
        };
        let resolved_type = self.declaration_type(
            state.semantic_type(),
            crate::CheckedTypeUseKind::State { owner, ordinal },
            false,
            CandidateTypeProjection::Durable,
        )?;
        let type_id = resolved_type
            .value_type()
            .or_else(|| state.standard_value_type())
            .or_else(|| match state.semantic_type() {
                SemanticType::Named(id) | SemanticType::Reference { target: id } => {
                    self.identities.type_id(id).ok()
                }
                SemanticType::Scalar(_) => None,
            })
            .ok_or(PrepareError::InvalidCheckedBundle {
                reason: "checked CLIENT state slot has no durable value type identity",
            })?;
        let scope = match state.scope() {
            CheckedStateScope::Local => StateScope::Local,
            CheckedStateScope::Session => StateScope::Session,
            CheckedStateScope::User => StateScope::User,
        };
        let default = match state.default() {
            CheckedStateDefault::Unset => StateDefault::Unset,
            CheckedStateDefault::Null => StateDefault::Null,
            CheckedStateDefault::Expression(expression) => {
                if client_expression_contains_inspect(expression) {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: "checked CLIENT state default cannot contain Inspector expressions",
                    });
                }
                StateDefault::Expression(self.client_expression_node(expression)?)
            }
        };
        Ok(StateSlot::new(state_slot_id, type_id, scope, default))
    }

    fn client_expression_node(
        &self,
        expression: &CheckedClientExpression,
    ) -> Result<ClientExpressionNode, PrepareError> {
        self.client_expression_node_with_locals(expression, &HashMap::new())
    }

    fn client_expression_node_with_locals(
        &self,
        expression: &CheckedClientExpression,
        local_ids: &HashMap<u32, LocalId>,
    ) -> Result<ClientExpressionNode, PrepareError> {
        Ok(match expression {
            CheckedClientExpression::Call {
                function,
                arguments,
                ..
            } => ClientExpressionNode::Call {
                function: self.identities.function(*function)?,
                arguments: arguments
                    .iter()
                    .map(|(parameter, value)| {
                        Ok((
                            self.identities.parameter(*parameter)?,
                            self.client_expression_node_with_locals(value, local_ids)?,
                        ))
                    })
                    .collect::<Result<Vec<_>, PrepareError>>()?,
            },
            CheckedClientExpression::Await { expression, .. } => ClientExpressionNode::Await {
                expression: Box::new(
                    self.client_expression_node_with_locals(expression, local_ids)?,
                ),
            },
            CheckedClientExpression::Resource { operation } => ClientExpressionNode::Resource {
                operation: self.client_resource_operation(operation)?,
            },
            CheckedClientExpression::Action { operation } => ClientExpressionNode::Action {
                operation: self.client_action_operation(operation)?,
            },
            CheckedClientExpression::Inspect { operation } => {
                let operation = match operation {
                    CheckedInspectOperation::Snapshot {
                        target, options, ..
                    } => {
                        if options.is_some() {
                            return Err(PrepareError::InvalidCheckedBundle {
                                reason: "checked Inspector snapshot options are unsupported in Inspector v1",
                            });
                        }
                        InspectOperationNode::snapshot(
                            self.client_expression_node_with_locals(target, local_ids)?,
                        )
                    }
                    CheckedInspectOperation::Projection {
                        projection,
                        snapshot,
                        ..
                    } => {
                        let projection = match projection {
                            CheckedInspectProjection::InvocationNodes => {
                                InspectProjection::InvocationNodes
                            }
                            CheckedInspectProjection::Calls => InspectProjection::Calls,
                            CheckedInspectProjection::Resources => InspectProjection::Resources,
                            CheckedInspectProjection::StateCells => InspectProjection::StateCells,
                            CheckedInspectProjection::UiNodes => InspectProjection::UiNodes,
                            CheckedInspectProjection::PresentationCandidates => {
                                InspectProjection::PresentationCandidates
                            }
                            CheckedInspectProjection::RuntimeBindings => {
                                InspectProjection::RuntimeBindings
                            }
                            CheckedInspectProjection::SecurityDecisions => {
                                InspectProjection::SecurityDecisions
                            }
                        };
                        InspectOperationNode::Projection {
                            projection,
                            snapshot: Box::new(
                                self.client_expression_node_with_locals(snapshot, local_ids)?,
                            ),
                        }
                    }
                };
                ClientExpressionNode::Inspect { operation }
            }
            CheckedClientExpression::String { value, .. } => ClientExpressionNode::String {
                value: value.clone(),
            },
            CheckedClientExpression::Integer { value, .. } => {
                ClientExpressionNode::Integer { value: *value }
            }
            CheckedClientExpression::Boolean { value, .. } => {
                ClientExpressionNode::Boolean { value: *value }
            }
            CheckedClientExpression::ParameterRead { parameter, .. } => {
                ClientExpressionNode::ParameterRead {
                    parameter: self.identities.parameter(*parameter)?,
                }
            }
            CheckedClientExpression::LocalRead { local, .. } => {
                let local =
                    local_ids
                        .get(local)
                        .copied()
                        .ok_or(PrepareError::InvalidCheckedBundle {
                            reason: "checked CLIENT local read has no durable local identity",
                        })?;
                ClientExpressionNode::LocalRead { local }
            }
            CheckedClientExpression::FieldPath { root, fields, .. } => {
                ClientExpressionNode::FieldPath {
                    root: self.identities.parameter(*root)?,
                    fields: fields
                        .iter()
                        .map(|field| self.identities.field(*field))
                        .collect::<Result<Vec<_>, PrepareError>>()?,
                }
            }
            CheckedClientExpression::SourceIntrospection { .. } => {
                ClientExpressionNode::SourceIntrospection
            }
            CheckedClientExpression::Input { .. } => ClientExpressionNode::Input,
            CheckedClientExpression::Evaluate { expression, .. } => {
                ClientExpressionNode::Evaluate {
                    expression: Box::new(
                        self.client_expression_node_with_locals(expression, local_ids)?,
                    ),
                }
            }
            CheckedClientExpression::Unary {
                operator,
                expression,
                ..
            } => ClientExpressionNode::Unary {
                operator: *operator,
                expression: Box::new(
                    self.client_expression_node_with_locals(expression, local_ids)?,
                ),
            },
            CheckedClientExpression::Binary {
                operator,
                left,
                right,
                ..
            } => ClientExpressionNode::Binary {
                operator: *operator,
                left: Box::new(self.client_expression_node_with_locals(left, local_ids)?),
                right: Box::new(self.client_expression_node_with_locals(right, local_ids)?),
            },
            CheckedClientExpression::Parenthesized { expression, .. } => {
                self.client_expression_node_with_locals(expression, local_ids)?
            }

            CheckedClientExpression::Concat { left, right, .. } => ClientExpressionNode::Concat {
                left: Box::new(self.client_expression_node_with_locals(left, local_ids)?),
                right: Box::new(self.client_expression_node_with_locals(right, local_ids)?),
            },
        })
    }

    fn client_control_flow_statements(
        &self,
        statements: &[CheckedClientControlFlowStatement],
        local_ids: &HashMap<u32, LocalId>,
    ) -> Result<Vec<ControlFlowStatement>, PrepareError> {
        statements
            .iter()
            .map(|statement| self.client_control_flow_statement(statement, local_ids))
            .collect()
    }

    fn client_control_flow_statement(
        &self,
        statement: &CheckedClientControlFlowStatement,
        local_ids: &HashMap<u32, LocalId>,
    ) -> Result<ControlFlowStatement, PrepareError> {
        match statement {
            CheckedClientControlFlowStatement::Let {
                local, expression, ..
            } => Ok(ControlFlowStatement::let_(
                *local_ids
                    .get(local)
                    .ok_or(PrepareError::InvalidCheckedBundle {
                        reason: "checked CLIENT control-flow LET targets an unknown local",
                    })?,
                self.client_expression_node_with_locals(expression, local_ids)?,
            )),
            CheckedClientControlFlowStatement::Assignment {
                local, expression, ..
            } => Ok(ControlFlowStatement::assignment(
                *local_ids
                    .get(local)
                    .ok_or(PrepareError::InvalidCheckedBundle {
                        reason: "checked CLIENT control-flow assignment targets an unknown local",
                    })?,
                self.client_expression_node_with_locals(expression, local_ids)?,
            )),
            CheckedClientControlFlowStatement::Return { expression, .. } => {
                let expression = expression
                    .as_ref()
                    .map(|expression| {
                        self.client_expression_node_with_locals(expression, local_ids)
                    })
                    .transpose()?;
                Ok(ControlFlowStatement::return_(expression))
            }
            CheckedClientControlFlowStatement::If {
                branches,
                else_statements,
                ..
            } => {
                let branches = branches
                    .iter()
                    .map(|branch| {
                        Ok(ControlFlowIfBranch::new(
                            self.client_expression_node_with_locals(branch.condition(), local_ids)?,
                            self.client_control_flow_statements(branch.statements(), local_ids)?,
                        ))
                    })
                    .collect::<Result<Vec<_>, PrepareError>>()?;
                let else_statements = else_statements
                    .as_ref()
                    .map(|statements| self.client_control_flow_statements(statements, local_ids))
                    .transpose()?;
                Ok(ControlFlowStatement::if_(ControlFlowIfStatement::new(
                    branches,
                    else_statements,
                )))
            }
            CheckedClientControlFlowStatement::While {
                condition,
                statements,
                ..
            } => Ok(ControlFlowStatement::while_(
                ControlFlowWhileStatement::new(
                    self.client_expression_node_with_locals(condition, local_ids)?,
                    self.client_control_flow_statements(statements, local_ids)?,
                ),
            )),
        }
    }

    fn resource_target_revision(
        &self,
        target: CheckedFunctionId,
        function: FunctionId,
    ) -> Result<RevisionPair, PrepareError> {
        // Every supported target is installed with this candidate pair. The
        // active and verified-standard catalogues below only establish that an
        // unchanged target remains resolvable after application.
        if self
            .checked
            .server_functions()
            .iter()
            .any(|candidate| candidate.id() == target)
            || self
                .checked
                .client_functions()
                .iter()
                .any(|candidate| candidate.id() == target)
        {
            return Ok(RevisionPair::new(
                self.source.revision.id(),
                self.catalogue_revision,
            ));
        }
        if self.active.catalogue().function_by_id(function).is_some()
            || self
                .active
                .catalogue_hash_context()
                .standard()
                .is_some_and(|standard| standard.catalogue().function_by_id(function).is_some())
        {
            return Ok(RevisionPair::new(
                self.source.revision.id(),
                self.catalogue_revision,
            ));
        }
        Err(existing_mismatch(DefinitionIdentity::Function(function)))
    }
    fn action_target_revision(
        &self,
        target: CheckedFunctionId,
        function: FunctionId,
    ) -> Result<RevisionPair, PrepareError> {
        // Actions are installed with the candidate. An unchanged active
        // target therefore uses the candidate pair, not the pair that was
        // active while the CLIENT source was checked.
        if self
            .checked
            .server_functions()
            .iter()
            .any(|candidate| candidate.id() == target)
            || self
                .checked
                .client_functions()
                .iter()
                .any(|candidate| candidate.id() == target)
            || self.active.catalogue().function_by_id(function).is_some()
            || self
                .active
                .catalogue_hash_context()
                .standard()
                .is_some_and(|standard| standard.catalogue().function_by_id(function).is_some())
        {
            return Ok(RevisionPair::new(
                self.source.revision.id(),
                self.catalogue_revision,
            ));
        }
        Err(existing_mismatch(DefinitionIdentity::Function(function)))
    }

    fn client_action_operation(
        &self,
        operation: &CheckedActionOperation,
    ) -> Result<ActionOperationNode, PrepareError> {
        let result_type = match operation.standard_result_type() {
            Some(type_id) => type_id,
            None => match operation.result_type() {
                SemanticType::Named(type_id) | SemanticType::Reference { target: type_id } => {
                    self.client_named_type_id(type_id)?
                }
                SemanticType::Scalar(_) => {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: "checked CLIENT action result has no durable value type identity",
                    });
                }
            },
        };
        let mut arguments = operation
            .arguments()
            .iter()
            .map(|(parameter, value)| {
                Ok((
                    self.identities.parameter(*parameter)?,
                    self.client_expression_node(value)?,
                ))
            })
            .collect::<Result<Vec<_>, PrepareError>>()?;
        arguments.sort_by_key(|(parameter, _)| *parameter);
        let target = self.identities.function(operation.target())?;
        let target_revision = self.action_target_revision(operation.target(), target)?;
        Ok(ActionOperationNode::new(
            operation.target_domain(),
            target,
            target_revision,
            operation.call_site(),
            arguments,
            result_type,
        ))
    }

    fn client_resource_operation(
        &self,
        operation: &CheckedResourceOperation,
    ) -> Result<ResourceOperationNode, PrepareError> {
        let result_type = match operation.standard_result_type() {
            Some(type_id) => type_id,
            None => match operation.result_type() {
                SemanticType::Named(type_id) | SemanticType::Reference { target: type_id } => {
                    self.client_named_type_id(type_id)?
                }
                SemanticType::Scalar(_) => {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: "checked CLIENT resource result has no durable value type identity",
                    });
                }
            },
        };
        let mut arguments = operation
            .arguments()
            .iter()
            .map(|(parameter, value)| {
                Ok((
                    self.identities.parameter(*parameter)?,
                    self.client_expression_node(value)?,
                ))
            })
            .collect::<Result<Vec<_>, PrepareError>>()?;
        // The artifact contract is ordered by durable ParameterId. Resolver
        // identities may be provisional and declaration order is not a valid
        // substitute once the identity map allocates durable IDs.
        arguments.sort_by_key(|(parameter, _)| *parameter);
        let target = self.identities.function(operation.target())?;
        let target_is_server = self
            .checked
            .server_functions()
            .iter()
            .any(|candidate| candidate.id() == operation.target())
            || self
                .active
                .catalogue()
                .function_by_id(target)
                .is_some_and(|candidate| candidate.domain() == FunctionDomain::Server)
            || self
                .active
                .catalogue_hash_context()
                .standard()
                .is_some_and(|standard| {
                    standard
                        .catalogue()
                        .function_by_id(target)
                        .is_some_and(|candidate| candidate.domain() == FunctionDomain::Server)
                });
        if !target_is_server {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "checked CLIENT resource target is not a SERVER function",
            });
        }
        let target_revision = self.resource_target_revision(operation.target(), target)?;
        Ok(ResourceOperationNode::new(
            operation.kind(),
            target,
            target_revision,
            operation.call_site(),
            arguments,
            result_type,
        ))
    }
    fn client_named_type_id(&self, id: CheckedTypeId) -> Result<TypeId, PrepareError> {
        if let CheckedTypeId::Existing(type_id) = id
            && is_sealed_inspect_type_id(type_id)
        {
            return Ok(type_id);
        }
        if let CheckedTypeId::Existing(type_id) = id
            && self
                .mode
                .durable_standard_catalogue()
                .is_some_and(|catalogue| catalogue.value_type_by_id(type_id).is_some())
        {
            return Ok(type_id);
        }
        self.identities.type_id(id)
    }

    fn client_call_reference_sequence(
        &self,
        body: &ValidatedClientBody,
    ) -> Result<Vec<(CheckedFunctionId, SourceLocation)>, PrepareError> {
        let mut calls = Vec::new();
        match body {
            ValidatedClientBody::BooleanLiteral(_) | ValidatedClientBody::ExternalContract(_) => {}
            ValidatedClientBody::Expression(expression) => {
                self.append_client_expression_call_references(expression, &mut calls)?;
            }
            ValidatedClientBody::Procedural {
                statements,
                return_expression,
                ..
            } => {
                for statement in statements {
                    self.append_client_expression_call_references(
                        statement.expression(),
                        &mut calls,
                    )?;
                }
                self.append_client_expression_call_references(return_expression, &mut calls)?;
            }
            ValidatedClientBody::ControlFlow { statements, .. } => {
                self.append_client_control_flow_call_references(statements, &mut calls)?;
            }

            ValidatedClientBody::StateBlock {
                return_expression,
                states,
            } => {
                for state in states {
                    if let CheckedStateDefault::Expression(expression) = state.default() {
                        self.append_client_expression_call_references(expression, &mut calls)?;
                    }
                }
                self.append_client_expression_call_references(return_expression, &mut calls)?;
            }
        }
        Ok(calls)
    }

    fn append_client_operation_call_references(
        &self,
        arguments: &[(CheckedParameterId, CheckedClientExpression)],
        calls: &mut Vec<(CheckedFunctionId, SourceLocation)>,
    ) -> Result<(), PrepareError> {
        let mut ordered = arguments
            .iter()
            .map(|(parameter, expression)| Ok((self.identities.parameter(*parameter)?, expression)))
            .collect::<Result<Vec<_>, PrepareError>>()?;
        ordered.sort_by_key(|(parameter, _)| *parameter);
        for (_, expression) in ordered {
            self.append_client_expression_call_references(expression, calls)?;
        }
        Ok(())
    }

    fn append_client_expression_call_references(
        &self,
        expression: &CheckedClientExpression,
        calls: &mut Vec<(CheckedFunctionId, SourceLocation)>,
    ) -> Result<(), PrepareError> {
        match expression {
            CheckedClientExpression::Call {
                function,
                arguments,
                location,
            } => {
                for (_, argument) in arguments {
                    self.append_client_expression_call_references(argument, calls)?;
                }
                calls.push((*function, location.clone()));
            }
            CheckedClientExpression::Await { expression, .. }
            | CheckedClientExpression::Unary { expression, .. }
            | CheckedClientExpression::Parenthesized { expression, .. }
            | CheckedClientExpression::Evaluate { expression, .. } => {
                self.append_client_expression_call_references(expression, calls)?;
            }
            CheckedClientExpression::Resource { operation } => {
                self.append_client_operation_call_references(operation.arguments(), calls)?;
                calls.push((operation.target(), operation.location().clone()));
            }
            CheckedClientExpression::Action { operation } => {
                self.append_client_operation_call_references(operation.arguments(), calls)?;
                calls.push((operation.target(), operation.location().clone()));
            }
            CheckedClientExpression::Inspect { operation } => match operation {
                CheckedInspectOperation::Snapshot {
                    target, options, ..
                } => {
                    self.append_client_expression_call_references(target, calls)?;
                    if let Some(options) = options {
                        self.append_client_expression_call_references(options, calls)?;
                    }
                }
                CheckedInspectOperation::Projection { snapshot, .. } => {
                    self.append_client_expression_call_references(snapshot, calls)?;
                }
            },
            CheckedClientExpression::Concat { left, right, .. }
            | CheckedClientExpression::Binary { left, right, .. } => {
                self.append_client_expression_call_references(left, calls)?;
                self.append_client_expression_call_references(right, calls)?;
            }
            CheckedClientExpression::Input { .. }
            | CheckedClientExpression::SourceIntrospection { .. }
            | CheckedClientExpression::String { .. }
            | CheckedClientExpression::Integer { .. }
            | CheckedClientExpression::Boolean { .. }
            | CheckedClientExpression::ParameterRead { .. }
            | CheckedClientExpression::LocalRead { .. }
            | CheckedClientExpression::FieldPath { .. } => {}
        }
        Ok(())
    }

    fn append_client_control_flow_call_references(
        &self,
        statements: &[CheckedClientControlFlowStatement],
        calls: &mut Vec<(CheckedFunctionId, SourceLocation)>,
    ) -> Result<(), PrepareError> {
        for statement in statements {
            match statement {
                CheckedClientControlFlowStatement::Let { expression, .. }
                | CheckedClientControlFlowStatement::Assignment { expression, .. } => {
                    self.append_client_expression_call_references(expression, calls)?;
                }
                CheckedClientControlFlowStatement::Return { expression, .. } => {
                    if let Some(expression) = expression {
                        self.append_client_expression_call_references(expression, calls)?;
                    }
                }
                CheckedClientControlFlowStatement::If {
                    branches,
                    else_statements,
                    ..
                } => {
                    for branch in branches {
                        self.append_client_expression_call_references(branch.condition(), calls)?;
                        self.append_client_control_flow_call_references(
                            branch.statements(),
                            calls,
                        )?;
                    }
                    if let Some(statements) = else_statements {
                        self.append_client_control_flow_call_references(statements, calls)?;
                    }
                }
                CheckedClientControlFlowStatement::While {
                    condition,
                    statements,
                    ..
                } => {
                    self.append_client_expression_call_references(condition, calls)?;
                    self.append_client_control_flow_call_references(statements, calls)?;
                }
            }
        }
        Ok(())
    }

    fn reorder_client_call_references(
        &self,
        validated: &ValidatedClient,
        remaining_references: &mut Vec<&crate::CheckedDefinitionReference>,
    ) -> Result<(), PrepareError> {
        let sequence = self.client_call_reference_sequence(&validated.body)?;
        let call_slots = remaining_references
            .iter()
            .enumerate()
            .filter_map(|(index, reference)| {
                (reference.kind() == DefinitionReferenceKind::FunctionCall).then_some(index)
            })
            .collect::<Vec<_>>();
        if call_slots.len() != sequence.len() {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "checked CLIENT function-call references do not match artifact calls",
            });
        }

        let mut used_slots = HashSet::with_capacity(call_slots.len());
        let mut ordered_references = Vec::with_capacity(sequence.len());
        for (target, location) in sequence {
            let expected_target = CheckedDefinitionReferenceTarget::Function(target);
            let Some(slot) = call_slots.iter().copied().find(|slot| {
                !used_slots.contains(slot)
                    && remaining_references[*slot].target() == expected_target
                    && remaining_references[*slot].location() == &location
            }) else {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "checked CLIENT artifact call has no exact definition reference",
                });
            };
            used_slots.insert(slot);
            ordered_references.push(remaining_references[slot]);
        }

        for (slot, reference) in call_slots.into_iter().zip(ordered_references) {
            remaining_references[slot] = reference;
        }
        Ok(())
    }

    fn client_function_references(
        &self,
        function: FunctionId,
        revision: FunctionRevisionId,
        validated: &ValidatedClient,
    ) -> Result<Vec<DefinitionReference>, PrepareError> {
        let mut references =
            Vec::with_capacity(validated.references.len() + validated.parameters.len() + 1);
        let mut remaining_references = validated.references.iter().collect::<Vec<_>>();
        if let Some(signature_evidence) = self.mode.signature_evidence() {
            for signature_slot in signature_evidence.function_slots(validated.id) {
                let ordinal = u32::try_from(references.len()).map_err(|_| {
                    PrepareError::ReferenceCountExceedsU32 {
                        function: validated.id,
                        count: references.len(),
                    }
                })?;
                if signature_slot.flattened_ordinal != ordinal {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: "checked standard CLIENT signature has a non-contiguous slot sequence",
                    });
                }
                let (target, kind, origin) = match signature_slot.target {
                    EvidenceTarget::Value(target) => (
                        DefinitionReferenceTarget::ValueType(target),
                        DefinitionReferenceKind::NamedType,
                        self.source.origin(&signature_slot.location)?,
                    ),
                    EvidenceTarget::Named(target) => (
                        DefinitionReferenceTarget::ValueType(self.client_named_type_id(target)?),
                        DefinitionReferenceKind::NamedType,
                        self.source.origin(&signature_slot.location)?,
                    ),
                    EvidenceTarget::ObjectReference(target) => {
                        let target = CheckedDefinitionReferenceTarget::ObjectType(target);
                        let Some(index) = remaining_references.iter().position(|reference| {
                            reference.target() == target
                                && reference.kind() == DefinitionReferenceKind::ObjectReference
                                && reference.location() == &signature_slot.location
                        }) else {
                            return Err(PrepareError::InvalidCheckedBundle {
                                reason: "checked standard CLIENT object signature has no exact definition reference",
                            });
                        };
                        let reference = remaining_references.remove(index);
                        (
                            self.identities.reference_target(reference.target())?,
                            reference.kind(),
                            self.source.origin(reference.location())?,
                        )
                    }
                    EvidenceTarget::Unknown => {
                        return Err(PrepareError::InvalidCheckedBundle {
                            reason: "checked standard CLIENT signature has an unknown declaration use",
                        });
                    }
                };
                references.push(DefinitionReference::new(
                    function, revision, ordinal, target, kind, origin,
                ));
            }
        } else if !matches!(validated.return_target, EvidenceTarget::Unknown) {
            let return_target = match validated.return_target {
                EvidenceTarget::Value(type_id) => DefinitionReferenceTarget::ValueType(type_id),
                EvidenceTarget::Named(type_id) => {
                    DefinitionReferenceTarget::ValueType(self.client_named_type_id(type_id)?)
                }
                EvidenceTarget::ObjectReference(type_id) => {
                    DefinitionReferenceTarget::ObjectType(self.identities.type_id(type_id)?)
                }
                EvidenceTarget::Unknown => unreachable!(),
            };
            references.push(DefinitionReference::new(
                function,
                revision,
                0,
                return_target,
                if matches!(validated.return_target, EvidenceTarget::ObjectReference(_)) {
                    DefinitionReferenceKind::ObjectReference
                } else {
                    DefinitionReferenceKind::NamedType
                },
                self.source.origin(&validated.return_location)?,
            ));
        }
        self.reorder_client_call_references(validated, &mut remaining_references)?;
        for reference in remaining_references {
            let ordinal = u32::try_from(references.len()).map_err(|_| {
                PrepareError::ReferenceCountExceedsU32 {
                    function: validated.id,
                    count: validated.references.len() + validated.parameters.len() + 1,
                }
            })?;
            references.push(DefinitionReference::new(
                function,
                revision,
                ordinal,
                self.identities.reference_target(reference.target())?,
                reference.kind(),
                self.source.origin(reference.location())?,
            ));
        }
        Ok(references)
    }

    fn initial_function_revision(
        &self,
        checked: CheckedFunctionId,
        function: FunctionId,
    ) -> Result<FunctionRevisionId, PrepareError> {
        match checked {
            CheckedFunctionId::Existing(_) => self
                .active
                .catalogue()
                .function_by_id(function)
                .ok_or(existing_mismatch(DefinitionIdentity::Function(function)))
                .map(|definition| definition.current_revision()),
            CheckedFunctionId::Provisional(_) => Ok(FunctionRevisionId::new()),
        }
    }

    fn finalise_function_revision(
        &mut self,
        input: FunctionFinalisation<'_>,
    ) -> Result<(FunctionRevisionId, FunctionRevisionRecord), PrepareError> {
        let FunctionFinalisation {
            checked,
            location,
            function,
            initial_revision,
            definition,
            prepared_artifact,
            references,
        } = input;
        let semantic_hash_version = self.mode.semantic_hash_version(references);
        let calculated = FunctionRevisionPlan::new(
            self.active,
            function,
            FunctionRevisionPlanInput {
                semantic_hash_version,
                definition,
                language_version: &prepared_artifact.language_version,
                artifact: &prepared_artifact.artifact,
                expressions: &self.expressions,
                references,
                current_only: matches!(self.mode, PreparationMode::StandardV1Match { .. }),
                reuse_policy: if matches!(self.mode, PreparationMode::LegacyV1) {
                    FunctionRevisionReusePolicy::SemanticHashOnly
                } else {
                    FunctionRevisionReusePolicy::Complete
                },
            },
        )?;
        if matches!(self.mode, PreparationMode::StandardV1Match { .. }) {
            let current = self
                .active
                .function_revisions()
                .iter()
                .find(|revision| {
                    revision.function() == function && revision.id() == initial_revision
                })
                .ok_or(PrepareError::InvalidCheckedBundle {
                    reason: "matched active source has no current function revision",
                })?;
            let declaration_origin = self.source.origin(location)?;
            let declaration = self.source.declaration(self.parse_report, location)?;
            let expected = FunctionRevisionRecord::new(
                function,
                initial_revision,
                current.revision_number(),
                declaration_origin,
                function_declaration_digest(declaration)?,
                calculated.semantic_hash,
                prepared_artifact.language_version,
                prepared_artifact.artifact,
            )?
            .with_semantic_hash_version(semantic_hash_version);
            return Ok((expected.id(), expected));
        }
        let plan = calculated;
        if let Some(revision) = plan.reusable {
            return Ok((revision.id(), revision));
        }
        let revision_id = match checked {
            CheckedFunctionId::Existing(_) => FunctionRevisionId::new(),
            CheckedFunctionId::Provisional(_) => initial_revision,
        };
        let revision_number =
            plan.next_revision_number
                .ok_or(PrepareError::InvalidCheckedBundle {
                    reason: "checked standard function has no validated next revision number",
                })?;
        let declaration_origin = self.source.origin(location)?;
        let declaration = self.source.declaration(self.parse_report, location)?;
        let revision = FunctionRevisionRecord::new(
            function,
            revision_id,
            revision_number,
            declaration_origin,
            function_declaration_digest(declaration)?,
            plan.semantic_hash,
            prepared_artifact.language_version,
            prepared_artifact.artifact,
        )?
        .with_semantic_hash_version(plan.semantic_hash_version);
        self.new_function_revisions.push(revision.clone());
        Ok((revision_id, revision))
    }

    fn rebind_function_references(
        &self,
        function: FunctionId,
        revision: FunctionRevisionId,
        references: &[DefinitionReference],
    ) -> Vec<DefinitionReference> {
        references
            .iter()
            .map(|reference| {
                DefinitionReference::new(
                    function,
                    revision,
                    reference.ordinal(),
                    reference.target(),
                    reference.kind(),
                    reference.source_origin(),
                )
            })
            .collect()
    }

    fn validated_client(&self, owner: CheckedFunctionId) -> Result<ValidatedClient, PrepareError> {
        if matches!(self.mode, PreparationMode::Generic) {
            let checked = self
                .checked
                .client_functions()
                .iter()
                .find(|function| function.id() == owner)
                .ok_or(PrepareError::InvalidCheckedBundle {
                    reason: "checked CLIENT function is absent from the checked bundle",
                })?;
            return validate_generic_client_function(checked, self.active);
        }
        let Some(standard_preflight) = self.mode.standard_preflight() else {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "checked CLIENT function requires standard preparation evidence",
            });
        };
        standard_preflight
            .clients
            .get(&owner)
            .cloned()
            .ok_or(PrepareError::InvalidCheckedBundle {
                reason: "checked CLIENT function has no exact validated return evidence",
            })
    }

    fn client_return_type(
        &self,
        validated: &ValidatedClient,
        consume_evidence: bool,
        projection: CandidateTypeProjection,
    ) -> Result<ResolvedType, PrepareError> {
        Ok(self.mode.lower_candidate_type(
            self.client_candidate_return_type(validated, consume_evidence)?,
            projection,
        ))
    }

    fn client_candidate_return_type(
        &self,
        validated: &ValidatedClient,
        consume_evidence: bool,
    ) -> Result<CandidateResolvedType, PrepareError> {
        if matches!(self.mode, PreparationMode::Generic) {
            return match validated.return_semantic_type {
                SemanticType::Scalar(_) => Ok(CandidateResolvedType::LegacyScalar(
                    validated.return_scalar.ok_or(PrepareError::InvalidCheckedBundle {
                        reason: "checked CLIENT scalar return has no compatibility type",
                    })?,
                )),
                SemanticType::Named(target) => Ok(CandidateResolvedType::Named(
                    self.client_named_type_id(target)?,
                )),
                SemanticType::Reference { target } => Ok(CandidateResolvedType::Reference(
                    self.identities.type_id(target)?,
                )),
            };
        }
        let evidence = if let Some(declaration_evidence) = &self.declaration_evidence {
            let kind = crate::CheckedTypeUseKind::Return {
                owner: validated.id,
                ordinal: 0,
            };
            if consume_evidence {
                declaration_evidence.borrow_mut().consume(kind)?
            } else {
                declaration_evidence.borrow().lookup(kind)?
            }
        } else {
            EvidenceUse {
                kind: crate::CheckedTypeUseKind::Return {
                    owner: validated.id,
                    ordinal: 0,
                },
                target: validated.return_target.clone(),
                location: validated.return_location.clone(),
            }
        };
        if evidence.target != validated.return_target
            || evidence.location != validated.return_location
        {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "checked CLIENT function return evidence does not match its validated slot",
            });
        }
        match (
            validated.return_semantic_type,
            validated.return_target.clone(),
        ) {
            (SemanticType::Scalar(_compatibility), EvidenceTarget::Value(type_id)) => {
                Ok(CandidateResolvedType::StandardValue {
                    type_id,
                    compatibility: validated.return_scalar.ok_or(
                        PrepareError::InvalidCheckedBundle {
                            reason: "checked CLIENT scalar return has no compatibility type",
                        },
                    )?,
                })
            }
            (SemanticType::Named(target), EvidenceTarget::Named(evidence))
                if target == evidence =>
            {
                if let CheckedTypeId::Existing(type_id) = target
                    && (is_sealed_inspect_type_id(type_id)
                        || self
                            .mode
                            .durable_standard_catalogue()
                            .is_some_and(|catalogue| catalogue.value_type_by_id(type_id).is_some()))
                {
                    Ok(CandidateResolvedType::StandardOpaqueValue(type_id))
                } else {
                    Ok(CandidateResolvedType::Named(
                        self.client_named_type_id(target)?,
                    ))
                }
            }
            (SemanticType::Reference { target }, EvidenceTarget::ObjectReference(evidence))
                if target == evidence =>
            {
                Ok(CandidateResolvedType::Reference(
                    self.identities.type_id(target)?,
                ))
            }
            _ => Err(PrepareError::InvalidCheckedBundle {
                reason: "checked CLIENT function return evidence does not match its semantic type",
            }),
        }
    }

    fn function_definition(
        &self,
        checked: &crate::CheckedServerFunction,
        current_revision: FunctionRevisionId,
        consume_evidence: bool,
        projection: CandidateTypeProjection,
    ) -> Result<FunctionDefinition, PrepareError> {
        let function_id = self.identities.function(checked.id())?;
        let parameters = checked
            .parameters()
            .iter()
            .map(|parameter| {
                Ok(ParameterDefinition::new(
                    self.identities.parameter(parameter.id())?,
                    parameter.name(),
                    parameter.ordinal(),
                    self.declaration_type(
                        parameter.semantic_type(),
                        crate::CheckedTypeUseKind::Parameter {
                            owner: checked.id(),
                            parameter: parameter.id(),
                        },
                        consume_evidence,
                        projection,
                    )?,
                    None,
                ))
            })
            .collect::<Result<Vec<_>, PrepareError>>()?;
        let return_type = match checked.return_type() {
            crate::resolver::CheckedServerFunctionReturn::Single { semantic_type, .. } => {
                FunctionReturn::Single(self.declaration_type(
                    *semantic_type,
                    crate::CheckedTypeUseKind::Return {
                        owner: checked.id(),
                        ordinal: 0,
                    },
                    consume_evidence,
                    projection,
                )?)
            }
            crate::resolver::CheckedServerFunctionReturn::Rows(columns) => {
                let return_columns = columns
                    .iter()
                    .map(|column| {
                        Ok(FunctionReturnColumnDefinition::new(
                            column.name(),
                            column.ordinal(),
                            self.declaration_type(
                                column.semantic_type(),
                                crate::CheckedTypeUseKind::Return {
                                    owner: checked.id(),
                                    ordinal: column.ordinal(),
                                },
                                consume_evidence,
                                projection,
                            )?,
                        ))
                    })
                    .collect::<Result<Vec<_>, PrepareError>>()?;
                FunctionReturn::Rows(return_columns)
            }
            crate::resolver::CheckedServerFunctionReturn::Stream { semantic_type, .. } => {
                FunctionReturn::Stream(self.declaration_type(
                    *semantic_type,
                    crate::CheckedTypeUseKind::Return {
                        owner: checked.id(),
                        ordinal: 0,
                    },
                    consume_evidence,
                    projection,
                )?)
            }
        };

        Ok(FunctionDefinition::new(
            function_id,
            checked.name().clone(),
            FunctionDomain::Server,
            parameters,
            return_type,
            current_revision,
            checked.security(),
            checked.transaction(),
            checked.volatility(),
        ))
    }

    fn server_artifact(
        &self,
        checked: &crate::CheckedServerFunction,
        function: &FunctionDefinition,
        object_types: &[ObjectTypeDefinition],
        enum_types: &[EnumTypeDefinition],
        record_value_types: &[RecordValueTypeDefinition],
    ) -> Result<PreparedFunctionArtifact, PrepareError> {
        let query_function = query_planning_function(function);

        if let Some(checked_plan) = checked.unique_text_selected_query_plan() {
            let plan = checked_plan.try_map_identities(
                |id| self.identities.type_id(id),
                |id| self.identities.field(id),
                |id| self.identities.function(id),
                |id| self.identities.parameter(id),
            )?;
            let references = self.mapped_references(checked)?;
            let hash_context = self.mode.catalogue_hash_context();
            let standard = hash_context
                .standard()
                .map(VerifiedStandardLibrarySnapshot::catalogue);
            let encoded = unique_text_selected_query_plan(
                &plan,
                &query_function,
                object_types,
                standard,
                &references,
            )?;
            let payload = encoded.payload().to_vec();
            let hash = artifact_payload_digest(&payload)?;
            return Ok(PreparedFunctionArtifact {
                artifact: ExecutableArtifact::new(
                    ExecutableArtifactKind::Server,
                    SERVER_PLAN_FORMAT,
                    encoded.format_version(),
                    payload,
                    hash,
                )?,
                language_version: SERVER_PLAN_LANGUAGE_VERSION.to_owned(),
            });
        }

        if let Some(checked_plan) = checked.identity_selected_query_plan() {
            let plan = checked_plan.try_map_identities(
                |id| self.identities.type_id(id),
                |id| self.identities.field(id),
                |id| self.identities.function(id),
                |id| self.identities.parameter(id),
            )?;
            let references = self.mapped_references(checked)?;
            let encoded =
                identity_selected_query_plan(&plan, &query_function, object_types, &references)?;
            let payload = encoded.payload().to_vec();
            let hash = artifact_payload_digest(&payload)?;
            return Ok(PreparedFunctionArtifact {
                artifact: ExecutableArtifact::new(
                    ExecutableArtifactKind::Server,
                    SERVER_PLAN_FORMAT,
                    encoded.format_version(),
                    payload,
                    hash,
                )?,
                language_version: SERVER_PLAN_LANGUAGE_VERSION.to_owned(),
            });
        }

        if let Some(checked_plan) = checked.distinct_query_plan() {
            let plan = checked_plan.try_map_identities(
                |id| self.identities.type_id(id),
                |id| self.identities.field(id),
            )?;
            let references = self.mapped_references(checked)?;
            let encoded = distinct_query_plan(&plan, &query_function, object_types, &references)?;
            let payload = encoded.payload().to_vec();
            let hash = artifact_payload_digest(&payload)?;
            return Ok(PreparedFunctionArtifact {
                artifact: ExecutableArtifact::new(
                    ExecutableArtifactKind::Server,
                    SERVER_PLAN_FORMAT,
                    encoded.format_version(),
                    payload,
                    hash,
                )?,
                language_version: SERVER_PLAN_LANGUAGE_VERSION.to_owned(),
            });
        }

        if let Some(checked_plan) = checked.query_plan() {
            let plan = checked_plan.try_map_identities(
                |id| self.identities.type_id(id),
                |id| self.identities.field(id),
            )?;
            let references = self.mapped_references(checked)?;
            let payload =
                version_one_query_plan(&plan, &query_function, object_types, &references)?;
            let hash = artifact_payload_digest(&payload)?;
            return Ok(PreparedFunctionArtifact {
                artifact: ExecutableArtifact::new(
                    ExecutableArtifactKind::Server,
                    SERVER_PLAN_FORMAT,
                    SERVER_PLAN_VERSION,
                    payload,
                    hash,
                )?,
                language_version: SERVER_PLAN_LANGUAGE_VERSION.to_owned(),
            });
        }

        let references = self.mapped_references(checked)?;

        let (format_version, payload) = if let Some(checked_plan) = checked.mutation_plan() {
            let plan = checked_plan.try_map_identities(
                |id| self.identities.type_id(id),
                |id| self.identities.field(id),
                |id| self.identities.function(id),
                |id| self.identities.parameter(id),
            )?;
            let context = self.mode.catalogue_hash_context();
            let standard = context
                .standard()
                .map(VerifiedStandardLibrarySnapshot::catalogue);
            let plan = server_mutation_plan(
                &plan,
                function,
                object_types,
                enum_types,
                record_value_types,
                standard,
                &references,
            )?;
            (plan.format_version(), plan.encode()?)
        } else {
            let checked_plan = checked
                .delete_plan()
                .ok_or(PrepareError::InvalidCheckedBundle {
                    reason: "checked SERVER function body cannot be prepared",
                })?;
            let plan = checked_plan.try_map_identities(
                |id| self.identities.type_id(id),
                |id| self.identities.function(id),
                |id| self.identities.parameter(id),
            )?;
            let plan = server_delete_plan(&plan, function, object_types, &references)?;
            (plan.format_version(), plan.encode()?)
        };
        let hash = artifact_payload_digest(&payload)?;
        Ok(PreparedFunctionArtifact {
            artifact: ExecutableArtifact::new(
                ExecutableArtifactKind::Server,
                SERVER_MUTATION_PLAN_FORMAT,
                format_version,
                payload,
                hash,
            )?,
            language_version: SERVER_MUTATION_PLAN_LANGUAGE_VERSION.to_owned(),
        })
    }

    fn mapped_references(
        &self,
        checked: &crate::CheckedServerFunction,
    ) -> Result<Vec<(DefinitionReferenceKind, DefinitionReferenceTarget)>, PrepareError> {
        checked
            .references()
            .iter()
            .map(|reference| {
                Ok((
                    reference.kind(),
                    self.identities.reference_target(reference.target())?,
                ))
            })
            .collect()
    }

    fn function_references(
        &self,
        checked: &crate::CheckedServerFunction,
        function: FunctionId,
        revision: FunctionRevisionId,
    ) -> Result<Vec<DefinitionReference>, PrepareError> {
        let mut references = Vec::with_capacity(checked.references().len());
        let mut remaining_references = checked.references().iter().collect::<Vec<_>>();
        if let Some(signature_evidence) = self.mode.signature_evidence() {
            for signature_slot in signature_evidence.function_slots(checked.id()) {
                let ordinal = u32::try_from(references.len()).map_err(|_| {
                    PrepareError::ReferenceCountExceedsU32 {
                        function: checked.id(),
                        count: references.len(),
                    }
                })?;
                if signature_slot.flattened_ordinal != ordinal {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: "checked standard signature has a non-contiguous slot sequence",
                    });
                }
                match signature_slot.target {
                    EvidenceTarget::Value(target) => {
                        references.push(DefinitionReference::new(
                            function,
                            revision,
                            ordinal,
                            DefinitionReferenceTarget::ValueType(target),
                            DefinitionReferenceKind::NamedType,
                            self.source.origin(&signature_slot.location)?,
                        ));
                    }
                    EvidenceTarget::Named(target) => {
                        references.push(DefinitionReference::new(
                            function,
                            revision,
                            ordinal,
                            DefinitionReferenceTarget::ValueType(self.identities.type_id(target)?),
                            DefinitionReferenceKind::NamedType,
                            self.source.origin(&signature_slot.location)?,
                        ));
                    }
                    EvidenceTarget::ObjectReference(target) => {
                        let target = CheckedDefinitionReferenceTarget::ObjectType(target);
                        let Some(index) = remaining_references.iter().position(|reference| {
                            reference.target() == target
                                && reference.kind() == DefinitionReferenceKind::ObjectReference
                                && reference.location() == &signature_slot.location
                        }) else {
                            return Err(PrepareError::InvalidCheckedBundle {
                                reason: "checked standard object signature has no exact definition reference",
                            });
                        };
                        let reference = remaining_references.remove(index);
                        references.push(DefinitionReference::new(
                            function,
                            revision,
                            ordinal,
                            self.identities.reference_target(reference.target())?,
                            reference.kind(),
                            self.source.origin(reference.location())?,
                        ));
                    }
                    EvidenceTarget::Unknown => {
                        return Err(PrepareError::InvalidCheckedBundle {
                            reason: "checked standard signature has an unknown declaration use",
                        });
                    }
                }
            }
        }
        let mut next_ordinal = u32::try_from(references.len()).map_err(|_| {
            PrepareError::ReferenceCountExceedsU32 {
                function: checked.id(),
                count: references.len(),
            }
        })?;
        for reference in remaining_references {
            let ordinal = next_ordinal;
            next_ordinal =
                next_ordinal
                    .checked_add(1)
                    .ok_or(PrepareError::ReferenceCountExceedsU32 {
                        function: checked.id(),
                        count: usize::MAX,
                    })?;
            references.push(DefinitionReference::new(
                function,
                revision,
                ordinal,
                self.identities.reference_target(reference.target())?,
                reference.kind(),
                self.source.origin(reference.location())?,
            ));
        }
        Ok(references)
    }

    fn push_function_origins(
        &mut self,
        checked: &crate::CheckedServerFunction,
        function: FunctionId,
    ) -> Result<(), PrepareError> {
        self.push_origin(DefinitionIdentity::Function(function), checked.location())?;
        for parameter in checked.parameters() {
            self.push_origin(
                DefinitionIdentity::Parameter {
                    owner: function,
                    parameter: self.identities.parameter(parameter.id())?,
                },
                parameter.location(),
            )?;
        }
        for column in checked.return_columns() {
            self.push_origin(
                DefinitionIdentity::FunctionReturnColumn {
                    owner: function,
                    ordinal: column.ordinal(),
                },
                column.location(),
            )?;
        }
        Ok(())
    }

    fn push_client_function_origins(
        &mut self,
        checked: &ValidatedClient,
        function: FunctionId,
    ) -> Result<(), PrepareError> {
        self.push_origin(DefinitionIdentity::Function(function), &checked.location)?;
        for parameter in &checked.parameters {
            self.push_origin(
                DefinitionIdentity::Parameter {
                    owner: function,
                    parameter: self.identities.parameter(parameter.id())?,
                },
                parameter.location(),
            )?;
        }
        Ok(())
    }

    fn push_origin(
        &mut self,
        identity: DefinitionIdentity,
        location: &SourceLocation,
    ) -> Result<(), PrepareError> {
        self.origins.push(DefinitionOrigin::new(
            identity,
            self.source.origin(location)?,
        ));
        Ok(())
    }
}

fn supports_unique_text_value_type(value_type: &ValueTypeDefinition) -> bool {
    value_type.kind() == ValueTypeKind::Primitive
        && value_type.mutability() == ValueTypeMutability::Immutable
        && value_type.persistence() == ValueTypePersistence::Persistable
        && value_type.representation_contract() == "orna.kernel.value.character-large-object@1"
}

fn supports_durable_unique_field(
    resolved_type: ResolvedType,
    nullable: bool,
    standard: Option<&CatalogueSnapshot>,
) -> bool {
    if !nullable && resolved_type.reference_target().is_some() {
        return true;
    }
    match (standard, resolved_type) {
        (None, ResolvedType::Scalar(StandardScalar::CharacterLargeObject)) => true,
        (Some(standard), ResolvedType::Value(type_id)) => standard
            .value_type_by_id(type_id)
            .is_some_and(supports_unique_text_value_type),
        _ => false,
    }
}

fn standard_upgrade_reuse_is_current_only(
    semantic_hash_version: FunctionSemanticHashVersion,
) -> bool {
    semantic_hash_version == FunctionSemanticHashVersion::Version1
}

fn next_function_revision_number(
    active: &ActiveDatabaseRevision,
    function: FunctionId,
) -> Result<u64, PrepareError> {
    active
        .function_revisions()
        .iter()
        .chain(active.historical_function_revisions())
        .filter(|revision| revision.function() == function)
        .map(FunctionRevisionRecord::revision_number)
        .max()
        .map_or(Ok(1), |maximum| {
            maximum
                .checked_add(1)
                .ok_or(PrepareError::FunctionRevisionNumberExhausted { function })
        })
}

#[cfg(test)]
mod tests;
