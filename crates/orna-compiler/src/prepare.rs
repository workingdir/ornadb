//! Construction of complete durable revisions from successful compiler checks.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    error::Error,
    fmt,
};

use orna_artifact::{
    client_plan::{
        ClientPlan, FORMAT_IDENTITY as CLIENT_PLAN_FORMAT, FORMAT_VERSION as CLIENT_PLAN_VERSION,
        LANGUAGE_VERSION_IDENTITY as CLIENT_PLAN_LANGUAGE_VERSION,
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
        MutationExpressionKind as ServerMutationExpressionKind, MutationSelector, ServerDeletePlan,
        ServerMutationPlan, ServerMutationPlanError,
    },
    server_plan::{
        FORMAT_IDENTITY as SERVER_PLAN_FORMAT, FORMAT_VERSION as SERVER_PLAN_VERSION,
        LANGUAGE_VERSION_IDENTITY as SERVER_PLAN_LANGUAGE_VERSION, ServerPlanError,
    },
};
use orna_core::{
    CatalogueRevisionId, ExpressionId, FieldId, FunctionId, FunctionRevisionId, ParameterId,
    SchemaId, SourceBundleId, SourceRevisionId, SourceUnitId, StandardLibraryRevisionId,
    TypeBindingId, TypeId,
    canonical_hash::{
        CanonicalHashError, artifact_payload_digest, catalogue_digest,
        catalogue_digest_with_context, function_declaration_digest, function_semantic_digest,
        function_semantic_digest_with_version, source_bundle_digest, source_revision_record_digest,
        source_unit_content_digest,
    },
    catalogue::{
        CatalogueSnapshot, CatalogueSnapshotError, EnumTypeDefinition, FieldDefinition,
        FunctionDefinition, FunctionDomain, FunctionReturn, FunctionReturnColumnDefinition,
        FunctionSecurity, FunctionTransaction, FunctionVolatility, ObjectTypeDefinition,
        ParameterDefinition, QualifiedSemanticName, RecordValueFieldDefinition,
        RecordValueTypeDefinition, SchemaDefinition,
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
    types::{ResolvedType, StandardScalar},
};

use crate::{
    CheckReport, CheckedBundle, CheckedDefinitionReferenceTarget, CheckedExpressionId,
    CheckedFieldId, CheckedFunctionId, CheckedParameterId, CheckedSchemaId, CheckedTypeId,
    CompilerDiagnostic, ConstantValue, ParseReport, SemanticType, SourceLocation,
    StandardApplicationCheckContext, StandardApplicationCheckReport,
    StandardApplicationContextError, check_standard_application,
};
use crate::{
    mutation::{DeletePlanIr, MutationExpressionKind, MutationOperation, MutationPlanIr},
    relational::{supports_server_select_distinct, supports_server_select_equality},
    resolver::{
        CheckedFieldRename, REQUIRED_UNIQUE_REFERENCE_MESSAGE, supports_required_unique_reference,
    },
};

/// One encoded SERVER artifact with the language version that defines it.
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
    let mut allocations = CandidateAllocator::legacy();
    let identities = IdentityMap::build_legacy(checked, active, &mut allocations)?;
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
        PreparationMode::LegacyV1,
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
pub(crate) fn prepare_checked_standard_upgrade_with_allocator(
    standard: &crate::CheckedStandardLibrary,
    active: &ActiveDatabaseRevision,
    mut allocations: CandidateAllocator,
) -> Result<PreparedStandardUpgrade, PrepareStandardUpgradeError> {
    if let Some(installed) = active.catalogue_hash_context().standard() {
        return Err(
            PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled {
                revision: installed.revision(),
            },
        );
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
        crate::CheckedTypeUseKind::Return { .. } => "return",
        crate::CheckedTypeUseKind::Expression { .. } => "expression",
        crate::CheckedTypeUseKind::Result { .. } => "result",
    }
}

enum PreparationMode<'a> {
    LegacyV1,
    StandardV1Match {
        declaration_evidence: DeclarationEvidence,
        standard_preflight: Box<StandardPreflight>,
    },
    StandardV2Plan {
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
    Named(TypeId),
    Reference(TypeId),
}

impl CandidateResolvedType {
    fn from_compatibility(compatibility: ResolvedType) -> Result<Self, PrepareError> {
        if let Some(scalar) = compatibility.legacy_scalar() {
            return Ok(Self::LegacyScalar(scalar));
        }
        if let Some(type_id) = compatibility.named_type() {
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
            Self::Named(type_id) => ResolvedType::Named(type_id),
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
    if let CandidateResolvedType::Named(target) = candidate
        && let MappedEvidenceTarget::Named(actual) = evidence
        && target == actual
    {
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
                candidate => candidate.compatibility_type(),
            },
        }
    }

    fn lower_durable_standard_value(
        self,
        type_id: TypeId,
        compatibility: StandardScalar,
    ) -> ResolvedType {
        match self {
            Self::LegacyV1 | Self::StandardV1Match => ResolvedType::Scalar(compatibility),
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
            Self::LegacyV1 => CandidateLoweringMode::LegacyV1,
            Self::StandardV1Match { .. } => CandidateLoweringMode::StandardV1Match,
            Self::StandardV2Plan { .. } => CandidateLoweringMode::StandardV2Plan,
            Self::StandardV2 { .. } => CandidateLoweringMode::StandardV2,
        }
    }

    fn catalogue_hash_context(&self) -> CatalogueHashContext {
        match self {
            Self::LegacyV1 | Self::StandardV1Match { .. } => CatalogueHashContext::version_one(),
            Self::StandardV2Plan { .. } => CatalogueHashContext::version_one(),
            Self::StandardV2 { standard, .. } => {
                CatalogueHashContext::version_two(standard.verified_snapshot().clone())
            }
        }
    }

    fn standard_preflight(&self) -> Option<&StandardPreflight> {
        match self {
            Self::LegacyV1 => None,
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
            Self::LegacyV1 | Self::StandardV1Match { .. } => None,
        }
    }

    fn semantic_hash_version(
        &self,
        references: &[DefinitionReference],
    ) -> FunctionSemanticHashVersion {
        match self {
            Self::LegacyV1 | Self::StandardV1Match { .. } => FunctionSemanticHashVersion::Version1,
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
    return_type: TypeId,
    location: SourceLocation,
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
struct ValidatedClient {
    id: CheckedFunctionId,
    name: orna_core::catalogue::QualifiedSemanticName,
    location: SourceLocation,
    security: FunctionSecurity,
    transaction: Option<FunctionTransaction>,
    volatility: FunctionVolatility,
    return_type: TypeId,
    return_location: SourceLocation,
    return_scalar: StandardScalar,
    body_value: bool,
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
            let [slot] = slots.as_slice() else {
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
            let EvidenceTarget::Value(return_type) = &slot.target else {
                return Err(
                    PrepareStandardApplicationError::FunctionTypeReferenceMismatch {
                        function: owner,
                    },
                );
            };
            if slot_owner != owner || ordinal != 0 || slot.flattened_ordinal != 0 {
                return Err(
                    PrepareStandardApplicationError::FunctionTypeReferenceMismatch {
                        function: owner,
                    },
                );
            }
            ordered.push(ValidatedClientReturn {
                owner,
                return_type: *return_type,
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
    references: &[(DefinitionReferenceKind, DefinitionReferenceTarget)],
) -> Result<ServerMutationPlan, PrepareError> {
    if plan.assignments().iter().any(|assignment| {
        matches!(
            assignment.expression().kind(),
            MutationExpressionKind::RecordConstructor { .. }
        )
    }) {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "record constructor mutation artifact encoding is not implemented",
        });
    }
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

    validate_mutation_parameters(function, object_types)?;
    let assignments = validate_mutation_assignments(
        plan.assignments(),
        target,
        function,
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
        if resolved_type.named_type().is_some() {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "mutation parameter has an unsupported runtime type",
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

fn validate_mutation_assignments(
    assignments: &[crate::mutation::MutationAssignment<TypeId, FieldId, FunctionId, ParameterId>],
    target: &ObjectTypeDefinition,
    function: &FunctionDefinition,
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
        let expression = server_mutation_expression(assignment.expression(), function, field)?;
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
    expression: &crate::mutation::MutationExpression<TypeId, FunctionId, ParameterId>,
    function: &FunctionDefinition,
    field: &FieldDefinition,
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
        MutationExpressionKind::RecordConstructor { .. } => {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "record constructor mutation artifact encoding is not implemented",
            });
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
        _ => {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "mutation artifact has an unsupported expression kind",
            });
        }
    }
    Ok(artifact_expression)
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
        if let MutationExpressionKind::ParameterRead { owner, parameter } =
            assignment.expression().kind()
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
    if let FunctionReturn::Rows(columns) = function.return_type() {
        for column in columns {
            if let ResolvedType::Reference { target } = column.resolved_type() {
                references.push((
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(target),
                ));
            }
        }
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
    if !function.parameters().is_empty() {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "checked CLIENT function declares parameters",
        });
    }
    let standard_boolean = standard.value_types().iter().any(|value_type| {
        value_type.id() == return_evidence.return_type
            && value_type.representation_contract() == "orna.kernel.value.boolean@1"
    });
    if function.return_type() != SemanticType::Scalar(StandardScalar::Boolean) || !standard_boolean
    {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "checked CLIENT function does not return BOOLEAN from the checked standard library",
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
    let (body_value, _) = function
        .boolean_body()
        .ok_or(PrepareError::InvalidCheckedBundle {
            reason: "checked CLIENT function has an unsupported body",
        })?;
    if !function.references().is_empty() {
        return Err(PrepareError::InvalidCheckedBundle {
            reason: "checked CLIENT function contains unsupported application definition references",
        });
    }
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
        return_type: return_evidence.return_type,
        return_location: return_evidence.location.clone(),
        return_scalar: StandardScalar::Boolean,
        body_value,
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
            && !supports_required_unique_reference(field.semantic_type(), field.nullable())
        {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: REQUIRED_UNIQUE_REFERENCE_MESSAGE,
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
        if let Some((_, location)) = function.boolean_body() {
            locations.push(location);
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
                        }
                    }
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
        Ok(catalogue_digest(
            &self.catalogue,
            &self.current_function_revisions,
            &self.expressions,
            &self.origins,
            &self.references,
        )? == active.catalogue_hash())
    }

    fn into_deployable(
        self,
        active: &ActiveDatabaseRevision,
        context: CatalogueHashContext,
    ) -> Result<DeployableRevision, PrepareError> {
        let catalogue_hash = self.catalogue_hash(&context)?;
        self.into_deployable_with_catalogue_hash(active, context, catalogue_hash)
    }

    fn catalogue_hash(&self, context: &CatalogueHashContext) -> Result<Sha256Digest, PrepareError> {
        if context.standard().is_none() {
            return Ok(catalogue_digest(
                &self.catalogue,
                &self.current_function_revisions,
                &self.expressions,
                &self.origins,
                &self.references,
            )?);
        }
        Ok(catalogue_digest_with_context(
            context,
            &self.catalogue,
            &self.current_function_revisions,
            &self.expressions,
            &self.origins,
            &self.references,
        )?)
    }

    fn into_deployable_with_catalogue_hash(
        self,
        active: &ActiveDatabaseRevision,
        context: CatalogueHashContext,
        catalogue_hash: Sha256Digest,
    ) -> Result<DeployableRevision, PrepareError> {
        if context.standard().is_none() {
            return Ok(DeployableRevision::new(
                active.pair(),
                self.source,
                active.pair().catalogue(),
                self.catalogue,
                catalogue_hash,
                self.origins,
                self.expressions,
                self.new_function_revisions,
                self.references,
            )?);
        }
        Ok(DeployableRevision::new_with_catalogue_hash_context(
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
        )?)
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
            PreparationMode::LegacyV1 => None,
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
        self.build_functions(&object_types.compatibility)?;
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
                fields.push(RecordValueFieldDefinition::new(
                    field_id,
                    checked_field.name(),
                    checked_field.ordinal(),
                    resolved_type,
                ));
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
                if candidate_field.resolved_type() != active_field.resolved_type() {
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
                self.build_server_function(checked, object_types)?;
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
                    self.build_server_function(&checked, object_types)?;
                }
                FunctionDomain::Client => {
                    let validated = self.validated_client(owner)?.clone();
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
                    self.push_origin(DefinitionIdentity::Function(function), &client.location)?;
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
        let prepared_artifact =
            self.server_artifact(checked, &compatibility_definition, object_types)?;
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
        let function_id = self.identities.function(validated.id)?;
        let initial_revision = self.initial_function_revision(validated.id, function_id)?;
        let initial_definition = self.client_function_definition(
            validated,
            initial_revision,
            false,
            CandidateTypeProjection::Durable,
        )?;
        let prepared_artifact = self.client_artifact(validated)?;
        let initial_references = if self.mode.signature_evidence().is_some() {
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
        self.push_origin(
            DefinitionIdentity::Function(function_id),
            &validated.location,
        )?;
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
        Ok(FunctionDefinition::new(
            self.identities.function(validated.id)?,
            validated.name.clone(),
            FunctionDomain::Client,
            Vec::new(),
            FunctionReturn::Single(self.client_return_type(
                validated,
                consume_evidence,
                projection,
            )?),
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
        let payload = ClientPlan::return_boolean(validated.body_value).encode();
        let hash = artifact_payload_digest(&payload)?;
        Ok(PreparedFunctionArtifact {
            artifact: ExecutableArtifact::new(
                ExecutableArtifactKind::Client,
                CLIENT_PLAN_FORMAT,
                CLIENT_PLAN_VERSION,
                payload,
                hash,
            )?,
            language_version: CLIENT_PLAN_LANGUAGE_VERSION.to_owned(),
        })
    }

    fn client_function_references(
        &self,
        function: FunctionId,
        revision: FunctionRevisionId,
        validated: &ValidatedClient,
    ) -> Result<Vec<DefinitionReference>, PrepareError> {
        Ok(vec![DefinitionReference::new(
            function,
            revision,
            0,
            DefinitionReferenceTarget::ValueType(validated.return_type),
            DefinitionReferenceKind::NamedType,
            self.source.origin(&validated.return_location)?,
        )])
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

    fn validated_client(&self, owner: CheckedFunctionId) -> Result<&ValidatedClient, PrepareError> {
        let Some(standard_preflight) = self.mode.standard_preflight() else {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "checked CLIENT function requires standard preparation evidence",
            });
        };
        standard_preflight
            .clients
            .get(&owner)
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
        let Some(declaration_evidence) = &self.declaration_evidence else {
            return Ok(CandidateResolvedType::LegacyScalar(validated.return_scalar));
        };
        let kind = crate::CheckedTypeUseKind::Return {
            owner: validated.id,
            ordinal: 0,
        };
        let evidence = if consume_evidence {
            declaration_evidence.borrow_mut().consume(kind)?
        } else {
            declaration_evidence.borrow().lookup(kind)?
        };
        if evidence.target != EvidenceTarget::Value(validated.return_type)
            || evidence.location != validated.return_location
        {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "checked CLIENT function return evidence does not match its validated slot",
            });
        }
        Ok(CandidateResolvedType::StandardValue {
            type_id: validated.return_type,
            compatibility: validated.return_scalar,
        })
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
        let return_columns = checked
            .return_columns()
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

        Ok(FunctionDefinition::new(
            function_id,
            checked.name().clone(),
            FunctionDomain::Server,
            parameters,
            FunctionReturn::Rows(return_columns),
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
    ) -> Result<PreparedFunctionArtifact, PrepareError> {
        if let Some(checked_plan) = checked.identity_selected_query_plan() {
            let plan = checked_plan.try_map_identities(
                |id| self.identities.type_id(id),
                |id| self.identities.field(id),
                |id| self.identities.function(id),
                |id| self.identities.parameter(id),
            )?;
            let references = self.mapped_references(checked)?;
            let encoded = identity_selected_query_plan(&plan, function, object_types, &references)?;
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
            let encoded = distinct_query_plan(&plan, function, object_types, &references)?;
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
            let payload = version_one_query_plan(&plan, function, object_types, &references)?;
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
            let plan = server_mutation_plan(&plan, function, object_types, &references)?;
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
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use orna_artifact::{
        constant_expression::ConstantExpression,
        server_mutation_plan::{
            MutationExpressionKind as DurableMutationExpressionKind, ServerDeletePlan,
            ServerMutationOperation, ServerMutationPlan,
        },
        server_plan::{
            DistinctServerPlan, ExpressionKind, IdentitySelectedServerPlan, ServerPlan,
            ServerPlanError,
        },
    };
    use orna_core::{
        catalogue::{
            CatalogueSnapshot, FieldDefinition, FunctionDefinition, FunctionDomain, FunctionReturn,
            FunctionSecurity, FunctionTransaction, FunctionVolatility, ObjectTypeDefinition,
            ParameterDefinition,
        },
        revision::{
            ActiveDatabaseRevision, DefinitionReferenceKind, DefinitionReferenceTarget,
            ExecutableArtifactKind, FunctionRevisionRecord, Sha256Digest, SourceOrigin,
            StoredSourceRevision,
        },
        source::{SourceBundle, SourceUnit},
        types::{ResolvedType, StandardScalar},
    };

    use super::*;
    use crate::{
        check,
        mutation::{
            MutationAssignment, MutationExpression, MutationExpressionKind, MutationValueType,
        },
    };

    #[test]
    fn member_multiset_comparison_ignores_order_but_preserves_exact_multiplicity() {
        assert!(same_member_multiset(&[1_u8, 2, 2, 3], &[3, 2, 1, 2]));
        assert!(!same_member_multiset(&[1_u8, 2, 2, 3], &[3, 2, 1, 4]));
        assert!(!same_member_multiset(&[1_u8, 2, 2, 3], &[3, 2, 1, 1]));
    }

    #[test]
    fn checked_value_type_reference_maps_to_its_durable_identity() {
        let checked = CheckedTypeId::Existing(TypeId::from_bytes([0x70; 16]));
        let durable = TypeId::from_bytes([0x71; 16]);
        let mut identities = IdentityMap::default();
        identities.types.insert(checked, durable);

        assert_eq!(
            identities
                .reference_target(CheckedDefinitionReferenceTarget::ValueType(checked))
                .unwrap(),
            DefinitionReferenceTarget::ValueType(durable)
        );
        assert!(matches!(
            identities.reference_target(CheckedDefinitionReferenceTarget::ValueType(
                CheckedTypeId::Existing(TypeId::from_bytes([0x72; 16]))
            )),
            Err(PrepareError::InvalidCheckedBundle {
                reason: "checked type has no durable identity"
            })
        ));
    }

    #[test]
    fn legacy_preparation_reaches_the_explicit_enum_hash_version_gate() {
        let active = empty_active();
        let report = checked_report(
            "CREATE SCHEMA crm; CREATE TYPE crm.stage AS ENUM ('lead', 'customer');",
            active.catalogue(),
        );
        assert!(report.diagnostics().is_empty());
        let checked = report.checked_bundle().unwrap();
        let mut allocations = CandidateAllocator::legacy();
        let identities = IdentityMap::build_legacy(checked, &active, &mut allocations).unwrap();
        let source = PreparedSource::new(
            report.parse_report(),
            active.pair().source(),
            &mut allocations,
        )
        .unwrap();
        let material = CandidateBuilder::new(
            report.parse_report(),
            checked,
            &active,
            identities,
            source,
            PreparationMode::LegacyV1,
            allocations.catalogue_revision(),
        )
        .materialise()
        .unwrap();
        let enum_type = &material.catalogue.enum_types()[0];
        assert_eq!(enum_type.name(), &semantic_name(&["crm", "stage"]));
        assert_eq!(enum_type.labels(), &["lead", "customer"]);
        assert!(
            material.origins.iter().any(|origin| {
                origin.identity() == DefinitionIdentity::ValueType(enum_type.id())
            })
        );

        assert!(matches!(
            prepare(&report, active.pair(), &active),
            Err(PrepareError::CanonicalHash(
                CanonicalHashError::CatalogueFactUnsupportedByHashVersion {
                    fact: orna_core::canonical_hash::CatalogueHashFact::EnumTypeDefinition(_),
                    ..
                }
            ))
        ));
    }

    const SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT (name TEXT NOT NULL);\n\
        CREATE TYPE tasks.task AS OBJECT (\n\
            title TEXT DEFAULT 'todo',\n\
            completed BOOL NOT NULL DEFAULT FALSE,\n\
            priority INT DEFAULT 7,\n\
            note TEXT DEFAULT NULL,\n\
            assignee REF tasks.person ON DELETE SET NULL\n\
        );\n\
        CREATE SERVER FUNCTION tasks.open_tasks()\n\
        RETURNS ROWS (task REF tasks.task, title TEXT)\n\
        TRANSACTION READ ONLY VOLATILITY STABLE\n\
        AS SELECT REF(t), t.title FROM tasks.task t\n\
        WHERE t.completed = FALSE ORDER BY t.title;\n";

    const REFORMATTED_SOURCE: &str = "-- source-only édit\n\
        CREATE SCHEMA tasks;\n\n\
        CREATE TYPE tasks.person AS OBJECT ( name TEXT NOT NULL );\n\
        CREATE TYPE tasks.task AS OBJECT (\n\
          title TEXT DEFAULT 'todo', completed BOOL NOT NULL DEFAULT FALSE,\n\
          priority INT DEFAULT 7, note TEXT DEFAULT NULL,\n\
          assignee REF tasks.person ON DELETE SET NULL\n\
        );\n\n\
        CREATE SERVER FUNCTION tasks.open_tasks()\n\
          RETURNS ROWS (task REF tasks.task, title TEXT)\n\
          TRANSACTION READ ONLY VOLATILITY STABLE\n\
          AS SELECT REF(t), t.title FROM tasks.task t\n\
          WHERE t.completed = FALSE ORDER BY t.title;\n";

    const REQUIRED_UNIQUE_REFERENCE_SOURCE: &str = "CREATE SCHEMA relations;\n\
        CREATE TYPE relations.assignment AS OBJECT (\n\
            owner REF relations.owner NOT NULL UNIQUE\n\
        );\n\
        CREATE TYPE relations.owner AS OBJECT (name TEXT NOT NULL);\n";

    static CATALOGUE_ALLOCATION: AtomicUsize = AtomicUsize::new(0);
    static BUNDLE_ALLOCATION: AtomicUsize = AtomicUsize::new(0);
    static REVISION_ALLOCATION: AtomicUsize = AtomicUsize::new(0);
    static UNIT_ALLOCATION: AtomicUsize = AtomicUsize::new(0);
    static SCHEMA_ALLOCATION: AtomicUsize = AtomicUsize::new(0);
    static TYPE_ALLOCATION: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn mapped_candidate_type_selection_is_closed_and_retains_standard_identity() {
        let standard_id = TypeId::from_bytes([0x91; 16]);
        let reference_id = TypeId::from_bytes([0x92; 16]);
        let named_id = TypeId::from_bytes([0x93; 16]);

        assert_eq!(
            CandidateResolvedType::from_compatibility(ResolvedType::scalar(
                StandardScalar::Integer,
            ))
            .unwrap(),
            CandidateResolvedType::LegacyScalar(StandardScalar::Integer)
        );
        assert_eq!(
            candidate_from_mapped_evidence(
                ResolvedType::scalar(StandardScalar::Boolean),
                Some(MappedEvidenceTarget::Value(standard_id)),
            )
            .unwrap(),
            CandidateResolvedType::StandardValue {
                type_id: standard_id,
                compatibility: StandardScalar::Boolean,
            }
        );
        assert_eq!(
            candidate_from_mapped_evidence(
                ResolvedType::named(named_id),
                Some(MappedEvidenceTarget::Named(named_id)),
            )
            .unwrap(),
            CandidateResolvedType::Named(named_id)
        );
        assert_eq!(
            candidate_from_mapped_evidence(
                ResolvedType::reference(reference_id),
                Some(MappedEvidenceTarget::ObjectReference(reference_id)),
            )
            .unwrap(),
            CandidateResolvedType::Reference(reference_id)
        );

        for (compatibility, expected) in [
            (
                ResolvedType::scalar(StandardScalar::Integer),
                CandidateResolvedType::LegacyScalar(StandardScalar::Integer),
            ),
            (
                ResolvedType::Named(TypeId::from_bytes([0x94; 16])),
                CandidateResolvedType::Named(TypeId::from_bytes([0x94; 16])),
            ),
            (
                ResolvedType::reference(TypeId::from_bytes([0x95; 16])),
                CandidateResolvedType::Reference(TypeId::from_bytes([0x95; 16])),
            ),
        ] {
            assert_eq!(
                candidate_from_mapped_evidence(compatibility, None).unwrap(),
                expected
            );
        }

        for (compatibility, evidence) in [
            (
                ResolvedType::Named(TypeId::from_bytes([0x96; 16])),
                MappedEvidenceTarget::Named(named_id),
            ),
            (
                ResolvedType::scalar(StandardScalar::Boolean),
                MappedEvidenceTarget::Named(named_id),
            ),
            (
                ResolvedType::Named(TypeId::from_bytes([0x96; 16])),
                MappedEvidenceTarget::Value(standard_id),
            ),
            (
                ResolvedType::Named(TypeId::from_bytes([0x96; 16])),
                MappedEvidenceTarget::ObjectReference(reference_id),
            ),
            (
                ResolvedType::reference(reference_id),
                MappedEvidenceTarget::Value(standard_id),
            ),
            (
                ResolvedType::scalar(StandardScalar::Boolean),
                MappedEvidenceTarget::ObjectReference(reference_id),
            ),
            (
                ResolvedType::reference(reference_id),
                MappedEvidenceTarget::ObjectReference(TypeId::from_bytes([0x97; 16])),
            ),
            (
                ResolvedType::scalar(StandardScalar::Boolean),
                MappedEvidenceTarget::Unknown,
            ),
            (
                ResolvedType::Named(TypeId::from_bytes([0x96; 16])),
                MappedEvidenceTarget::Unknown,
            ),
            (
                ResolvedType::reference(reference_id),
                MappedEvidenceTarget::Unknown,
            ),
        ] {
            let error = candidate_from_mapped_evidence(compatibility, Some(evidence)).unwrap_err();
            assert!(matches!(
                error,
                PrepareError::InvalidCheckedBundle {
                    reason: "checked standard declaration type evidence disagrees with its semantic type",
                }
            ));
        }
    }

    #[test]
    fn candidate_projection_policy_emits_value_identities_only_for_durable_v2_modes() {
        let type_id = TypeId::from_bytes([0x98; 16]);
        let candidate = CandidateResolvedType::StandardValue {
            type_id,
            compatibility: StandardScalar::Boolean,
        };
        let compatibility = ResolvedType::scalar(StandardScalar::Boolean);

        assert_ne!(
            CandidateTypeProjection::Compatibility,
            CandidateTypeProjection::Durable
        );
        for (mode, durable) in [
            (
                CandidateLoweringMode::LegacyV1,
                ResolvedType::scalar(StandardScalar::Boolean),
            ),
            (
                CandidateLoweringMode::StandardV1Match,
                ResolvedType::scalar(StandardScalar::Boolean),
            ),
            (
                CandidateLoweringMode::StandardV2Plan,
                ResolvedType::Value(type_id),
            ),
            (
                CandidateLoweringMode::StandardV2,
                ResolvedType::Value(type_id),
            ),
        ] {
            assert_eq!(
                mode.lower(candidate, CandidateTypeProjection::Compatibility),
                compatibility
            );
            assert_eq!(
                mode.lower(candidate, CandidateTypeProjection::Durable),
                durable
            );
        }
    }

    #[test]
    fn declaration_evidence_lookup_preserves_the_slot_for_final_consumption() {
        let kind = crate::CheckedTypeUseKind::Return {
            owner: CheckedFunctionId::Existing(FunctionId::from_bytes([0x92; 16])),
            ordinal: 0,
        };
        let evidence = EvidenceUse {
            kind,
            target: EvidenceTarget::Value(TypeId::from_bytes([0x93; 16])),
            location: SourceLocation::from_syntax(
                "prepared.orna",
                &orna_syntax::SourceSpan { start: 4, end: 9 },
            ),
        };
        let mut declarations = DeclarationEvidence {
            ordered: vec![evidence.clone()],
            remaining: vec![evidence.clone()],
            consumed: Vec::new(),
        };

        assert_eq!(declarations.lookup(kind).unwrap(), evidence);
        assert_eq!(declarations.remaining.len(), 1);
        assert_eq!(declarations.consume(kind).unwrap(), evidence);
        assert!(declarations.is_empty());
        assert_eq!(declarations.consumed, vec![evidence]);
    }

    fn allocation_byte(counter: &AtomicUsize) -> u8 {
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            1
        } else {
            2
        }
    }

    fn next_catalogue_id() -> CatalogueRevisionId {
        CatalogueRevisionId::from_bytes([allocation_byte(&CATALOGUE_ALLOCATION); 16])
    }

    fn next_bundle_id() -> SourceBundleId {
        SourceBundleId::from_bytes([allocation_byte(&BUNDLE_ALLOCATION); 16])
    }

    fn next_revision_id() -> SourceRevisionId {
        SourceRevisionId::from_bytes([allocation_byte(&REVISION_ALLOCATION); 16])
    }

    fn next_unit_id() -> SourceUnitId {
        SourceUnitId::from_bytes([allocation_byte(&UNIT_ALLOCATION); 16])
    }

    fn next_schema_id() -> SchemaId {
        SchemaId::from_bytes([allocation_byte(&SCHEMA_ALLOCATION); 16])
    }

    fn next_type_id() -> TypeId {
        TypeId::from_bytes([allocation_byte(&TYPE_ALLOCATION); 16])
    }

    fn next_function_revision_id() -> FunctionRevisionId {
        FunctionRevisionId::new()
    }

    fn allocated_standard_upgrade_plan_for_construction_test() -> AllocatedStandardUpgradePlan {
        let unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes([0x74; 16]),
            0,
            "application.orna",
            "",
            source_unit_content_digest("").unwrap(),
        )
        .unwrap();
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
        AllocatedStandardUpgradePlan {
            source_template: StoredSourceRevision::new(
                SourceBundleId::from_bytes([0x75; 16]),
                SourceRevisionId::from_bytes([0x76; 16]),
                None,
                vec![unit],
                bundle_hash,
                source_revision_record_digest(
                    SourceBundleId::from_bytes([0x75; 16]),
                    None,
                    bundle_hash,
                )
                .unwrap(),
            )
            .unwrap(),
            source_ids: PreparedSourceIds {
                bundle: SourceBundleId::from_bytes([0x77; 16]),
                revision: SourceRevisionId::from_bytes([0x78; 16]),
                units: vec![SourceUnitId::from_bytes([0x79; 16])],
            },
            catalogue_revision: CatalogueRevisionId::from_bytes([0x7a; 16]),
            schemas: Vec::new(),
            object_types: Vec::new(),
            expressions: Vec::new(),
            origin_templates: Vec::new(),
            functions: Vec::new(),
        }
    }

    #[test]
    fn standard_allocator_retries_each_same_class_reserved_identity() {
        CATALOGUE_ALLOCATION.store(0, Ordering::SeqCst);
        BUNDLE_ALLOCATION.store(0, Ordering::SeqCst);
        REVISION_ALLOCATION.store(0, Ordering::SeqCst);
        UNIT_ALLOCATION.store(0, Ordering::SeqCst);
        SCHEMA_ALLOCATION.store(0, Ordering::SeqCst);
        TYPE_ALLOCATION.store(0, Ordering::SeqCst);
        let mut reserved = ReservedStandardIds::default();
        reserved
            .catalogues
            .insert(CatalogueRevisionId::from_bytes([1; 16]));
        reserved
            .source_bundles
            .insert(SourceBundleId::from_bytes([1; 16]));
        reserved
            .source_revisions
            .insert(SourceRevisionId::from_bytes([1; 16]));
        reserved
            .source_units
            .insert(SourceUnitId::from_bytes([1; 16]));
        reserved.schemas.insert(SchemaId::from_bytes([1; 16]));
        reserved.types.insert(TypeId::from_bytes([1; 16]));
        let source = CandidateIdSource {
            catalogue_revision: next_catalogue_id,
            source_bundle: next_bundle_id,
            source_revision: next_revision_id,
            source_unit: next_unit_id,
            schema: next_schema_id,
            type_id: next_type_id,
            function_revision: next_function_revision_id,
        };
        let mut allocator = CandidateAllocator::with_source(reserved, source);

        assert_eq!(
            allocator.catalogue_revision(),
            CatalogueRevisionId::from_bytes([2; 16])
        );
        assert_eq!(
            allocator.source_bundle(),
            SourceBundleId::from_bytes([2; 16])
        );
        assert_eq!(
            allocator.source_revision(),
            SourceRevisionId::from_bytes([2; 16])
        );
        assert_eq!(allocator.source_unit(), SourceUnitId::from_bytes([2; 16]));
        assert_eq!(allocator.schema(), SchemaId::from_bytes([2; 16]));
        assert_eq!(allocator.type_id(), TypeId::from_bytes([2; 16]));
    }

    #[test]
    fn standard_upgrade_final_construction_errors_preserve_their_exact_sources() {
        let catalogue = PrepareStandardUpgradeError::Catalogue {
            source: orna_core::catalogue::CatalogueSnapshotError::DuplicateSchemaId {
                id: SchemaId::from_bytes([0x71; 16]),
            },
        };
        assert!(matches!(
            &catalogue,
            PrepareStandardUpgradeError::Catalogue {
                source: orna_core::catalogue::CatalogueSnapshotError::DuplicateSchemaId { id },
            } if *id == SchemaId::from_bytes([0x71; 16])
        ));
        assert_eq!(
            catalogue.to_string(),
            format!(
                "the standard upgrade catalogue is invalid: {}",
                std::error::Error::source(&catalogue).unwrap()
            )
        );
        assert!(std::error::Error::source(&catalogue).is_some());

        let candidate_records = PrepareStandardUpgradeError::CandidateRecords {
            source: RevisionInvariantError::EmptyArtifactFormat,
        };
        assert_eq!(
            candidate_records.to_string(),
            format!(
                "the standard upgrade candidate records are invalid: {}",
                std::error::Error::source(&candidate_records).unwrap()
            )
        );
        assert!(std::error::Error::source(&candidate_records).is_some());

        let canonical = PrepareStandardUpgradeError::CanonicalHash {
            source: orna_core::canonical_hash::CanonicalHashError::LengthExceedsU32 {
                value: "upgrade test",
                length: usize::MAX,
            },
        };
        assert!(matches!(
            &canonical,
            PrepareStandardUpgradeError::CanonicalHash {
                source: orna_core::canonical_hash::CanonicalHashError::LengthExceedsU32 {
                    value: "upgrade test",
                    length,
                },
            } if *length == usize::MAX
        ));
        assert_eq!(
            canonical.to_string(),
            format!(
                "the standard upgrade canonical hashes are invalid: {}",
                std::error::Error::source(&canonical).unwrap()
            )
        );
        assert!(std::error::Error::source(&canonical).is_some());

        let revision = PrepareStandardUpgradeError::Revision {
            source: orna_core::revision::RevisionInvariantError::EmptyArtifactFormat,
        };
        assert!(matches!(
            revision,
            PrepareStandardUpgradeError::Revision {
                source: orna_core::revision::RevisionInvariantError::EmptyArtifactFormat,
            }
        ));
        assert_eq!(
            revision.to_string(),
            format!(
                "the standard upgrade revision is invalid: {}",
                std::error::Error::source(&revision).unwrap()
            )
        );
        assert!(std::error::Error::source(&revision).is_some());
    }

    #[test]
    fn standard_upgrade_gate_eight_uses_the_real_catalogue_transition() {
        let mut plan = allocated_standard_upgrade_plan_for_construction_test();
        let duplicate = SchemaId::from_bytes([0x72; 16]);
        plan.schemas = vec![
            SchemaDefinition::new(duplicate, semantic_name(&["first"])),
            SchemaDefinition::new(duplicate, semantic_name(&["second"])),
        ];
        let catalogue_error = plan.into_catalogue().unwrap_err();

        assert!(matches!(
            catalogue_error,
            orna_core::catalogue::CatalogueSnapshotError::DuplicateSchemaId { id }
                if id == duplicate
        ));
    }

    #[test]
    fn standard_upgrade_gate_nine_uses_the_real_candidate_record_transition() {
        let mut plan = allocated_standard_upgrade_plan_for_construction_test();
        plan.source_ids.revision = plan.source_template.id();
        let error = plan
            .into_catalogue()
            .unwrap()
            .into_candidate_records()
            .unwrap_err();

        assert!(matches!(
            error,
            RevisionInvariantError::SourceRevisionSelfParent { revision }
                if revision == SourceRevisionId::from_bytes([0x76; 16])
        ));
    }

    #[test]
    fn standard_upgrade_gate_ten_uses_the_real_canonical_transition() {
        let mut records = allocated_standard_upgrade_plan_for_construction_test()
            .into_catalogue()
            .unwrap()
            .into_candidate_records()
            .unwrap();
        let unit = &records.source.units()[0];
        let invalid = StoredSourceUnit::new(
            unit.id(),
            unit.ordinal(),
            unit.logical_path(),
            unit.content(),
            Sha256Digest::from_bytes([0x7b; 32]),
        )
        .unwrap();
        records.source = StoredSourceRevision::new(
            records.source.bundle(),
            records.source.id(),
            records.source.parent(),
            vec![invalid],
            Sha256Digest::from_bytes([0; 32]),
            Sha256Digest::from_bytes([0; 32]),
        )
        .unwrap();
        let error = records
            .canonicalise(&CatalogueHashContext::version_one())
            .unwrap_err();

        assert!(matches!(
            error,
            CanonicalHashError::SourceContentHashMismatch { source_unit }
                if source_unit == SourceUnitId::from_bytes([0x79; 16])
        ));
    }

    #[test]
    fn standard_upgrade_gate_eleven_follows_eight_nine_and_ten() {
        let candidate = allocated_standard_upgrade_plan_for_construction_test()
            .into_catalogue()
            .unwrap()
            .into_candidate_records()
            .unwrap()
            .canonicalise(&CatalogueHashContext::version_one())
            .unwrap();
        let active = empty_active();
        let error = candidate
            .into_deployable(&active, CatalogueHashContext::version_one())
            .unwrap_err();

        assert!(matches!(
            error,
            RevisionInvariantError::DeployableSourceParentMismatch { expected, actual }
                if expected == active.source().id()
                    && actual == Some(SourceRevisionId::from_bytes([0x76; 16]))
        ));
    }

    #[test]
    fn accepts_all_supported_definition_reference_kinds() {
        let kinds = [
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

        assert_eq!(SUPPORTED_DEFINITION_REFERENCE_KINDS, kinds.as_slice());
        assert!(kinds.into_iter().all(supports_definition_reference_kind));
    }

    #[test]
    fn active_field_rename_states_are_exact_and_fail_closed() {
        let owner = TypeId::from_bytes([9; 16]);
        let field_id = FieldId::from_bytes([10; 16]);
        let other_id = FieldId::from_bytes([11; 16]);
        let rename = CheckedFieldRename {
            owner: CheckedTypeId::Existing(owner),
            field: CheckedFieldId::Existing(field_id),
            old_name: "email".to_owned(),
            new_name: "primary_email".to_owned(),
        };
        let object =
            |fields| ObjectTypeDefinition::new(owner, semantic_name(&["people", "person"]), fields);
        let field = |id, name, ordinal| {
            FieldDefinition::new(
                id,
                name,
                ordinal,
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                false,
                false,
                None,
                None,
            )
        };
        assert!(
            validate_active_field_rename(&object(vec![field(field_id, "email", 0)]), &rename)
                .is_ok()
        );
        assert!(
            validate_active_field_rename(
                &object(vec![field(field_id, "primary_email", 0)]),
                &rename
            )
            .is_ok()
        );
        assert!(matches!(
            validate_active_field_rename(
                &object(vec![
                    field(field_id, "email", 0),
                    field(other_id, "primary_email", 1)
                ]),
                &rename
            ),
            Err(PrepareError::InvalidCheckedBundle {
                reason: "field rename active catalogue contains both names"
            })
        ));
        assert!(matches!(
            validate_active_field_rename(&object(vec![field(other_id, "email", 0)]), &rename),
            Err(PrepareError::InvalidCheckedBundle {
                reason: "field rename names do not resolve to its checked field"
            })
        ));
        assert!(matches!(
            validate_active_field_rename(&object(vec![field(other_id, "other", 0)]), &rename),
            Err(PrepareError::InvalidCheckedBundle {
                reason: "field rename active catalogue contains neither name"
            })
        ));
    }

    const CHANGED_SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT (name TEXT NOT NULL);\n\
        CREATE TYPE tasks.task AS OBJECT (\n\
            title TEXT DEFAULT 'todo',\n\
            completed BOOL NOT NULL DEFAULT FALSE,\n\
            priority INT DEFAULT 7,\n\
            note TEXT DEFAULT NULL,\n\
            assignee REF tasks.person ON DELETE SET NULL\n\
        );\n\
        CREATE SERVER FUNCTION tasks.open_tasks()\n\
        RETURNS ROWS (task REF tasks.task, title TEXT)\n\
        TRANSACTION READ ONLY VOLATILITY STABLE\n\
        AS SELECT REF(t), t.title FROM tasks.task t\n\
        WHERE t.completed = TRUE ORDER BY t.title;\n";

    const DIRECT_BOOLEAN_SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT (active BOOL NOT NULL);\n\
        CREATE TYPE tasks.task AS OBJECT (owner REF tasks.person, completed BOOL NOT NULL);\n\
        CREATE SERVER FUNCTION tasks.active_tasks()\n\
        RETURNS ROWS (active BOOL, completed BOOL)\n\
        AS SELECT t.owner.active, t.completed FROM tasks.task t\n\
        WHERE t.owner.active ORDER BY t.completed DESC;\n";

    const DIRECT_BOOLEAN_REFORMATTED_SOURCE: &str = "-- source-only direct-predicate edit\n\
        CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT ( active BOOL NOT NULL );\n\
        CREATE TYPE tasks.task AS OBJECT ( owner REF tasks.person, completed BOOL NOT NULL );\n\
        CREATE SERVER FUNCTION tasks.active_tasks()\n\
        RETURNS ROWS ( active BOOL, completed BOOL )\n\
        AS SELECT t.owner.active, t.completed\n\
        FROM tasks.task t WHERE t.owner.active ORDER BY t.completed DESC;\n";

    const DIRECT_BOOLEAN_CHANGED_PREDICATE_SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT (active BOOL NOT NULL);\n\
        CREATE TYPE tasks.task AS OBJECT (owner REF tasks.person, completed BOOL NOT NULL);\n\
        CREATE SERVER FUNCTION tasks.active_tasks()\n\
        RETURNS ROWS (active BOOL, completed BOOL)\n\
        AS SELECT t.owner.active, t.completed FROM tasks.task t\n\
        WHERE t.completed ORDER BY t.completed DESC;\n";

    const VERSION_ONE_REFERENCE_SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT (active BOOL NOT NULL);\n\
        CREATE TYPE tasks.task AS OBJECT (owner REF tasks.person);\n\
        CREATE SERVER FUNCTION tasks.owners()\n\
        RETURNS ROWS (owner REF tasks.person)\n\
        AS SELECT t.owner FROM tasks.task t;\n";

    const IDENTITY_SELECTED_SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.task AS OBJECT (title TEXT NOT NULL);\n\
        CREATE SERVER FUNCTION tasks.find(p_task REF tasks.task)\n\
        RETURNS ROWS (task REF tasks.task, title TEXT)\n\
        SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
        AS SELECT REF(t), t.title FROM tasks.task t WHERE REF(t) = p_task;\n";

    const IDENTITY_SELECTED_RENAMED_SELECTOR_SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.task AS OBJECT (title TEXT NOT NULL);\n\
        CREATE SERVER FUNCTION tasks.find(selector REF tasks.task)\n\
        RETURNS ROWS (task REF tasks.task, title TEXT)\n\
        SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
        AS SELECT REF(t), t.title FROM tasks.task t WHERE REF(t) = selector;\n";

    const IDENTITY_SELECTED_NULLABLE_EQUALITY_SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT (name TEXT NOT NULL);\n\
        CREATE TYPE tasks.task AS OBJECT (owner REF tasks.person);\n\
        CREATE SERVER FUNCTION tasks.matches(p_task REF tasks.task)\n\
        RETURNS ROWS (name TEXT, same BOOL)\n\
        SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
        AS SELECT t.owner.name, t.owner = t.owner FROM tasks.task t WHERE REF(t) = p_task;\n";

    const DISTINCT_SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT (active BOOL NOT NULL);\n\
        CREATE TYPE tasks.task AS OBJECT (owner REF tasks.person, completed BOOL NOT NULL);\n\
        CREATE SERVER FUNCTION tasks.completion_values()\n\
        RETURNS ROWS (task REF tasks.task, active BOOL, completed BOOL)\n\
        SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
        AS SELECT DISTINCT REF(t), t.owner.active, t.completed FROM tasks.task t\n\
        WHERE t.completed = TRUE;\n";

    const DISTINCT_REFORMATTED_SOURCE: &str = "-- source-only DISTINCT edit\n\
        CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT ( active BOOL NOT NULL );\n\
        CREATE TYPE tasks.task AS OBJECT ( owner REF tasks.person, completed BOOL NOT NULL );\n\
        CREATE SERVER FUNCTION tasks.completion_values()\n\
        RETURNS ROWS ( task REF tasks.task, active BOOL, completed BOOL )\n\
        SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
        AS SELECT DISTINCT REF(t), t.owner.active, t.completed\n\
        FROM tasks.task t WHERE t.completed = TRUE;\n";

    const DISTINCT_REMOVED_SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT (active BOOL NOT NULL);\n\
        CREATE TYPE tasks.task AS OBJECT (owner REF tasks.person, completed BOOL NOT NULL);\n\
        CREATE SERVER FUNCTION tasks.completion_values()\n\
        RETURNS ROWS (task REF tasks.task, active BOOL, completed BOOL)\n\
        SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
        AS SELECT REF(t), t.owner.active, t.completed FROM tasks.task t\n\
        WHERE t.completed = TRUE;\n";

    const DISTINCT_REFERENCE_SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT (active BOOL NOT NULL);\n\
        CREATE TYPE tasks.task AS OBJECT (owner REF tasks.person);\n\
        CREATE SERVER FUNCTION tasks.owner_values()\n\
        RETURNS ROWS (owner REF tasks.person)\n\
        SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
        AS SELECT DISTINCT t.owner FROM tasks.task t;\n";

    const DIRECT_BOOLEAN_DISTINCT_SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT (active BOOL NOT NULL);\n\
        CREATE TYPE tasks.task AS OBJECT (owner REF tasks.person, completed BOOL NOT NULL);\n\
        CREATE SERVER FUNCTION tasks.visible_values()\n\
        RETURNS ROWS (completed BOOL)\n\
        SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
        AS SELECT DISTINCT t.completed FROM tasks.task t WHERE t.owner.active;\n";

    const DIRECT_BOOLEAN_DISTINCT_REFORMATTED_SOURCE: &str = "-- source-only direct DISTINCT edit\n\
        CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT ( active BOOL NOT NULL );\n\
        CREATE TYPE tasks.task AS OBJECT ( owner REF tasks.person, completed BOOL NOT NULL );\n\
        CREATE SERVER FUNCTION tasks.visible_values()\n\
        RETURNS ROWS ( completed BOOL )\n\
        SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
        AS SELECT DISTINCT t.completed\n\
        FROM tasks.task AS t WHERE t.owner.active;\n";

    const DIRECT_BOOLEAN_DISTINCT_CHANGED_PREDICATE_SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT (active BOOL NOT NULL);\n\
        CREATE TYPE tasks.task AS OBJECT (owner REF tasks.person, completed BOOL NOT NULL);\n\
        CREATE SERVER FUNCTION tasks.visible_values()\n\
        RETURNS ROWS (completed BOOL)\n\
        SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
        AS SELECT DISTINCT t.completed FROM tasks.task t WHERE t.completed;\n";

    const MUTATION_SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT (name TEXT NOT NULL);\n\
        CREATE TYPE tasks.task AS OBJECT (title TEXT NOT NULL, done BOOL NOT NULL, note TEXT, owner REF tasks.person);\n\
        CREATE SERVER FUNCTION tasks.create(p_title TEXT, p_unused INT, p_owner REF tasks.person)\n\
        RETURNS ROWS (result REF tasks.task) TRANSACTION ATOMIC\n\
        AS INSERT INTO tasks.task AS created (title, done, note, owner)\n\
        VALUES (p_title, FALSE, NULL, p_owner) RETURNING REF(created);\n";

    const MUTATION_REFORMATTED_SOURCE: &str = "-- source-only edit\n\
        CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT ( name TEXT NOT NULL );\n\
        CREATE TYPE tasks.task AS OBJECT ( title TEXT NOT NULL, done BOOL NOT NULL, note TEXT, owner REF tasks.person );\n\
        CREATE SERVER FUNCTION tasks.create( p_title TEXT, p_unused INT, p_owner REF tasks.person )\n\
        RETURNS ROWS ( result REF tasks.task ) TRANSACTION ATOMIC\n\
        AS INSERT INTO tasks.task AS created ( title, done, note, owner )\n\
        VALUES ( p_title, FALSE, NULL, p_owner ) RETURNING REF(created);\n";

    const UPDATE_SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT (name TEXT NOT NULL);\n\
        CREATE TYPE tasks.task AS OBJECT (title TEXT NOT NULL, done BOOL NOT NULL, owner REF tasks.person);\n\
        CREATE SERVER FUNCTION tasks.update(p_task REF tasks.task, p_title TEXT, p_owner REF tasks.person)\n\
        RETURNS ROWS (updated REF tasks.task) TRANSACTION ATOMIC\n\
        AS UPDATE tasks.task AS changed SET title = p_title, owner = p_owner\n\
        WHERE REF(changed) = p_task RETURNING REF(changed);\n";

    const DELETE_SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.task AS OBJECT (title TEXT NOT NULL);\n\
        CREATE SERVER FUNCTION tasks.remove(p_task REF tasks.task)\n\
        RETURNS ROWS (deleted BOOL) TRANSACTION ATOMIC\n\
        AS DELETE FROM tasks.task AS removed\n\
        WHERE REF(removed) = p_task RETURNING TRUE;\n";

    const MUTATION_CHANGED_SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT (name TEXT NOT NULL);\n\
        CREATE TYPE tasks.task AS OBJECT (title TEXT NOT NULL, done BOOL NOT NULL, note TEXT, owner REF tasks.person);\n\
        CREATE SERVER FUNCTION tasks.create(p_title TEXT, p_unused INT, p_owner REF tasks.person)\n\
        RETURNS ROWS (result REF tasks.task) TRANSACTION ATOMIC\n\
        AS INSERT INTO tasks.task AS created (title, done, note, owner)\n\
        VALUES (p_title, TRUE, NULL, p_owner) RETURNING REF(created);\n";

    const SHARED_EXPRESSION_SOURCE: &str = "CREATE SCHEMA demo;\n\
        CREATE TYPE demo.item AS OBJECT (first INT DEFAULT 1, second INT DEFAULT 1);\n";

    #[test]
    fn prepares_a_complete_source_catalogue_artifact_and_reference_revision() {
        let active = empty_active();
        let report = checked_report(SOURCE, active.catalogue());

        let prepared = prepare(&report, active.pair(), &active).unwrap();

        assert_eq!(prepared.expected_base(), active.pair());
        assert_eq!(prepared.source().parent(), Some(active.pair().source()));
        assert_eq!(prepared.source().units().len(), 1);
        assert_eq!(prepared.source().units()[0].logical_path(), "tasks.orna");
        assert_eq!(prepared.source().units()[0].content(), SOURCE);
        assert_eq!(
            source_unit_content_digest(SOURCE).unwrap(),
            prepared.source().units()[0].content_hash()
        );
        assert_eq!(
            source_bundle_digest(prepared.source().units()).unwrap(),
            prepared.source().bundle_hash()
        );
        assert_eq!(
            orna_core::canonical_hash::source_revision_digest(prepared.source()).unwrap(),
            prepared.source().revision_hash()
        );

        let catalogue = prepared.candidate();
        assert_eq!(catalogue.schemas().len(), 1);
        assert_eq!(catalogue.object_types().len(), 2);
        assert_eq!(catalogue.functions().len(), 1);
        assert_eq!(prepared.expressions().len(), 4);
        assert!(prepared.expressions().iter().all(|artifact| {
            artifact_payload_digest(artifact.payload()).unwrap() == artifact.content_hash()
        }));
        assert_eq!(prepared.new_function_revisions().len(), 1);
        assert_eq!(prepared.new_function_revisions()[0].revision_number(), 1);

        let task = catalogue
            .object_type_by_name(&semantic_name(&["tasks", "task"]))
            .unwrap();
        let person = catalogue
            .object_type_by_name(&semantic_name(&["tasks", "person"]))
            .unwrap();
        let title = task.field_by_name("title").unwrap();
        let completed = task.field_by_name("completed").unwrap();
        let priority = task.field_by_name("priority").unwrap();
        let note = task.field_by_name("note").unwrap();
        let assignee = task.field_by_name("assignee").unwrap();
        assert_eq!(
            assignee.resolved_type(),
            ResolvedType::reference(person.id())
        );
        assert_eq!(
            ConstantExpression::decode(expression(prepared.expressions(), title).payload())
                .unwrap(),
            ConstantExpression::Text("todo".to_owned())
        );
        assert_eq!(
            ConstantExpression::decode(expression(prepared.expressions(), completed).payload())
                .unwrap(),
            ConstantExpression::Boolean(false)
        );
        assert_eq!(
            ConstantExpression::decode(expression(prepared.expressions(), priority).payload())
                .unwrap(),
            ConstantExpression::Integer(7)
        );
        assert_eq!(
            ConstantExpression::decode(expression(prepared.expressions(), note).payload()).unwrap(),
            ConstantExpression::Null
        );

        let function = &catalogue.functions()[0];
        let revision = &prepared.new_function_revisions()[0];
        assert_eq!(function.current_revision(), revision.id());
        assert_eq!(
            artifact_payload_digest(revision.artifact().payload()).unwrap(),
            revision.artifact().content_hash()
        );
        let declaration_origin = revision.declaration_origin();
        let source = prepared
            .source()
            .units()
            .iter()
            .find(|unit| unit.id() == declaration_origin.source_unit())
            .unwrap();
        assert_eq!(
            function_declaration_digest(
                &source.content().as_bytes()[declaration_origin.byte_start() as usize
                    ..declaration_origin.byte_end() as usize]
            )
            .unwrap(),
            revision.declaration_content_hash()
        );
        let plan = ServerPlan::decode(revision.artifact().payload()).unwrap();
        assert_eq!(revision.artifact().version(), SERVER_PLAN_VERSION);
        assert_eq!(
            IdentitySelectedServerPlan::decode(revision.artifact().payload()),
            Err(ServerPlanError::UnsupportedVersion(SERVER_PLAN_VERSION))
        );
        assert_eq!(
            DistinctServerPlan::decode(revision.artifact().payload()),
            Err(ServerPlanError::UnsupportedVersion(SERVER_PLAN_VERSION))
        );
        assert_eq!(plan.scan.object_type, task.id());
        assert!(matches!(
            plan.projections[0].kind,
            ExpressionKind::ObjectReference { .. }
        ));
        let ExpressionKind::FieldPath { ref steps, .. } = plan.projections[1].kind else {
            panic!("second projection is not a field path");
        };
        assert_eq!(steps[0].owner, task.id());
        assert_eq!(steps[0].field, title.id());

        assert_eq!(prepared.references().len(), 6);
        assert_eq!(
            prepared
                .references()
                .iter()
                .map(|reference| reference.ordinal())
                .collect::<Vec<_>>(),
            (0..6).collect::<Vec<_>>()
        );
        assert!(prepared.references().iter().all(|reference| {
            reference.source_function() == function.id()
                && reference.source_revision() == revision.id()
        }));
        assert_eq!(
            prepared
                .references()
                .iter()
                .map(|reference| (reference.kind(), reference.target()))
                .collect::<Vec<_>>(),
            vec![
                (
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(task.id()),
                ),
                (
                    DefinitionReferenceKind::QueryObject,
                    DefinitionReferenceTarget::ObjectType(task.id()),
                ),
                (
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(task.id()),
                ),
                (
                    DefinitionReferenceKind::QueryField,
                    DefinitionReferenceTarget::Field {
                        owner: task.id(),
                        field: title.id(),
                    },
                ),
                (
                    DefinitionReferenceKind::QueryField,
                    DefinitionReferenceTarget::Field {
                        owner: task.id(),
                        field: completed.id(),
                    },
                ),
                (
                    DefinitionReferenceKind::QueryField,
                    DefinitionReferenceTarget::Field {
                        owner: task.id(),
                        field: title.id(),
                    },
                ),
            ]
        );
        assert_eq!(prepared.origins().len(), 16);
        assert_eq!(
            catalogue_digest(
                catalogue,
                prepared.new_function_revisions(),
                prepared.expressions(),
                prepared.origins(),
                prepared.references(),
            )
            .unwrap(),
            prepared.catalogue_hash()
        );
    }

    #[test]
    fn prepares_direct_boolean_predicates_as_version_one_server_plans_and_replays_by_semantics() {
        let empty = empty_active();
        let initial = prepare(
            &checked_report(DIRECT_BOOLEAN_SOURCE, empty.catalogue()),
            empty.pair(),
            &empty,
        )
        .unwrap();
        let catalogue = initial.candidate();
        let person = catalogue
            .object_type_by_name(&semantic_name(&["tasks", "person"]))
            .unwrap();
        let task = catalogue
            .object_type_by_name(&semantic_name(&["tasks", "task"]))
            .unwrap();
        let revision = &initial.new_function_revisions()[0];
        assert_eq!(revision.artifact().version(), SERVER_PLAN_VERSION);
        let plan = ServerPlan::decode(revision.artifact().payload()).unwrap();
        assert_eq!(
            IdentitySelectedServerPlan::decode(revision.artifact().payload()),
            Err(ServerPlanError::UnsupportedVersion(SERVER_PLAN_VERSION))
        );
        assert_eq!(
            DistinctServerPlan::decode(revision.artifact().payload()),
            Err(ServerPlanError::UnsupportedVersion(SERVER_PLAN_VERSION))
        );
        let selection = plan
            .selection
            .as_ref()
            .expect("fixture has a direct predicate");
        let ExpressionKind::FieldPath { input, steps } = &selection.kind else {
            panic!("direct predicate must encode as a field path");
        };
        assert_eq!(*input, 0);
        assert_eq!(
            steps
                .iter()
                .map(|step| (step.owner, step.field))
                .collect::<Vec<_>>(),
            vec![
                (task.id(), task.field_by_name("owner").unwrap().id()),
                (person.id(), person.field_by_name("active").unwrap().id()),
            ]
        );
        assert_eq!(
            selection.value_type.resolved_type,
            ResolvedType::scalar(StandardScalar::Boolean)
        );
        assert!(selection.value_type.nullable);
        assert_eq!(
            initial
                .references()
                .iter()
                .map(|reference| (reference.kind(), reference.target()))
                .collect::<Vec<_>>(),
            vec![
                (
                    DefinitionReferenceKind::QueryObject,
                    DefinitionReferenceTarget::ObjectType(task.id()),
                ),
                (
                    DefinitionReferenceKind::QueryField,
                    DefinitionReferenceTarget::Field {
                        owner: task.id(),
                        field: task.field_by_name("owner").unwrap().id(),
                    },
                ),
                (
                    DefinitionReferenceKind::QueryField,
                    DefinitionReferenceTarget::Field {
                        owner: person.id(),
                        field: person.field_by_name("active").unwrap().id(),
                    },
                ),
                (
                    DefinitionReferenceKind::QueryField,
                    DefinitionReferenceTarget::Field {
                        owner: task.id(),
                        field: task.field_by_name("completed").unwrap().id(),
                    },
                ),
                (
                    DefinitionReferenceKind::QueryField,
                    DefinitionReferenceTarget::Field {
                        owner: task.id(),
                        field: task.field_by_name("owner").unwrap().id(),
                    },
                ),
                (
                    DefinitionReferenceKind::QueryField,
                    DefinitionReferenceTarget::Field {
                        owner: person.id(),
                        field: person.field_by_name("active").unwrap().id(),
                    },
                ),
                (
                    DefinitionReferenceKind::QueryField,
                    DefinitionReferenceTarget::Field {
                        owner: task.id(),
                        field: task.field_by_name("completed").unwrap().id(),
                    },
                ),
            ]
        );

        let initial_revision = revision.clone();
        let active = activate(&initial, vec![initial_revision.clone()], Vec::new());
        let replay = prepare(
            &checked_report(DIRECT_BOOLEAN_REFORMATTED_SOURCE, active.catalogue()),
            active.pair(),
            &active,
        )
        .unwrap();
        assert!(replay.new_function_revisions().is_empty());
        assert_eq!(
            replay.candidate().functions()[0].current_revision(),
            initial_revision.id()
        );

        let changed = prepare(
            &checked_report(DIRECT_BOOLEAN_CHANGED_PREDICATE_SOURCE, active.catalogue()),
            active.pair(),
            &active,
        )
        .unwrap();
        let changed_revision = &changed.new_function_revisions()[0];
        assert_ne!(changed_revision.id(), initial_revision.id());
        assert_ne!(
            changed_revision.semantic_hash(),
            initial_revision.semantic_hash()
        );
        assert_ne!(
            changed_revision.artifact().content_hash(),
            initial_revision.artifact().content_hash()
        );
    }

    #[test]
    fn prepares_identity_selected_query_as_a_version_two_server_plan() {
        let active = empty_active();
        let prepared = prepare(
            &checked_report(IDENTITY_SELECTED_SOURCE, active.catalogue()),
            active.pair(),
            &active,
        )
        .unwrap();
        let catalogue = prepared.candidate();
        let task = catalogue
            .object_type_by_name(&semantic_name(&["tasks", "task"]))
            .unwrap();
        let function = &catalogue.functions()[0];
        let revision = &prepared.new_function_revisions()[0];
        assert_eq!(revision.artifact().format(), SERVER_PLAN_FORMAT);
        assert_eq!(revision.artifact().version(), 2);
        assert_eq!(
            artifact_payload_digest(revision.artifact().payload()).unwrap(),
            revision.artifact().content_hash()
        );
        let plan = IdentitySelectedServerPlan::decode(revision.artifact().payload()).unwrap();
        assert_eq!(plan.scan().object_type, task.id());
        assert_eq!(plan.selector().owner(), function.id());
        assert_eq!(plan.selector().parameter(), function.parameters()[0].id());
        assert!(ServerPlan::decode(revision.artifact().payload()).is_err());
        assert!(DistinctServerPlan::decode(revision.artifact().payload()).is_err());
        assert_eq!(
            prepared
                .references()
                .iter()
                .map(|reference| (reference.kind(), reference.target()))
                .collect::<Vec<_>>(),
            vec![
                (
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(task.id())
                ),
                (
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(task.id())
                ),
                (
                    DefinitionReferenceKind::QueryObject,
                    DefinitionReferenceTarget::ObjectType(task.id())
                ),
                (
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(task.id())
                ),
                (
                    DefinitionReferenceKind::QueryField,
                    DefinitionReferenceTarget::Field {
                        owner: task.id(),
                        field: task.field_by_name("title").unwrap().id()
                    }
                ),
                (
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(task.id())
                ),
                (
                    DefinitionReferenceKind::ParameterRead,
                    DefinitionReferenceTarget::Parameter {
                        owner: function.id(),
                        parameter: function.parameters()[0].id()
                    }
                ),
            ]
        );
    }

    #[test]
    fn prepares_distinct_query_as_a_version_three_server_plan_with_exact_evidence() {
        let active = empty_active();
        let prepared = prepare(
            &checked_report(DISTINCT_SOURCE, active.catalogue()),
            active.pair(),
            &active,
        )
        .unwrap();

        let catalogue = prepared.candidate();
        let person = catalogue
            .object_type_by_name(&semantic_name(&["tasks", "person"]))
            .unwrap();
        let task = catalogue
            .object_type_by_name(&semantic_name(&["tasks", "task"]))
            .unwrap();
        let function = &catalogue.functions()[0];
        let revision = &prepared.new_function_revisions()[0];
        assert_eq!(revision.artifact().kind(), ExecutableArtifactKind::Server);
        assert_eq!(revision.artifact().format(), SERVER_PLAN_FORMAT);
        assert_eq!(
            artifact_payload_digest(revision.artifact().payload()).unwrap(),
            revision.artifact().content_hash()
        );
        assert_eq!(revision.language_version(), SERVER_PLAN_LANGUAGE_VERSION);
        assert_eq!(
            function_semantic_digest(
                function,
                revision.language_version(),
                revision.artifact(),
                prepared.expressions(),
                prepared.references(),
            )
            .unwrap(),
            revision.semantic_hash()
        );
        assert_eq!(function.domain(), FunctionDomain::Server);
        assert_eq!(function.security(), FunctionSecurity::Invoker);
        assert_eq!(function.transaction(), Some(FunctionTransaction::ReadOnly));
        assert_eq!(function.volatility(), FunctionVolatility::Stable);
        assert!(function.parameters().is_empty());
        assert!(matches!(
            function.return_type(),
            FunctionReturn::Rows(columns)
                if columns.iter().map(FunctionReturnColumnDefinition::resolved_type).collect::<Vec<_>>()
                    == vec![
                        ResolvedType::reference(task.id()),
                        ResolvedType::scalar(StandardScalar::Boolean),
                        ResolvedType::scalar(StandardScalar::Boolean),
                    ]
        ));

        let plan = DistinctServerPlan::decode(revision.artifact().payload()).unwrap();
        assert_eq!(revision.artifact().version(), plan.format_version());
        assert_eq!(plan.scan().input, 0);
        assert_eq!(plan.scan().object_type, task.id());
        assert_eq!(plan.projections().len(), 3);
        assert!(matches!(
            plan.projections()[0].kind,
            ExpressionKind::ObjectReference { input: 0 }
        ));
        assert_eq!(
            plan.projections()[0].value_type.resolved_type,
            ResolvedType::reference(task.id())
        );
        assert!(!plan.projections()[0].value_type.nullable);
        let ExpressionKind::FieldPath { input, steps } = &plan.projections()[1].kind else {
            panic!("second DISTINCT projection must be a field path");
        };
        assert_eq!(*input, 0);
        assert_eq!(
            steps
                .iter()
                .map(|step| (step.owner, step.field))
                .collect::<Vec<_>>(),
            vec![
                (task.id(), task.field_by_name("owner").unwrap().id()),
                (person.id(), person.field_by_name("active").unwrap().id()),
            ]
        );
        assert_eq!(
            plan.projections()[1].value_type.resolved_type,
            ResolvedType::scalar(StandardScalar::Boolean)
        );
        assert!(plan.projections()[1].value_type.nullable);
        let ExpressionKind::FieldPath { input, steps } = &plan.projections()[2].kind else {
            panic!("third DISTINCT projection must be a field path");
        };
        assert_eq!(*input, 0);
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].owner, task.id());
        assert_eq!(
            steps[0].field,
            task.field_by_name("completed").unwrap().id()
        );
        assert_eq!(
            plan.projections()[2].value_type.resolved_type,
            ResolvedType::scalar(StandardScalar::Boolean)
        );
        assert!(!plan.projections()[2].value_type.nullable);
        let selection = plan.selection().expect("fixture has a selection");
        assert_eq!(
            selection.value_type.resolved_type,
            ResolvedType::scalar(StandardScalar::Boolean)
        );
        assert!(!selection.value_type.nullable);
        assert!(matches!(selection.kind, ExpressionKind::Equality { .. }));
        assert!(ServerPlan::decode(revision.artifact().payload()).is_err());
        assert!(IdentitySelectedServerPlan::decode(revision.artifact().payload()).is_err());

        assert_eq!(
            prepared
                .references()
                .iter()
                .map(|reference| (reference.kind(), reference.target()))
                .collect::<Vec<_>>(),
            vec![
                (
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(task.id()),
                ),
                (
                    DefinitionReferenceKind::QueryObject,
                    DefinitionReferenceTarget::ObjectType(task.id()),
                ),
                (
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(task.id()),
                ),
                (
                    DefinitionReferenceKind::QueryField,
                    DefinitionReferenceTarget::Field {
                        owner: task.id(),
                        field: task.field_by_name("owner").unwrap().id(),
                    },
                ),
                (
                    DefinitionReferenceKind::QueryField,
                    DefinitionReferenceTarget::Field {
                        owner: person.id(),
                        field: person.field_by_name("active").unwrap().id(),
                    },
                ),
                (
                    DefinitionReferenceKind::QueryField,
                    DefinitionReferenceTarget::Field {
                        owner: task.id(),
                        field: task.field_by_name("completed").unwrap().id(),
                    },
                ),
                (
                    DefinitionReferenceKind::QueryField,
                    DefinitionReferenceTarget::Field {
                        owner: task.id(),
                        field: task.field_by_name("completed").unwrap().id(),
                    },
                ),
            ]
        );
        assert_eq!(
            prepared
                .references()
                .iter()
                .map(|reference| reference.ordinal())
                .collect::<Vec<_>>(),
            (0..7).collect::<Vec<_>>()
        );
    }

    #[test]
    fn prepares_direct_boolean_distinct_predicates_as_v3_and_replays_by_semantics() {
        let empty = empty_active();
        let initial = prepare(
            &checked_report(DIRECT_BOOLEAN_DISTINCT_SOURCE, empty.catalogue()),
            empty.pair(),
            &empty,
        )
        .unwrap();
        let catalogue = initial.candidate();
        let person = catalogue
            .object_type_by_name(&semantic_name(&["tasks", "person"]))
            .unwrap();
        let task = catalogue
            .object_type_by_name(&semantic_name(&["tasks", "task"]))
            .unwrap();
        let function = &catalogue.functions()[0];
        let revision = &initial.new_function_revisions()[0];

        assert_eq!(revision.revision_number(), 1);
        assert_eq!(revision.artifact().kind(), ExecutableArtifactKind::Server);
        assert_eq!(revision.artifact().format(), SERVER_PLAN_FORMAT);
        assert_eq!(revision.language_version(), SERVER_PLAN_LANGUAGE_VERSION);
        assert_eq!(
            artifact_payload_digest(revision.artifact().payload()).unwrap(),
            revision.artifact().content_hash()
        );
        assert_eq!(
            function_semantic_digest(
                function,
                revision.language_version(),
                revision.artifact(),
                initial.expressions(),
                initial.references(),
            )
            .unwrap(),
            revision.semantic_hash()
        );

        let plan = DistinctServerPlan::decode(revision.artifact().payload()).unwrap();
        let format_version = plan.format_version();
        assert_eq!(revision.artifact().version(), format_version);
        assert_eq!(plan.encode().unwrap(), revision.artifact().payload());
        assert_eq!(
            ServerPlan::decode(revision.artifact().payload()),
            Err(ServerPlanError::UnsupportedVersion(format_version))
        );
        assert_eq!(
            IdentitySelectedServerPlan::decode(revision.artifact().payload()),
            Err(ServerPlanError::UnsupportedVersion(format_version))
        );
        assert_eq!(plan.scan().input, 0);
        assert_eq!(plan.scan().object_type, task.id());
        assert_eq!(plan.projections().len(), 1);
        let ExpressionKind::FieldPath { input, steps } = &plan.projections()[0].kind else {
            panic!("direct DISTINCT projection must encode as a field path");
        };
        assert_eq!(*input, 0);
        assert_eq!(
            steps
                .iter()
                .map(|step| (step.owner, step.field))
                .collect::<Vec<_>>(),
            vec![(task.id(), task.field_by_name("completed").unwrap().id())]
        );
        assert_eq!(
            plan.projections()[0].value_type.resolved_type,
            ResolvedType::scalar(StandardScalar::Boolean)
        );
        assert!(!plan.projections()[0].value_type.nullable);

        let selection = plan.selection().expect("fixture has a direct predicate");
        let ExpressionKind::FieldPath { input, steps } = &selection.kind else {
            panic!("direct DISTINCT predicate must encode as a field path");
        };
        assert_eq!(*input, 0);
        assert_eq!(
            steps
                .iter()
                .map(|step| (step.owner, step.field))
                .collect::<Vec<_>>(),
            vec![
                (task.id(), task.field_by_name("owner").unwrap().id()),
                (person.id(), person.field_by_name("active").unwrap().id()),
            ]
        );
        assert_eq!(
            selection.value_type.resolved_type,
            ResolvedType::scalar(StandardScalar::Boolean)
        );
        assert!(selection.value_type.nullable);

        assert_eq!(
            initial
                .references()
                .iter()
                .map(|reference| (reference.kind(), reference.target()))
                .collect::<Vec<_>>(),
            vec![
                (
                    DefinitionReferenceKind::QueryObject,
                    DefinitionReferenceTarget::ObjectType(task.id()),
                ),
                (
                    DefinitionReferenceKind::QueryField,
                    DefinitionReferenceTarget::Field {
                        owner: task.id(),
                        field: task.field_by_name("completed").unwrap().id(),
                    },
                ),
                (
                    DefinitionReferenceKind::QueryField,
                    DefinitionReferenceTarget::Field {
                        owner: task.id(),
                        field: task.field_by_name("owner").unwrap().id(),
                    },
                ),
                (
                    DefinitionReferenceKind::QueryField,
                    DefinitionReferenceTarget::Field {
                        owner: person.id(),
                        field: person.field_by_name("active").unwrap().id(),
                    },
                ),
            ]
        );
        assert_eq!(
            initial
                .references()
                .iter()
                .map(|reference| reference.ordinal())
                .collect::<Vec<_>>(),
            (0..4).collect::<Vec<_>>()
        );
        assert!(initial.references().iter().all(|reference| {
            reference.source_function() == function.id()
                && reference.source_revision() == revision.id()
        }));

        let initial_revision = revision.clone();
        let active = activate(&initial, vec![initial_revision.clone()], Vec::new());
        let replay = prepare(
            &checked_report(
                DIRECT_BOOLEAN_DISTINCT_REFORMATTED_SOURCE,
                active.catalogue(),
            ),
            active.pair(),
            &active,
        )
        .unwrap();
        assert!(replay.new_function_revisions().is_empty());
        assert_eq!(
            replay.candidate().functions()[0].current_revision(),
            initial_revision.id()
        );
        assert_ne!(replay.source().id(), active.source().id());
        assert_eq!(
            active.function_revisions(),
            std::slice::from_ref(&initial_revision)
        );
        assert_eq!(
            active.function_revisions()[0].artifact(),
            revision.artifact()
        );

        let changed = prepare(
            &checked_report(
                DIRECT_BOOLEAN_DISTINCT_CHANGED_PREDICATE_SOURCE,
                active.catalogue(),
            ),
            active.pair(),
            &active,
        )
        .unwrap();
        let changed_function = &changed.candidate().functions()[0];
        let changed_revision = &changed.new_function_revisions()[0];
        assert_eq!(changed_function.id(), function.id());
        assert_eq!(changed_revision.revision_number(), 2);
        assert_ne!(changed_revision.id(), initial_revision.id());
        assert_ne!(
            changed_revision.semantic_hash(),
            initial_revision.semantic_hash()
        );
        assert_ne!(
            changed_revision.artifact().content_hash(),
            initial_revision.artifact().content_hash()
        );
        assert_eq!(changed_revision.artifact().version(), format_version);
        let changed_plan =
            DistinctServerPlan::decode(changed_revision.artifact().payload()).unwrap();
        let changed_selection = changed_plan
            .selection()
            .expect("changed fixture has a direct predicate");
        let ExpressionKind::FieldPath { input, steps } = &changed_selection.kind else {
            panic!("changed direct DISTINCT predicate must encode as a field path");
        };
        assert_eq!(*input, 0);
        assert_eq!(
            steps
                .iter()
                .map(|step| (step.owner, step.field))
                .collect::<Vec<_>>(),
            vec![(task.id(), task.field_by_name("completed").unwrap().id())]
        );
        assert!(!changed_selection.value_type.nullable);

        let (mapped_plan, mapped_function, object_types, references) =
            mapped_distinct_fixture_for(DIRECT_BOOLEAN_DISTINCT_SOURCE);
        let mapped_person = object_types
            .iter()
            .find(|object_type| object_type.name() == &semantic_name(&["tasks", "person"]))
            .unwrap();
        let non_nullable_owner = object_types_with_task_field(
            &object_types,
            "owner",
            ResolvedType::reference(mapped_person.id()),
            false,
        );
        assert_preparation_reason(
            distinct_query_plan(
                &mapped_plan,
                &mapped_function,
                &non_nullable_owner,
                &references,
            ),
            "SELECT DISTINCT query field path type differs from its source field",
        );
    }

    #[test]
    fn distinct_replay_reuses_its_revision_and_removing_distinct_creates_version_one() {
        let empty = empty_active();
        let initial = prepare(
            &checked_report(DISTINCT_SOURCE, empty.catalogue()),
            empty.pair(),
            &empty,
        )
        .unwrap();
        let initial_revision = initial.new_function_revisions()[0].clone();
        let active = activate(&initial, vec![initial_revision.clone()], Vec::new());

        for source in [DISTINCT_SOURCE, DISTINCT_REFORMATTED_SOURCE] {
            let replay = prepare(
                &checked_report(source, active.catalogue()),
                active.pair(),
                &active,
            )
            .unwrap();
            assert!(replay.new_function_revisions().is_empty());
            assert_eq!(
                replay.candidate().functions()[0].current_revision(),
                initial_revision.id()
            );
            assert_eq!(
                active.function_revisions(),
                std::slice::from_ref(&initial_revision)
            );
            assert_eq!(active.function_revisions()[0], initial_revision);
            assert_eq!(
                active.function_revisions()[0].artifact(),
                initial_revision.artifact()
            );
        }

        let removed = prepare(
            &checked_report(DISTINCT_REMOVED_SOURCE, active.catalogue()),
            active.pair(),
            &active,
        )
        .unwrap();
        let changed = &removed.new_function_revisions()[0];
        assert_ne!(changed.id(), initial_revision.id());
        assert_ne!(changed.semantic_hash(), initial_revision.semantic_hash());
        assert_ne!(
            changed.artifact().content_hash(),
            initial_revision.artifact().content_hash()
        );
        assert_eq!(changed.artifact().version(), SERVER_PLAN_VERSION);
        assert!(ServerPlan::decode(changed.artifact().payload()).is_ok());
        assert!(DistinctServerPlan::decode(changed.artifact().payload()).is_err());
    }

    #[test]
    fn distinct_preparation_validates_header_facts_in_the_accepted_order() {
        let (plan, function, object_types, references) = mapped_distinct_fixture();
        assert!(distinct_query_plan(&plan, &function, &object_types, &references).is_ok());

        assert_preparation_reason(
            distinct_query_plan(&plan, &function, &[], &references),
            "SELECT DISTINCT query scan object is absent from the candidate catalogue",
        );

        let function_with = |domain, parameters, return_type, security, transaction, volatility| {
            FunctionDefinition::new(
                function.id(),
                function.name().clone(),
                domain,
                parameters,
                return_type,
                function.current_revision(),
                security,
                transaction,
                volatility,
            )
        };
        let bad_mode = function_with(
            FunctionDomain::Client,
            function.parameters().to_vec(),
            function.return_type().clone(),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert_preparation_reason(
            distinct_query_plan(
                &plan,
                &bad_mode,
                &object_types,
                &distinct_query_reference_sequence(&plan, &bad_mode),
            ),
            "SELECT DISTINCT query function has unsupported execution modes",
        );

        let parameterised = function_with(
            FunctionDomain::Server,
            vec![ParameterDefinition::new(
                ParameterId::new(),
                "unexpected",
                0,
                ResolvedType::scalar(StandardScalar::Boolean),
                None,
            )],
            function.return_type().clone(),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert_preparation_reason(
            distinct_query_plan(
                &plan,
                &parameterised,
                &object_types,
                &distinct_query_reference_sequence(&plan, &parameterised),
            ),
            "SELECT DISTINCT query function declares parameters",
        );

        let single = function_with(
            FunctionDomain::Server,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Boolean)),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert_preparation_reason(
            distinct_query_plan(
                &plan,
                &single,
                &object_types,
                &distinct_query_reference_sequence(&plan, &single),
            ),
            "SELECT DISTINCT query function does not return ROWS",
        );

        let empty_rows = function_with(
            FunctionDomain::Server,
            Vec::new(),
            FunctionReturn::Rows(Vec::new()),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert_preparation_reason(
            distinct_query_plan(
                &plan,
                &empty_rows,
                &object_types,
                &distinct_query_reference_sequence(&plan, &empty_rows),
            ),
            "SELECT DISTINCT query function returns empty ROWS",
        );

        let wrong_count = function_with(
            FunctionDomain::Server,
            Vec::new(),
            FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
                "one",
                0,
                ResolvedType::scalar(StandardScalar::Boolean),
            )]),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert_preparation_reason(
            distinct_query_plan(
                &plan,
                &wrong_count,
                &object_types,
                &distinct_query_reference_sequence(&plan, &wrong_count),
            ),
            "SELECT DISTINCT query projection count differs from its function return",
        );
    }

    #[test]
    fn version_one_preparation_revalidates_headers_facts_and_evidence_before_encoding() {
        let (plan, function, object_types, references) = mapped_version_one_fixture();
        assert!(version_one_query_plan(&plan, &function, &object_types, &references).is_ok());

        let function_with = |domain, parameters, return_type, security, transaction, volatility| {
            FunctionDefinition::new(
                function.id(),
                function.name().clone(),
                domain,
                parameters,
                return_type,
                function.current_revision(),
                security,
                transaction,
                volatility,
            )
        };
        for (transaction, volatility) in [
            (None, FunctionVolatility::Immutable),
            (
                Some(FunctionTransaction::Atomic),
                FunctionVolatility::Volatile,
            ),
            (
                Some(FunctionTransaction::ReadOnly),
                FunctionVolatility::Stable,
            ),
        ] {
            let accepted = function_with(
                FunctionDomain::Server,
                Vec::new(),
                function.return_type().clone(),
                FunctionSecurity::Invoker,
                transaction,
                volatility,
            );
            assert!(
                version_one_query_plan(
                    &plan,
                    &accepted,
                    &object_types,
                    &version_one_query_reference_sequence(&plan, &accepted),
                )
                .is_ok()
            );
        }

        assert_preparation_reason(
            version_one_query_plan(
                &plan.with_test_mutation(
                    crate::relational::RelationalQueryTestMutation::InvalidScan,
                ),
                &function,
                &object_types,
                &references,
            ),
            "SERVER SELECT query scan object is absent from the candidate catalogue",
        );

        let manual = function_with(
            FunctionDomain::Server,
            Vec::new(),
            function.return_type().clone(),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::Manual),
            function.volatility(),
        );
        assert_preparation_reason(
            version_one_query_plan(
                &plan,
                &manual,
                &object_types,
                &version_one_query_reference_sequence(&plan, &manual),
            ),
            "SERVER SELECT query function has unsupported execution modes",
        );
        for (domain, security) in [
            (FunctionDomain::Client, FunctionSecurity::Invoker),
            (FunctionDomain::Server, FunctionSecurity::Definer),
        ] {
            let unsupported = function_with(
                domain,
                Vec::new(),
                function.return_type().clone(),
                security,
                function.transaction(),
                function.volatility(),
            );
            assert_preparation_reason(
                version_one_query_plan(
                    &plan,
                    &unsupported,
                    &object_types,
                    &version_one_query_reference_sequence(&plan, &unsupported),
                ),
                "SERVER SELECT query function has unsupported execution modes",
            );
        }

        let parameterised = function_with(
            FunctionDomain::Server,
            vec![ParameterDefinition::new(
                ParameterId::new(),
                "unexpected",
                0,
                ResolvedType::scalar(StandardScalar::Boolean),
                None,
            )],
            function.return_type().clone(),
            FunctionSecurity::Invoker,
            function.transaction(),
            function.volatility(),
        );
        assert_preparation_reason(
            version_one_query_plan(
                &plan,
                &parameterised,
                &object_types,
                &version_one_query_reference_sequence(&plan, &parameterised),
            ),
            "SERVER SELECT query function declares parameters",
        );

        let single = function_with(
            FunctionDomain::Server,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Boolean)),
            FunctionSecurity::Invoker,
            function.transaction(),
            function.volatility(),
        );
        assert_preparation_reason(
            version_one_query_plan(
                &plan,
                &single,
                &object_types,
                &version_one_query_reference_sequence(&plan, &single),
            ),
            "SERVER SELECT query function does not return ROWS",
        );

        let empty_rows = function_with(
            FunctionDomain::Server,
            Vec::new(),
            FunctionReturn::Rows(Vec::new()),
            FunctionSecurity::Invoker,
            function.transaction(),
            function.volatility(),
        );
        assert_preparation_reason(
            version_one_query_plan(
                &plan,
                &empty_rows,
                &object_types,
                &version_one_query_reference_sequence(&plan, &empty_rows),
            ),
            "SERVER SELECT query function returns empty ROWS",
        );

        let wrong_count = function_with(
            FunctionDomain::Server,
            Vec::new(),
            FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
                "only",
                0,
                ResolvedType::scalar(StandardScalar::Boolean),
            )]),
            FunctionSecurity::Invoker,
            function.transaction(),
            function.volatility(),
        );
        assert_preparation_reason(
            version_one_query_plan(
                &plan,
                &wrong_count,
                &object_types,
                &version_one_query_reference_sequence(&plan, &wrong_count),
            ),
            "SERVER SELECT query projection count differs from its function return",
        );

        let FunctionReturn::Rows(columns) = function.return_type() else {
            panic!("fixture must return rows");
        };
        let mut wrong_columns = columns.to_vec();
        wrong_columns[1] = FunctionReturnColumnDefinition::new(
            wrong_columns[1].name(),
            wrong_columns[1].ordinal(),
            ResolvedType::scalar(StandardScalar::Boolean),
        );
        let wrong_return = function_with(
            FunctionDomain::Server,
            Vec::new(),
            FunctionReturn::Rows(wrong_columns),
            FunctionSecurity::Invoker,
            function.transaction(),
            function.volatility(),
        );
        assert_preparation_reason(
            version_one_query_plan(
                &plan,
                &wrong_return,
                &object_types,
                &version_one_query_reference_sequence(&plan, &wrong_return),
            ),
            "SERVER SELECT query projection differs from its function return",
        );

        for (mutation, reason) in [
            (
                crate::relational::RelationalQueryTestMutation::InvalidProjectionFieldPathInput,
                "SERVER SELECT query field path has an invalid input or is empty",
            ),
            (
                crate::relational::RelationalQueryTestMutation::InvalidObjectReferenceInput,
                "SERVER SELECT query object reference has inconsistent facts",
            ),
            (
                crate::relational::RelationalQueryTestMutation::InvalidBooleanLiteralType,
                "SERVER SELECT query BOOLEAN expression has inconsistent type facts",
            ),
            (
                crate::relational::RelationalQueryTestMutation::InvalidEqualityType,
                "SERVER SELECT query equality expression has inconsistent type facts",
            ),
            (
                crate::relational::RelationalQueryTestMutation::InvalidOrderingFieldPathInput,
                "SERVER SELECT query field path has an invalid input or is empty",
            ),
            (
                crate::relational::RelationalQueryTestMutation::SelectionObjectReference,
                "SERVER SELECT query selection is not BOOLEAN",
            ),
        ] {
            let malformed = plan.with_test_mutation(mutation);
            assert_preparation_reason(
                version_one_query_plan(
                    &malformed,
                    &function,
                    &object_types,
                    &version_one_query_reference_sequence(&malformed, &function),
                ),
                reason,
            );
        }

        let unknown_field = plan
            .try_map_identities(Ok::<_, PrepareError>, |_| {
                Ok::<_, PrepareError>(FieldId::new())
            })
            .unwrap();
        assert_preparation_reason(
            version_one_query_plan(
                &unknown_field,
                &function,
                &object_types,
                &version_one_query_reference_sequence(&unknown_field, &function),
            ),
            "SERVER SELECT query field path field is absent from its source object",
        );
        let wrong_owner = plan
            .try_map_identities(
                {
                    let mut calls = 0;
                    move |type_id| {
                        calls += 1;
                        Ok::<_, PrepareError>(if calls == 3 { TypeId::new() } else { type_id })
                    }
                },
                Ok::<_, PrepareError>,
            )
            .unwrap();
        assert_preparation_reason(
            version_one_query_plan(
                &wrong_owner,
                &function,
                &object_types,
                &version_one_query_reference_sequence(&wrong_owner, &function),
            ),
            "SERVER SELECT query field path owner differs from its source object",
        );
        assert_preparation_reason(
            version_one_query_plan(
                &plan,
                &function,
                &object_types_with_task_field(
                    &object_types,
                    "title",
                    ResolvedType::scalar(StandardScalar::Boolean),
                    true,
                ),
                &references,
            ),
            "SERVER SELECT query field path type differs from its source field",
        );
        assert_preparation_reason(
            version_one_query_plan(
                &plan,
                &function,
                &object_types_with_task_field(
                    &object_types,
                    "title",
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    false,
                ),
                &references,
            ),
            "SERVER SELECT query field path type differs from its source field",
        );
        assert_preparation_reason(
            plan.try_map_identities(
                |_| {
                    Err::<TypeId, _>(PrepareError::InvalidCheckedBundle {
                        reason: "type mapping failure",
                    })
                },
                Ok,
            ),
            "type mapping failure",
        );
        assert_preparation_reason(
            plan.try_map_identities(Ok::<_, PrepareError>, |_| {
                Err::<FieldId, _>(PrepareError::InvalidCheckedBundle {
                    reason: "field mapping failure",
                })
            }),
            "field mapping failure",
        );

        let mut wrong_evidence = references.clone();
        wrong_evidence.reverse();
        assert_preparation_reason(
            version_one_query_plan(&plan, &function, &object_types, &wrong_evidence),
            "SERVER SELECT definition references differ from the checked function body",
        );
        assert_preparation_reason(
            version_one_query_plan(
                &plan,
                &function,
                &object_types,
                &references[..references.len() - 1],
            ),
            "SERVER SELECT definition references differ from the checked function body",
        );
        let mut extra_evidence = references.clone();
        extra_evidence.push(references[0]);
        assert_preparation_reason(
            version_one_query_plan(&plan, &function, &object_types, &extra_evidence),
            "SERVER SELECT definition references differ from the checked function body",
        );
        let mut wrong_kind = references.clone();
        wrong_kind[0].0 = DefinitionReferenceKind::QueryObject;
        assert_preparation_reason(
            version_one_query_plan(&plan, &function, &object_types, &wrong_kind),
            "SERVER SELECT definition references differ from the checked function body",
        );
        let mut wrong_target = references.clone();
        wrong_target[0].1 = DefinitionReferenceTarget::ObjectType(TypeId::new());
        assert_preparation_reason(
            version_one_query_plan(&plan, &function, &object_types, &wrong_target),
            "SERVER SELECT definition references differ from the checked function body",
        );

        let (direct_plan, direct_function, direct_objects, _) =
            mapped_version_one_fixture_for(DIRECT_BOOLEAN_SOURCE);
        let direct_references =
            version_one_query_reference_sequence(&direct_plan, &direct_function);
        assert_preparation_reason(
            version_one_query_plan(
                &direct_plan,
                &direct_function,
                &object_types_with_task_field(
                    &direct_objects,
                    "owner",
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    true,
                ),
                &direct_references,
            ),
            "SERVER SELECT query field path continues through a non-reference field",
        );
        let direct_task = direct_objects
            .iter()
            .find(|object_type| object_type.name() == &semantic_name(&["tasks", "task"]))
            .unwrap()
            .clone();
        assert_preparation_reason(
            version_one_query_plan(
                &direct_plan,
                &direct_function,
                std::slice::from_ref(&direct_task),
                &direct_references,
            ),
            "SERVER SELECT query field path target is absent from the candidate catalogue",
        );

        let (reference_plan, reference_function, reference_objects, _) =
            mapped_version_one_fixture_for(VERSION_ONE_REFERENCE_SOURCE);
        let reference_task = reference_objects
            .iter()
            .find(|object_type| object_type.name() == &semantic_name(&["tasks", "task"]))
            .unwrap()
            .clone();
        assert_preparation_reason(
            version_one_query_plan(
                &reference_plan,
                &reference_function,
                std::slice::from_ref(&reference_task),
                &version_one_query_reference_sequence(&reference_plan, &reference_function),
            ),
            "SERVER SELECT query field path target is absent from the candidate catalogue",
        );
    }

    #[test]
    fn distinct_preparation_revalidates_candidate_facts_and_evidence() {
        let (plan, function, object_types, references) = mapped_distinct_fixture();
        let person = object_types
            .iter()
            .find(|object_type| object_type.name() == &semantic_name(&["tasks", "person"]))
            .unwrap()
            .clone();
        let task = object_types
            .iter()
            .find(|object_type| object_type.name() == &semantic_name(&["tasks", "task"]))
            .unwrap()
            .clone();
        let owner = task.field_by_name("owner").unwrap().clone();
        let completed = task.field_by_name("completed").unwrap().clone();

        let missing_field_plan = mapped_distinct_plan(&plan, Ok, |_| {
            Err::<FieldId, _>(PrepareError::InvalidCheckedBundle {
                reason: "mapping failure",
            })
        });
        assert_preparation_reason(missing_field_plan, "mapping failure");

        let missing_type_plan = mapped_distinct_plan(
            &plan,
            |_| {
                Err::<TypeId, _>(PrepareError::InvalidCheckedBundle {
                    reason: "type mapping failure",
                })
            },
            Ok,
        );
        assert_preparation_reason(missing_type_plan, "type mapping failure");

        let object_reference_mismatch = mapped_distinct_plan(
            &plan,
            {
                let mut calls = 0;
                let task = task.id();
                let person = person.id();
                move |_| {
                    calls += 1;
                    Ok(if calls == 2 { person } else { task })
                }
            },
            Ok,
        )
        .unwrap();
        assert_preparation_reason(
            distinct_query_plan(
                &object_reference_mismatch,
                &function,
                &object_types,
                &distinct_query_reference_sequence(&object_reference_mismatch, &function),
            ),
            "SELECT DISTINCT query object reference has inconsistent facts",
        );

        let initial_owner_mismatch = mapped_distinct_plan(
            &plan,
            {
                let mut calls = 0;
                let task = task.id();
                let person = person.id();
                move |_| {
                    calls += 1;
                    Ok(if calls == 3 { person } else { task })
                }
            },
            Ok,
        )
        .unwrap();
        assert_preparation_reason(
            distinct_query_plan(
                &initial_owner_mismatch,
                &function,
                &object_types,
                &distinct_query_reference_sequence(&initial_owner_mismatch, &function),
            ),
            "SELECT DISTINCT query field path owner differs from its source object",
        );

        for (mutation, reason) in [
            (
                crate::relational::DistinctQueryTestMutation::InvalidFieldPathInput,
                "SELECT DISTINCT query field path has an invalid input or is empty",
            ),
            (
                crate::relational::DistinctQueryTestMutation::InvalidObjectReferenceInput,
                "SELECT DISTINCT query object reference has inconsistent facts",
            ),
            (
                crate::relational::DistinctQueryTestMutation::InvalidObjectReferenceType,
                "SELECT DISTINCT query object reference has inconsistent facts",
            ),
            (
                crate::relational::DistinctQueryTestMutation::InvalidBooleanLiteralType,
                "SELECT DISTINCT query BOOLEAN expression has inconsistent type facts",
            ),
            (
                crate::relational::DistinctQueryTestMutation::InvalidEqualityType,
                "SELECT DISTINCT query equality expression has inconsistent type facts",
            ),
        ] {
            let malformed = plan.with_test_mutation(mutation);
            assert_preparation_reason(
                distinct_query_plan(
                    &malformed,
                    &function,
                    &object_types,
                    &distinct_query_reference_sequence(&malformed, &function),
                ),
                reason,
            );
        }

        let unknown_field = mapped_distinct_plan(&plan, Ok, |field_id| {
            if field_id == owner.id() {
                Ok(FieldId::new())
            } else {
                Ok(field_id)
            }
        })
        .unwrap();
        assert_preparation_reason(
            distinct_query_plan(
                &unknown_field,
                &function,
                &object_types,
                &distinct_query_reference_sequence(&unknown_field, &function),
            ),
            "SELECT DISTINCT query field path field is absent from its source object",
        );

        let wrong_final_type = ObjectTypeDefinition::new(
            task.id(),
            task.name().clone(),
            vec![
                owner.clone(),
                FieldDefinition::new(
                    completed.id(),
                    completed.name(),
                    completed.ordinal(),
                    ResolvedType::scalar(StandardScalar::Integer),
                    completed.nullable(),
                    completed.unique(),
                    completed.default_expression(),
                    completed.on_delete(),
                ),
            ],
        );
        assert_preparation_reason(
            distinct_query_plan(
                &plan,
                &function,
                &[person.clone(), wrong_final_type],
                &references,
            ),
            "SELECT DISTINCT query field path type differs from its source field",
        );

        let nullable_final = ObjectTypeDefinition::new(
            task.id(),
            task.name().clone(),
            vec![
                owner.clone(),
                FieldDefinition::new(
                    completed.id(),
                    completed.name(),
                    completed.ordinal(),
                    completed.resolved_type(),
                    true,
                    completed.unique(),
                    completed.default_expression(),
                    completed.on_delete(),
                ),
            ],
        );
        assert_preparation_reason(
            distinct_query_plan(
                &plan,
                &function,
                &[person.clone(), nullable_final],
                &references,
            ),
            "SELECT DISTINCT query field path type differs from its source field",
        );

        let non_reference_owner = ObjectTypeDefinition::new(
            task.id(),
            task.name().clone(),
            vec![
                FieldDefinition::new(
                    owner.id(),
                    owner.name(),
                    owner.ordinal(),
                    ResolvedType::scalar(StandardScalar::Boolean),
                    owner.nullable(),
                    owner.unique(),
                    owner.default_expression(),
                    owner.on_delete(),
                ),
                completed.clone(),
            ],
        );
        assert_preparation_reason(
            distinct_query_plan(
                &plan,
                &function,
                &[person.clone(), non_reference_owner],
                &references,
            ),
            "SELECT DISTINCT query field path continues through a non-reference field",
        );

        let missing_target_owner = ObjectTypeDefinition::new(
            task.id(),
            task.name().clone(),
            vec![
                FieldDefinition::new(
                    owner.id(),
                    owner.name(),
                    owner.ordinal(),
                    ResolvedType::reference(TypeId::new()),
                    owner.nullable(),
                    owner.unique(),
                    owner.default_expression(),
                    owner.on_delete(),
                ),
                completed.clone(),
            ],
        );
        assert_preparation_reason(
            distinct_query_plan(
                &plan,
                &function,
                &[person.clone(), missing_target_owner],
                &references,
            ),
            "SELECT DISTINCT query field path target is absent from the candidate catalogue",
        );

        let wrong_return = FunctionDefinition::new(
            function.id(),
            function.name().clone(),
            FunctionDomain::Server,
            Vec::new(),
            FunctionReturn::Rows(vec![
                FunctionReturnColumnDefinition::new("task", 0, ResolvedType::reference(task.id())),
                FunctionReturnColumnDefinition::new(
                    "active",
                    1,
                    ResolvedType::scalar(StandardScalar::Boolean),
                ),
                FunctionReturnColumnDefinition::new(
                    "completed",
                    2,
                    ResolvedType::scalar(StandardScalar::Integer),
                ),
            ]),
            function.current_revision(),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert_preparation_reason(
            distinct_query_plan(
                &plan,
                &wrong_return,
                &object_types,
                &distinct_query_reference_sequence(&plan, &wrong_return),
            ),
            "SELECT DISTINCT query projection differs from its function return",
        );

        for invalid_references in [
            references[..references.len() - 1].to_vec(),
            {
                let mut extra = references.clone();
                extra.push(references[0]);
                extra
            },
            {
                let mut reordered = references.clone();
                reordered.reverse();
                reordered
            },
            {
                let mut wrong_kind = references.clone();
                wrong_kind[1].0 = DefinitionReferenceKind::QueryField;
                wrong_kind
            },
            {
                let mut wrong_target = references.clone();
                wrong_target[1].1 = DefinitionReferenceTarget::ObjectType(TypeId::new());
                wrong_target
            },
        ] {
            assert_preparation_reason(
                distinct_query_plan(&plan, &function, &object_types, &invalid_references),
                "SELECT DISTINCT definition references differ from the checked function body",
            );
        }
    }

    #[test]
    fn distinct_preparation_has_an_exhaustive_projection_domain_and_boolean_selection() {
        let (plan, function, object_types, _) = mapped_distinct_fixture();
        let person = object_types
            .iter()
            .find(|object_type| object_type.name() == &semantic_name(&["tasks", "person"]))
            .unwrap();

        for scalar in StandardScalar::ALL {
            let semantic_type = SemanticType::scalar(scalar);
            let malformed = plan
                .with_test_mutation(crate::relational::DistinctQueryTestMutation::ClearSelection)
                .with_test_mutation(
                    crate::relational::DistinctQueryTestMutation::ProjectionType {
                        semantic_type,
                        nullable: false,
                    },
                );
            let candidate = object_types_with_distinct_completed_type(
                &object_types,
                ResolvedType::scalar(scalar),
            );
            let function =
                distinct_function_with_completed_type(&function, ResolvedType::scalar(scalar));
            let references = distinct_query_reference_sequence(&malformed, &function);
            let accepted = matches!(
                scalar,
                StandardScalar::Boolean
                    | StandardScalar::Integer
                    | StandardScalar::BigInt
                    | StandardScalar::BinaryLargeObject
            );
            let result = distinct_query_plan(&malformed, &function, &candidate, &references);
            if accepted {
                assert!(result.is_ok(), "{scalar:?} must be accepted: {result:?}");
            } else {
                assert_preparation_reason(
                    result,
                    "SELECT DISTINCT query projection has an unsupported type",
                );
            }
        }

        let reference = SemanticType::reference(person.id());
        let reference_plan = plan
            .with_test_mutation(crate::relational::DistinctQueryTestMutation::ClearSelection)
            .with_test_mutation(
                crate::relational::DistinctQueryTestMutation::ProjectionType {
                    semantic_type: reference,
                    nullable: false,
                },
            );
        let reference_function =
            distinct_function_with_completed_type(&function, ResolvedType::reference(person.id()));
        assert!(
            distinct_query_plan(
                &reference_plan,
                &reference_function,
                &object_types_with_distinct_completed_type(
                    &object_types,
                    ResolvedType::reference(person.id()),
                ),
                &distinct_query_reference_sequence(&reference_plan, &reference_function),
            )
            .is_ok()
        );

        let named_plan = plan
            .with_test_mutation(crate::relational::DistinctQueryTestMutation::ClearSelection)
            .with_test_mutation(
                crate::relational::DistinctQueryTestMutation::ProjectionType {
                    semantic_type: SemanticType::Named(person.id()),
                    nullable: false,
                },
            );
        let named_function =
            distinct_function_with_completed_type(&function, ResolvedType::Named(person.id()));
        assert_preparation_reason(
            distinct_query_plan(
                &named_plan,
                &named_function,
                &object_types_with_distinct_completed_type(
                    &object_types,
                    ResolvedType::Named(person.id()),
                ),
                &distinct_query_reference_sequence(&named_plan, &named_function),
            ),
            "SELECT DISTINCT query projection has an unsupported type",
        );

        let non_boolean_selection = plan.with_test_mutation(
            crate::relational::DistinctQueryTestMutation::SelectionObjectReference,
        );
        assert_preparation_reason(
            distinct_query_plan(
                &non_boolean_selection,
                &function,
                &object_types,
                &distinct_query_reference_sequence(&non_boolean_selection, &function),
            ),
            "SELECT DISTINCT query selection is not BOOLEAN",
        );
    }

    #[test]
    fn distinct_preparation_requires_the_final_projected_reference_target() {
        let (plan, function, object_types, references) =
            mapped_distinct_fixture_for(DISTINCT_REFERENCE_SOURCE);
        let task = object_types
            .iter()
            .find(|object_type| object_type.name() == &semantic_name(&["tasks", "task"]))
            .unwrap()
            .clone();

        assert_preparation_reason(
            distinct_query_plan(&plan, &function, &[task], &references),
            "SELECT DISTINCT query field path target is absent from the candidate catalogue",
        );
    }

    #[test]
    fn identity_selected_query_replay_reuses_and_selector_rename_revises() {
        let empty = empty_active();
        let initial = prepare(
            &checked_report(IDENTITY_SELECTED_SOURCE, empty.catalogue()),
            empty.pair(),
            &empty,
        )
        .unwrap();
        let initial_revision = initial.new_function_revisions()[0].clone();
        let initial_parameter = initial.candidate().functions()[0].parameters()[0].id();
        let active = activate(&initial, vec![initial_revision.clone()], Vec::new());

        let replay = prepare(
            &checked_report(IDENTITY_SELECTED_SOURCE, active.catalogue()),
            active.pair(),
            &active,
        )
        .unwrap();
        assert!(replay.new_function_revisions().is_empty());
        assert_eq!(
            replay.candidate().functions()[0].current_revision(),
            initial_revision.id()
        );
        assert_eq!(
            replay.candidate().functions()[0].parameters()[0].id(),
            initial_parameter
        );

        let renamed = prepare(
            &checked_report(
                IDENTITY_SELECTED_RENAMED_SELECTOR_SOURCE,
                active.catalogue(),
            ),
            active.pair(),
            &active,
        )
        .unwrap();
        let changed = &renamed.new_function_revisions()[0];
        assert_eq!(
            changed.revision_number(),
            initial_revision.revision_number() + 1
        );
        assert_ne!(changed.id(), initial_revision.id());
        assert_ne!(changed.semantic_hash(), initial_revision.semantic_hash());
        assert_ne!(
            changed.artifact().payload(),
            initial_revision.artifact().payload()
        );
        assert_ne!(
            renamed.candidate().functions()[0].parameters()[0].id(),
            initial_parameter
        );
        assert_eq!(changed.artifact().version(), 2);
    }

    #[test]
    fn prepares_nullable_multi_hop_equality_projection_with_complete_evidence() {
        let active = empty_active();
        let prepared = prepare(
            &checked_report(
                IDENTITY_SELECTED_NULLABLE_EQUALITY_SOURCE,
                active.catalogue(),
            ),
            active.pair(),
            &active,
        )
        .unwrap();
        let task = prepared
            .candidate()
            .object_type_by_name(&semantic_name(&["tasks", "task"]))
            .unwrap();
        let person = prepared
            .candidate()
            .object_type_by_name(&semantic_name(&["tasks", "person"]))
            .unwrap();
        assert_eq!(
            prepared
                .references()
                .iter()
                .map(|reference| (reference.kind(), reference.target()))
                .collect::<Vec<_>>(),
            vec![
                (
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(task.id())
                ),
                (
                    DefinitionReferenceKind::QueryObject,
                    DefinitionReferenceTarget::ObjectType(task.id())
                ),
                (
                    DefinitionReferenceKind::QueryField,
                    DefinitionReferenceTarget::Field {
                        owner: task.id(),
                        field: task.field_by_name("owner").unwrap().id()
                    }
                ),
                (
                    DefinitionReferenceKind::QueryField,
                    DefinitionReferenceTarget::Field {
                        owner: person.id(),
                        field: person.field_by_name("name").unwrap().id()
                    }
                ),
                (
                    DefinitionReferenceKind::QueryField,
                    DefinitionReferenceTarget::Field {
                        owner: task.id(),
                        field: task.field_by_name("owner").unwrap().id()
                    }
                ),
                (
                    DefinitionReferenceKind::QueryField,
                    DefinitionReferenceTarget::Field {
                        owner: task.id(),
                        field: task.field_by_name("owner").unwrap().id()
                    }
                ),
                (
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(task.id())
                ),
                (
                    DefinitionReferenceKind::ParameterRead,
                    DefinitionReferenceTarget::Parameter {
                        owner: prepared.candidate().functions()[0].id(),
                        parameter: prepared.candidate().functions()[0].parameters()[0].id()
                    }
                ),
            ]
        );
    }

    #[test]
    fn identity_selected_validator_rejects_private_plan_and_evidence_mismatches() {
        let active = empty_active();
        let prepared = prepare(
            &checked_report(IDENTITY_SELECTED_SOURCE, active.catalogue()),
            active.pair(),
            &active,
        )
        .unwrap();
        let task = prepared.candidate().object_types()[0].clone();
        let function = prepared.candidate().functions()[0].clone();
        let checked = checked_report(IDENTITY_SELECTED_SOURCE, active.catalogue());
        let checked_function = &checked.checked_bundle().unwrap().server_functions()[0];
        let map = |owner, parameter, scan, field| {
            checked_function
                .identity_selected_query_plan()
                .unwrap()
                .try_map_identities(
                    |_| Ok::<_, PrepareError>(scan),
                    |_| Ok::<_, PrepareError>(field),
                    |_| Ok::<_, PrepareError>(owner),
                    |_| Ok::<_, PrepareError>(parameter),
                )
                .unwrap()
        };
        let plan = map(
            function.id(),
            function.parameters()[0].id(),
            task.id(),
            task.fields()[0].id(),
        );
        let references = identity_selected_query_reference_sequence(&plan, &function);
        let expect = |result: Result<_, PrepareError>, reason| {
            assert!(
                matches!(result, Err(PrepareError::InvalidCheckedBundle { reason: actual }) if actual == reason)
            );
        };
        expect(
            identity_selected_query_plan(
                &map(
                    function.id(),
                    function.parameters()[0].id(),
                    TypeId::new(),
                    task.fields()[0].id(),
                ),
                &function,
                std::slice::from_ref(&task),
                &references,
            ),
            "identity-selected query scan object is absent from the candidate catalogue",
        );
        let wrong_mode = FunctionDefinition::new(
            function.id(),
            function.name().clone(),
            FunctionDomain::Server,
            function.parameters().to_vec(),
            function.return_type().clone(),
            function.current_revision(),
            FunctionSecurity::Definer,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        expect(
            identity_selected_query_plan(
                &plan,
                &wrong_mode,
                std::slice::from_ref(&task),
                &references,
            ),
            "identity-selected query function has unsupported execution modes",
        );
        let wrong_selector_type = FunctionDefinition::new(
            function.id(),
            function.name().clone(),
            FunctionDomain::Server,
            vec![ParameterDefinition::new(
                function.parameters()[0].id(),
                function.parameters()[0].name(),
                0,
                ResolvedType::reference(TypeId::new()),
                None,
            )],
            function.return_type().clone(),
            function.current_revision(),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        expect(
            identity_selected_query_plan(
                &plan,
                &wrong_selector_type,
                std::slice::from_ref(&task),
                &references,
            ),
            "identity-selected query selector parameter does not reference its scan object",
        );
        let non_rows = FunctionDefinition::new(
            function.id(),
            function.name().clone(),
            FunctionDomain::Server,
            function.parameters().to_vec(),
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Boolean)),
            function.current_revision(),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        expect(
            identity_selected_query_plan(
                &plan,
                &non_rows,
                std::slice::from_ref(&task),
                &references,
            ),
            "identity-selected query function does not return ROWS",
        );
        let wrong_count = FunctionDefinition::new(
            function.id(),
            function.name().clone(),
            FunctionDomain::Server,
            function.parameters().to_vec(),
            FunctionReturn::Rows(Vec::new()),
            function.current_revision(),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        expect(
            identity_selected_query_plan(
                &plan,
                &wrong_count,
                std::slice::from_ref(&task),
                &references,
            ),
            "identity-selected query projection count differs from its function return",
        );
        let wrong_return_type = FunctionDefinition::new(
            function.id(),
            function.name().clone(),
            FunctionDomain::Server,
            function.parameters().to_vec(),
            FunctionReturn::Rows(vec![
                FunctionReturnColumnDefinition::new("task", 0, ResolvedType::reference(task.id())),
                FunctionReturnColumnDefinition::new(
                    "title",
                    1,
                    ResolvedType::scalar(StandardScalar::Boolean),
                ),
            ]),
            function.current_revision(),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        expect(
            identity_selected_query_plan(
                &plan,
                &wrong_return_type,
                std::slice::from_ref(&task),
                &identity_selected_query_reference_sequence(&plan, &wrong_return_type),
            ),
            "identity-selected query projection differs from its function return",
        );
        let no_parameters = FunctionDefinition::new(
            function.id(),
            function.name().clone(),
            FunctionDomain::Server,
            Vec::new(),
            function.return_type().clone(),
            function.current_revision(),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        expect(
            identity_selected_query_plan(
                &plan,
                &no_parameters,
                std::slice::from_ref(&task),
                &references,
            ),
            "identity-selected query function does not declare exactly one parameter",
        );
        let two_parameters = FunctionDefinition::new(
            function.id(),
            function.name().clone(),
            FunctionDomain::Server,
            vec![
                function.parameters()[0].clone(),
                function.parameters()[0].clone(),
            ],
            function.return_type().clone(),
            function.current_revision(),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        expect(
            identity_selected_query_plan(
                &plan,
                &two_parameters,
                std::slice::from_ref(&task),
                &references,
            ),
            "identity-selected query function does not declare exactly one parameter",
        );
        let default_parameter = ParameterDefinition::new(
            function.parameters()[0].id(),
            function.parameters()[0].name(),
            0,
            function.parameters()[0].resolved_type(),
            Some(ExpressionId::new()),
        );
        let with_default = FunctionDefinition::new(
            function.id(),
            function.name().clone(),
            FunctionDomain::Server,
            vec![default_parameter],
            function.return_type().clone(),
            function.current_revision(),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        expect(
            identity_selected_query_plan(
                &plan,
                &with_default,
                std::slice::from_ref(&task),
                &references,
            ),
            "identity-selected query selector parameter has an unsupported default expression",
        );
        expect(
            identity_selected_query_plan(
                &map(
                    FunctionId::new(),
                    function.parameters()[0].id(),
                    task.id(),
                    task.fields()[0].id(),
                ),
                &function,
                std::slice::from_ref(&task),
                &references,
            ),
            "identity-selected query selector owner differs from its enclosing function",
        );
        expect(
            identity_selected_query_plan(
                &map(
                    function.id(),
                    ParameterId::new(),
                    task.id(),
                    task.fields()[0].id(),
                ),
                &function,
                std::slice::from_ref(&task),
                &references,
            ),
            "identity-selected query selector parameter is not its enclosing function parameter",
        );
        expect(
            identity_selected_query_plan(
                &map(
                    function.id(),
                    function.parameters()[0].id(),
                    task.id(),
                    FieldId::new(),
                ),
                &function,
                std::slice::from_ref(&task),
                &references,
            ),
            "identity-selected query field path field is absent from its source object",
        );
        let wrong_final_type = ObjectTypeDefinition::new(
            task.id(),
            task.name().clone(),
            vec![FieldDefinition::new(
                task.fields()[0].id(),
                task.fields()[0].name(),
                0,
                ResolvedType::scalar(StandardScalar::Boolean),
                false,
                false,
                None,
                None,
            )],
        );
        expect(
            identity_selected_query_plan(
                &plan,
                &function,
                std::slice::from_ref(&wrong_final_type),
                &references,
            ),
            "identity-selected query field path type differs from its source field",
        );
        let nullable_final = ObjectTypeDefinition::new(
            task.id(),
            task.name().clone(),
            vec![FieldDefinition::new(
                task.fields()[0].id(),
                task.fields()[0].name(),
                0,
                task.fields()[0].resolved_type(),
                true,
                false,
                None,
                None,
            )],
        );
        expect(
            identity_selected_query_plan(
                &plan,
                &function,
                std::slice::from_ref(&nullable_final),
                &references,
            ),
            "identity-selected query field path type differs from its source field",
        );
        let mut wrong_evidence = references.clone();
        wrong_evidence.reverse();
        expect(
            identity_selected_query_plan(
                &plan,
                &function,
                std::slice::from_ref(&task),
                &wrong_evidence,
            ),
            "parameterised SELECT definition references differ from the checked function body",
        );
        expect(
            identity_selected_query_plan(
                &plan,
                &function,
                std::slice::from_ref(&task),
                &references[..references.len() - 1],
            ),
            "parameterised SELECT definition references differ from the checked function body",
        );
        let mut extra_evidence = references.clone();
        extra_evidence.push(references[0]);
        expect(
            identity_selected_query_plan(
                &plan,
                &function,
                std::slice::from_ref(&task),
                &extra_evidence,
            ),
            "parameterised SELECT definition references differ from the checked function body",
        );
        let mut wrong_target_evidence = references.clone();
        wrong_target_evidence[0].1 = DefinitionReferenceTarget::ObjectType(TypeId::new());
        expect(
            identity_selected_query_plan(
                &plan,
                &function,
                std::slice::from_ref(&task),
                &wrong_target_evidence,
            ),
            "parameterised SELECT definition references differ from the checked function body",
        );
        let checked_plan = checked_function.identity_selected_query_plan().unwrap();
        assert_eq!(
            checked_plan
                .try_map_identities(
                    |_| Err::<TypeId, _>("type identity"),
                    |_| Ok::<_, &'static str>(task.fields()[0].id()),
                    |_| Ok::<_, &'static str>(function.id()),
                    |_| Ok::<_, &'static str>(function.parameters()[0].id()),
                )
                .unwrap_err(),
            "type identity"
        );
        assert_eq!(
            checked_plan
                .try_map_identities(
                    |_| Ok::<_, &'static str>(task.id()),
                    |_| Err::<FieldId, _>("field identity"),
                    |_| Ok::<_, &'static str>(function.id()),
                    |_| Ok::<_, &'static str>(function.parameters()[0].id()),
                )
                .unwrap_err(),
            "field identity"
        );
        assert_eq!(
            checked_plan
                .try_map_identities(
                    |_| Ok::<_, &'static str>(task.id()),
                    |_| Ok::<_, &'static str>(task.fields()[0].id()),
                    |_| Err::<FunctionId, _>("function identity"),
                    |_| Ok::<_, &'static str>(function.parameters()[0].id()),
                )
                .unwrap_err(),
            "function identity"
        );
        assert_eq!(
            checked_plan
                .try_map_identities(
                    |_| Ok::<_, &'static str>(task.id()),
                    |_| Ok::<_, &'static str>(task.fields()[0].id()),
                    |_| Ok::<_, &'static str>(function.id()),
                    |_| Err::<ParameterId, _>("parameter identity"),
                )
                .unwrap_err(),
            "parameter identity"
        );
    }

    #[test]
    fn identity_selected_validator_rejects_multi_hop_catalogue_mismatches() {
        let active = empty_active();
        let prepared = prepare(
            &checked_report(
                IDENTITY_SELECTED_NULLABLE_EQUALITY_SOURCE,
                active.catalogue(),
            ),
            active.pair(),
            &active,
        )
        .unwrap();
        let task = prepared
            .candidate()
            .object_type_by_name(&semantic_name(&["tasks", "task"]))
            .unwrap()
            .clone();
        let person = prepared
            .candidate()
            .object_type_by_name(&semantic_name(&["tasks", "person"]))
            .unwrap()
            .clone();
        let function = prepared.candidate().functions()[0].clone();
        let checked = checked_report(
            IDENTITY_SELECTED_NULLABLE_EQUALITY_SOURCE,
            active.catalogue(),
        );
        let checked_plan = checked.checked_bundle().unwrap().server_functions()[0]
            .identity_selected_query_plan()
            .unwrap();
        let owner_field = task.field_by_name("owner").unwrap();
        let name_field = person.field_by_name("name").unwrap();
        let map_plan = |type_ids: [TypeId; 7]| {
            let mut type_index = 0;
            let mut field_index = 0;
            let field_ids = [
                owner_field.id(),
                name_field.id(),
                owner_field.id(),
                name_field.id(),
            ];
            let plan = checked_plan
                .try_map_identities(
                    |_| {
                        let mapped = type_ids[type_index];
                        type_index += 1;
                        Ok::<_, PrepareError>(mapped)
                    },
                    |_| {
                        let mapped = field_ids[field_index];
                        field_index += 1;
                        Ok::<_, PrepareError>(mapped)
                    },
                    |_| Ok::<_, PrepareError>(function.id()),
                    |_| Ok::<_, PrepareError>(function.parameters()[0].id()),
                )
                .unwrap();
            assert_eq!(type_index, type_ids.len());
            assert_eq!(field_index, field_ids.len());
            plan
        };
        let exact_types = [
            task.id(),
            task.id(),
            person.id(),
            task.id(),
            person.id(),
            task.id(),
            person.id(),
        ];
        let plan = map_plan(exact_types);
        let references = identity_selected_query_reference_sequence(&plan, &function);
        let non_reference_task = ObjectTypeDefinition::new(
            task.id(),
            task.name().clone(),
            vec![FieldDefinition::new(
                owner_field.id(),
                owner_field.name(),
                owner_field.ordinal(),
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                owner_field.nullable(),
                false,
                None,
                None,
            )],
        );
        assert!(matches!(
            identity_selected_query_plan(
                &plan,
                &function,
                &[non_reference_task, person.clone()],
                &references,
            ),
            Err(PrepareError::InvalidCheckedBundle {
                reason: "identity-selected query field path continues through a non-reference field"
            })
        ));

        let wrong_owner_plan = map_plan([
            task.id(),
            person.id(),
            person.id(),
            task.id(),
            person.id(),
            task.id(),
            person.id(),
        ]);
        assert!(matches!(
            identity_selected_query_plan(
                &wrong_owner_plan,
                &function,
                &[task, person],
                &identity_selected_query_reference_sequence(&wrong_owner_plan, &function),
            ),
            Err(PrepareError::InvalidCheckedBundle {
                reason: "identity-selected query field path owner differs from its source object"
            })
        ));
    }

    #[test]
    fn prepares_a_complete_server_mutation_artifact_and_reuses_only_equal_semantics() {
        let empty = empty_active();
        let initial = prepare(
            &checked_report(MUTATION_SOURCE, empty.catalogue()),
            empty.pair(),
            &empty,
        )
        .unwrap();

        let catalogue = initial.candidate();
        let task = catalogue
            .object_type_by_name(&semantic_name(&["tasks", "task"]))
            .unwrap();
        let person = catalogue
            .object_type_by_name(&semantic_name(&["tasks", "person"]))
            .unwrap();
        let function = &catalogue.functions()[0];
        let revision = &initial.new_function_revisions()[0];
        assert_eq!(revision.artifact().kind(), ExecutableArtifactKind::Server);
        assert_eq!(revision.artifact().format(), SERVER_MUTATION_PLAN_FORMAT);
        assert_eq!(
            revision.artifact().version(),
            orna_artifact::server_mutation_plan::INSERT_FORMAT_VERSION
        );
        assert_eq!(
            revision.language_version(),
            SERVER_MUTATION_PLAN_LANGUAGE_VERSION
        );
        assert_eq!(
            artifact_payload_digest(revision.artifact().payload()).unwrap(),
            revision.artifact().content_hash()
        );

        let plan = ServerMutationPlan::decode(revision.artifact().payload()).unwrap();
        assert_eq!(plan.target(), task.id());
        assert_eq!(plan.returned_object(), task.id());
        assert_eq!(plan.assignments().len(), 4);
        assert_eq!(plan.assignments()[0].owner(), task.id());
        assert_eq!(plan.assignments()[0].field(), task.fields()[0].id());
        assert_eq!(plan.assignments()[1].field(), task.fields()[1].id());
        assert_eq!(plan.assignments()[2].field(), task.fields()[2].id());
        assert_eq!(plan.assignments()[3].field(), task.fields()[3].id());
        assert!(
            plan.assignments()
                .iter()
                .all(|assignment| assignment.owner() == task.id())
        );
        assert_eq!(
            plan.assignments()[0].expression().resolved_type(),
            ResolvedType::scalar(StandardScalar::CharacterLargeObject)
        );
        assert!(!plan.assignments()[0].expression().nullable());
        assert_eq!(
            plan.assignments()[1].expression().resolved_type(),
            ResolvedType::scalar(StandardScalar::Boolean)
        );
        assert!(!plan.assignments()[1].expression().nullable());
        assert_eq!(
            plan.assignments()[2].expression().resolved_type(),
            ResolvedType::scalar(StandardScalar::CharacterLargeObject)
        );
        assert_eq!(
            plan.assignments()[3].expression().resolved_type(),
            ResolvedType::reference(person.id())
        );
        assert!(!plan.assignments()[3].expression().nullable());
        assert!(matches!(
            plan.assignments()[0].expression().kind(),
            DurableMutationExpressionKind::Parameter { owner, parameter }
                if *owner == function.id() && *parameter == function.parameters()[0].id()
        ));
        assert!(matches!(
            plan.assignments()[1].expression().kind(),
            DurableMutationExpressionKind::BooleanLiteral { value: false }
        ));
        assert!(plan.assignments()[2].expression().nullable());
        assert!(matches!(
            plan.assignments()[2].expression().kind(),
            DurableMutationExpressionKind::TypedNull
        ));
        assert!(matches!(
            plan.assignments()[3].expression().kind(),
            DurableMutationExpressionKind::Parameter { owner, parameter }
                if *owner == function.id() && *parameter == function.parameters()[2].id()
        ));
        assert_eq!(
            initial
                .references()
                .iter()
                .map(|reference| (reference.kind(), reference.target()))
                .collect::<Vec<_>>(),
            vec![
                (
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(person.id())
                ),
                (
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(task.id())
                ),
                (
                    DefinitionReferenceKind::WriteObject,
                    DefinitionReferenceTarget::ObjectType(task.id())
                ),
                (
                    DefinitionReferenceKind::WriteField,
                    DefinitionReferenceTarget::Field {
                        owner: task.id(),
                        field: task.fields()[0].id()
                    }
                ),
                (
                    DefinitionReferenceKind::ParameterRead,
                    DefinitionReferenceTarget::Parameter {
                        owner: function.id(),
                        parameter: function.parameters()[0].id()
                    }
                ),
                (
                    DefinitionReferenceKind::WriteField,
                    DefinitionReferenceTarget::Field {
                        owner: task.id(),
                        field: task.fields()[1].id()
                    }
                ),
                (
                    DefinitionReferenceKind::WriteField,
                    DefinitionReferenceTarget::Field {
                        owner: task.id(),
                        field: task.fields()[2].id()
                    }
                ),
                (
                    DefinitionReferenceKind::WriteField,
                    DefinitionReferenceTarget::Field {
                        owner: task.id(),
                        field: task.fields()[3].id()
                    }
                ),
                (
                    DefinitionReferenceKind::ParameterRead,
                    DefinitionReferenceTarget::Parameter {
                        owner: function.id(),
                        parameter: function.parameters()[2].id()
                    }
                ),
                (
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(task.id())
                ),
            ]
        );
        assert_eq!(
            initial.references()[2].source_origin().byte_start() as usize,
            MUTATION_SOURCE.rfind("tasks.task AS created").unwrap()
        );
        assert_eq!(
            initial
                .references()
                .iter()
                .map(|reference| reference.ordinal())
                .collect::<Vec<_>>(),
            (0..10).collect::<Vec<_>>()
        );
        assert!(initial.references().iter().all(|reference| {
            reference.source_function() == function.id()
                && reference.source_revision() == revision.id()
        }));

        let active = activate(&initial, vec![revision.clone()], Vec::new());
        let reformatted = prepare(
            &checked_report(MUTATION_REFORMATTED_SOURCE, active.catalogue()),
            active.pair(),
            &active,
        )
        .unwrap();
        assert!(reformatted.new_function_revisions().is_empty());
        assert_eq!(
            reformatted.candidate().functions()[0].current_revision(),
            revision.id()
        );

        let changed = prepare(
            &checked_report(MUTATION_CHANGED_SOURCE, active.catalogue()),
            active.pair(),
            &active,
        )
        .unwrap();
        assert_eq!(changed.new_function_revisions().len(), 1);
        assert_ne!(changed.new_function_revisions()[0].id(), revision.id());
        assert_ne!(
            changed.new_function_revisions()[0].semantic_hash(),
            revision.semantic_hash()
        );
    }

    #[test]
    fn prepares_update_version_two_with_selector_and_exact_references() {
        let empty = empty_active();
        let prepared = prepare(
            &checked_report(UPDATE_SOURCE, empty.catalogue()),
            empty.pair(),
            &empty,
        )
        .unwrap();

        let catalogue = prepared.candidate();
        let person = catalogue
            .object_type_by_name(&semantic_name(&["tasks", "person"]))
            .unwrap();
        let task = catalogue
            .object_type_by_name(&semantic_name(&["tasks", "task"]))
            .unwrap();
        let function = &catalogue.functions()[0];
        let revision = &prepared.new_function_revisions()[0];
        assert_eq!(
            revision.artifact().version(),
            orna_artifact::server_mutation_plan::UPDATE_FORMAT_VERSION
        );
        assert_eq!(
            artifact_payload_digest(revision.artifact().payload()).unwrap(),
            revision.artifact().content_hash()
        );
        let plan = ServerMutationPlan::decode(revision.artifact().payload()).unwrap();
        assert_eq!(plan.format_version(), 2);
        assert_eq!(plan.target(), task.id());
        assert_eq!(plan.returned_object(), task.id());
        assert_eq!(plan.assignments().len(), 2);
        assert_eq!(plan.assignments()[0].field(), task.fields()[0].id());
        assert_eq!(plan.assignments()[1].field(), task.fields()[2].id());
        assert_eq!(
            plan.operation(),
            &ServerMutationOperation::Update {
                selector: orna_artifact::server_mutation_plan::MutationSelector::new(
                    function.id(),
                    function.parameters()[0].id(),
                )
            }
        );
        assert_eq!(
            prepared
                .references()
                .iter()
                .map(|reference| (reference.kind(), reference.target()))
                .collect::<Vec<_>>(),
            vec![
                (
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(task.id()),
                ),
                (
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(person.id()),
                ),
                (
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(task.id()),
                ),
                (
                    DefinitionReferenceKind::WriteObject,
                    DefinitionReferenceTarget::ObjectType(task.id()),
                ),
                (
                    DefinitionReferenceKind::WriteField,
                    DefinitionReferenceTarget::Field {
                        owner: task.id(),
                        field: task.fields()[0].id(),
                    },
                ),
                (
                    DefinitionReferenceKind::ParameterRead,
                    DefinitionReferenceTarget::Parameter {
                        owner: function.id(),
                        parameter: function.parameters()[1].id(),
                    },
                ),
                (
                    DefinitionReferenceKind::WriteField,
                    DefinitionReferenceTarget::Field {
                        owner: task.id(),
                        field: task.fields()[2].id(),
                    },
                ),
                (
                    DefinitionReferenceKind::ParameterRead,
                    DefinitionReferenceTarget::Parameter {
                        owner: function.id(),
                        parameter: function.parameters()[2].id(),
                    },
                ),
                (
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(task.id()),
                ),
                (
                    DefinitionReferenceKind::ParameterRead,
                    DefinitionReferenceTarget::Parameter {
                        owner: function.id(),
                        parameter: function.parameters()[0].id(),
                    },
                ),
                (
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(task.id()),
                ),
            ]
        );
        assert_eq!(
            prepared
                .references()
                .iter()
                .map(|reference| reference.ordinal())
                .collect::<Vec<_>>(),
            (0..11).collect::<Vec<_>>()
        );
    }

    #[test]
    fn prepares_delete_version_three_with_boolean_result_and_exact_references() {
        let empty = empty_active();
        let prepared = prepare(
            &checked_report(DELETE_SOURCE, empty.catalogue()),
            empty.pair(),
            &empty,
        )
        .unwrap();

        let catalogue = prepared.candidate();
        let target = catalogue
            .object_type_by_name(&semantic_name(&["tasks", "task"]))
            .unwrap();
        let function = &catalogue.functions()[0];
        let revision = &prepared.new_function_revisions()[0];
        assert_eq!(revision.artifact().kind(), ExecutableArtifactKind::Server);
        assert_eq!(revision.artifact().format(), SERVER_MUTATION_PLAN_FORMAT);
        assert_eq!(
            revision.artifact().version(),
            orna_artifact::server_mutation_plan::DELETE_FORMAT_VERSION
        );
        assert_eq!(
            artifact_payload_digest(revision.artifact().payload()).unwrap(),
            revision.artifact().content_hash()
        );
        assert_eq!(
            revision.language_version(),
            SERVER_MUTATION_PLAN_LANGUAGE_VERSION
        );
        let plan = ServerDeletePlan::decode(revision.artifact().payload()).unwrap();
        assert_eq!(plan.target(), target.id());
        assert_eq!(plan.selector().owner(), function.id());
        assert_eq!(plan.selector().parameter(), function.parameters()[0].id());
        assert!(matches!(
            function.return_type(),
            FunctionReturn::Rows(columns)
                if columns.len() == 1
                    && columns[0].resolved_type()
                        == ResolvedType::Scalar(StandardScalar::Boolean)
        ));
        assert_eq!(
            prepared
                .references()
                .iter()
                .map(|reference| (reference.kind(), reference.target()))
                .collect::<Vec<_>>(),
            vec![
                (
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(target.id()),
                ),
                (
                    DefinitionReferenceKind::WriteObject,
                    DefinitionReferenceTarget::ObjectType(target.id()),
                ),
                (
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(target.id()),
                ),
                (
                    DefinitionReferenceKind::ParameterRead,
                    DefinitionReferenceTarget::Parameter {
                        owner: function.id(),
                        parameter: function.parameters()[0].id(),
                    },
                ),
            ]
        );
        assert_eq!(
            prepared
                .references()
                .iter()
                .map(|reference| {
                    (
                        reference.ordinal(),
                        reference.source_origin().byte_start() as usize,
                        reference.source_origin().byte_end() as usize,
                    )
                })
                .collect::<Vec<_>>(),
            [
                (0, "p_task REF ", "tasks.task"),
                (1, "DELETE FROM ", "tasks.task"),
                (2, "WHERE REF(", "removed"),
                (3, "= ", "p_task"),
            ]
            .into_iter()
            .zip([
                "p_task REF tasks.task",
                "DELETE FROM tasks.task",
                "WHERE REF(removed)",
                "= p_task RETURNING",
            ])
            .map(|((ordinal, prefix, token), context)| {
                let start = DELETE_SOURCE.find(context).unwrap() + prefix.len();
                (ordinal, start, start + token.len())
            })
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn mutation_preparation_revalidates_durable_catalogue_and_reference_facts() {
        let target_id = TypeId::from_bytes([41; 16]);
        let title_id = FieldId::from_bytes([42; 16]);
        let note_id = FieldId::from_bytes([43; 16]);
        let function_id = FunctionId::from_bytes([44; 16]);
        let parameter_id = ParameterId::from_bytes([45; 16]);
        let text = ResolvedType::scalar(StandardScalar::CharacterLargeObject);
        let target = ObjectTypeDefinition::new(
            target_id,
            semantic_name(&["tasks", "task"]),
            vec![
                FieldDefinition::new(title_id, "title", 0, text, false, false, None, None),
                FieldDefinition::new(note_id, "note", 1, text, true, false, None, None),
            ],
        );
        let function = FunctionDefinition::new(
            function_id,
            semantic_name(&["tasks", "create"]),
            FunctionDomain::Server,
            vec![ParameterDefinition::new(
                parameter_id,
                "title",
                0,
                text,
                None,
            )],
            FunctionReturn::Rows(Vec::new()),
            FunctionRevisionId::from_bytes([46; 16]),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Volatile,
        );
        let parameter = MutationAssignment::new(
            target_id,
            title_id,
            MutationExpression::new(
                MutationExpressionKind::ParameterRead {
                    owner: function_id,
                    parameter: parameter_id,
                },
                MutationValueType::new(
                    SemanticType::scalar(StandardScalar::CharacterLargeObject),
                    false,
                ),
            ),
        );
        assert!(
            validate_mutation_assignments(
                std::slice::from_ref(&parameter),
                &target,
                &function,
                true,
            )
            .is_ok()
        );

        let cross_owner = MutationAssignment::new(
            TypeId::from_bytes([47; 16]),
            title_id,
            parameter.expression().clone(),
        );
        let unknown_field = MutationAssignment::new(
            target_id,
            FieldId::from_bytes([48; 16]),
            parameter.expression().clone(),
        );
        let wrong_field_type = MutationAssignment::new(
            target_id,
            title_id,
            MutationExpression::new(
                MutationExpressionKind::BooleanLiteral { value: true },
                MutationValueType::new(SemanticType::scalar(StandardScalar::Boolean), false),
            ),
        );
        let wrong_parameter_type = MutationAssignment::new(
            target_id,
            title_id,
            MutationExpression::new(
                MutationExpressionKind::ParameterRead {
                    owner: function_id,
                    parameter: parameter_id,
                },
                MutationValueType::new(
                    SemanticType::scalar(StandardScalar::CharacterLargeObject),
                    false,
                ),
            ),
        );
        let function_with_wrong_parameter_type = FunctionDefinition::new(
            function_id,
            semantic_name(&["tasks", "create"]),
            FunctionDomain::Server,
            vec![ParameterDefinition::new(
                parameter_id,
                "title",
                0,
                ResolvedType::scalar(StandardScalar::Integer),
                None,
            )],
            FunctionReturn::Rows(Vec::new()),
            FunctionRevisionId::from_bytes([46; 16]),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Volatile,
        );
        let nullable_null = MutationAssignment::new(
            target_id,
            title_id,
            MutationExpression::new(
                MutationExpressionKind::TypedNull,
                MutationValueType::new(
                    SemanticType::scalar(StandardScalar::CharacterLargeObject),
                    true,
                ),
            ),
        );
        for assignments in [
            vec![cross_owner],
            vec![unknown_field],
            vec![wrong_field_type],
            vec![nullable_null],
            Vec::new(),
        ] {
            assert!(validate_mutation_assignments(&assignments, &target, &function, true).is_err());
        }
        assert!(matches!(
            validate_mutation_assignments(
                &[wrong_parameter_type],
                &target,
                &function_with_wrong_parameter_type,
                true,
            ),
            Err(PrepareError::InvalidCheckedBundle {
                reason: "mutation parameter type differs from its expression"
            })
        ));

        let expected = vec![
            (
                DefinitionReferenceKind::WriteObject,
                DefinitionReferenceTarget::ObjectType(target_id),
            ),
            (
                DefinitionReferenceKind::WriteField,
                DefinitionReferenceTarget::Field {
                    owner: target_id,
                    field: title_id,
                },
            ),
        ];
        assert!(
            validate_reference_sequence(
                &expected,
                &expected,
                "mutation definition references differ from the checked body"
            )
            .is_ok()
        );
        let mut reordered = expected.clone();
        reordered.reverse();
        assert!(
            validate_reference_sequence(
                &expected,
                &reordered,
                "mutation definition references differ from the checked body"
            )
            .is_err()
        );
        assert!(
            validate_reference_sequence(
                &expected,
                &expected[..1],
                "mutation definition references differ from the checked body"
            )
            .is_err()
        );
    }

    #[test]
    fn mutation_parameter_validation_rejects_unused_unsupported_types_and_defaults() {
        let function_id = FunctionId::from_bytes([51; 16]);
        let valid_parameter_id = ParameterId::from_bytes([52; 16]);
        let unused_parameter_id = ParameterId::from_bytes([53; 16]);
        let function_with_unused = |resolved_type, default_expression| {
            FunctionDefinition::new(
                function_id,
                semantic_name(&["tasks", "create"]),
                FunctionDomain::Server,
                vec![
                    ParameterDefinition::new(
                        valid_parameter_id,
                        "used_title",
                        0,
                        ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                        None,
                    ),
                    ParameterDefinition::new(
                        unused_parameter_id,
                        "unused",
                        1,
                        resolved_type,
                        default_expression,
                    ),
                ],
                FunctionReturn::Rows(Vec::new()),
                FunctionRevisionId::from_bytes([54; 16]),
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::Atomic),
                FunctionVolatility::Volatile,
            )
        };

        let unsupported = function_with_unused(ResolvedType::scalar(StandardScalar::Decimal), None);
        assert!(matches!(
            validate_mutation_parameters(&unsupported, &[]),
            Err(PrepareError::InvalidCheckedBundle {
                reason: "mutation parameter has an unsupported runtime type"
            })
        ));

        let defaulted = function_with_unused(
            ResolvedType::scalar(StandardScalar::Integer),
            Some(ExpressionId::from_bytes([55; 16])),
        );
        assert!(matches!(
            validate_mutation_parameters(&defaulted, &[]),
            Err(PrepareError::InvalidCheckedBundle {
                reason: "mutation parameter has an unsupported default expression"
            })
        ));
    }

    #[test]
    fn mutation_selector_validation_requires_exact_owner_parameter_and_target() {
        let function_id = FunctionId::from_bytes([61; 16]);
        let parameter_id = ParameterId::from_bytes([62; 16]);
        let target = TypeId::from_bytes([63; 16]);
        let function_with = |resolved_type| {
            FunctionDefinition::new(
                function_id,
                semantic_name(&["tasks", "update"]),
                FunctionDomain::Server,
                vec![ParameterDefinition::new(
                    parameter_id,
                    "selected",
                    0,
                    resolved_type,
                    None,
                )],
                FunctionReturn::Rows(Vec::new()),
                FunctionRevisionId::from_bytes([64; 16]),
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::Atomic),
                FunctionVolatility::Volatile,
            )
        };
        let valid = function_with(ResolvedType::reference(target));
        assert!(validate_mutation_selector(function_id, parameter_id, target, &valid).is_ok());
        assert!(matches!(
            validate_mutation_selector(
                FunctionId::from_bytes([65; 16]),
                parameter_id,
                target,
                &valid,
            ),
            Err(PrepareError::InvalidCheckedBundle {
                reason: "mutation selector owner differs from its enclosing function"
            })
        ));
        assert!(matches!(
            validate_mutation_selector(
                function_id,
                ParameterId::from_bytes([66; 16]),
                target,
                &valid,
            ),
            Err(PrepareError::InvalidCheckedBundle {
                reason: "mutation selector parameter is not declared by its enclosing function"
            })
        ));
        let wrong_type = function_with(ResolvedType::scalar(StandardScalar::BigInt));
        assert!(matches!(
            validate_mutation_selector(function_id, parameter_id, target, &wrong_type),
            Err(PrepareError::InvalidCheckedBundle {
                reason: "mutation selector parameter does not reference its target object"
            })
        ));
        let wrong_target = function_with(ResolvedType::reference(TypeId::from_bytes([67; 16])));
        assert!(matches!(
            validate_mutation_selector(function_id, parameter_id, target, &wrong_target),
            Err(PrepareError::InvalidCheckedBundle {
                reason: "mutation selector parameter does not reference its target object"
            })
        ));
    }

    #[test]
    fn delete_preparation_revalidates_target_modes_result_and_evidence() {
        let function_id = FunctionId::from_bytes([71; 16]);
        let parameter_id = ParameterId::from_bytes([72; 16]);
        let target_id = TypeId::from_bytes([73; 16]);
        let revision_id = FunctionRevisionId::from_bytes([74; 16]);
        let target =
            ObjectTypeDefinition::new(target_id, semantic_name(&["tasks", "task"]), Vec::new());
        let function_with = |return_type, security| {
            FunctionDefinition::new(
                function_id,
                semantic_name(&["tasks", "remove"]),
                FunctionDomain::Server,
                vec![ParameterDefinition::new(
                    parameter_id,
                    "p_task",
                    0,
                    ResolvedType::reference(target_id),
                    None,
                )],
                return_type,
                revision_id,
                security,
                Some(FunctionTransaction::Atomic),
                FunctionVolatility::Volatile,
            )
        };
        let boolean_rows = || {
            FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
                "deleted",
                0,
                ResolvedType::scalar(StandardScalar::Boolean),
            )])
        };
        let function = function_with(boolean_rows(), FunctionSecurity::Invoker);
        let plan = DeletePlanIr::new(target_id, function_id, parameter_id);
        let references = delete_reference_sequence(&plan, &function);

        assert!(
            server_delete_plan(&plan, &function, std::slice::from_ref(&target), &references)
                .is_ok()
        );
        assert!(matches!(
            server_delete_plan(&plan, &function, &[], &references),
            Err(PrepareError::InvalidCheckedBundle {
                reason: "DELETE target object is absent from the candidate catalogue"
            })
        ));

        let definer = function_with(boolean_rows(), FunctionSecurity::Definer);
        assert!(matches!(
            server_delete_plan(
                &plan,
                &definer,
                std::slice::from_ref(&target),
                &delete_reference_sequence(&plan, &definer),
            ),
            Err(PrepareError::InvalidCheckedBundle {
                reason: "DELETE function has unsupported execution modes"
            })
        ));

        let wrong_result = function_with(
            FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
                "deleted",
                0,
                ResolvedType::scalar(StandardScalar::Integer),
            )]),
            FunctionSecurity::Invoker,
        );
        assert!(matches!(
            server_delete_plan(
                &plan,
                &wrong_result,
                std::slice::from_ref(&target),
                &delete_reference_sequence(&plan, &wrong_result),
            ),
            Err(PrepareError::InvalidCheckedBundle {
                reason: "DELETE function does not return exactly one BOOLEAN column"
            })
        ));

        assert!(matches!(
            server_delete_plan(
                &plan,
                &function,
                std::slice::from_ref(&target),
                &references[..references.len() - 1],
            ),
            Err(PrepareError::InvalidCheckedBundle {
                reason: "mutation definition references differ from the checked body"
            })
        ));
    }

    #[test]
    fn allocates_fresh_candidate_revisions_for_repeated_preparation() {
        let active = empty_active();
        let report = checked_report(SOURCE, active.catalogue());

        let first = prepare(&report, active.pair(), &active).unwrap();
        let second = prepare(&report, active.pair(), &active).unwrap();

        assert_ne!(first.candidate_pair(), second.candidate_pair());
        assert_ne!(first.source().bundle(), second.source().bundle());
        assert_ne!(
            first.source().units()[0].id(),
            second.source().units()[0].id()
        );
        assert_ne!(
            first.candidate().object_types()[0].id(),
            second.candidate().object_types()[0].id()
        );
    }

    #[test]
    fn prepares_and_replays_required_unique_references_fail_closed() {
        let empty = empty_active();
        let report = checked_report(REQUIRED_UNIQUE_REFERENCE_SOURCE, empty.catalogue());
        let checked = report.checked_bundle().unwrap();
        let checked_assignment = checked
            .object_types()
            .iter()
            .find(|object_type| object_type.name() == &semantic_name(&["relations", "assignment"]))
            .unwrap();
        let checked_field = &checked_assignment.fields()[0];
        let checked_owner = checked
            .object_types()
            .iter()
            .find(|object_type| object_type.name() == &semantic_name(&["relations", "owner"]))
            .unwrap()
            .id();

        let prepared = prepare(&report, empty.pair(), &empty).unwrap();
        let assignment = prepared
            .candidate()
            .object_type_by_name(&semantic_name(&["relations", "assignment"]))
            .unwrap();
        let owner = prepared
            .candidate()
            .object_type_by_name(&semantic_name(&["relations", "owner"]))
            .unwrap();
        let field = assignment.field_by_name("owner").unwrap();
        assert!(field.unique());
        assert!(!field.nullable());
        assert_eq!(field.resolved_type(), ResolvedType::reference(owner.id()));

        let field_id = field.id();
        let assignment_id = assignment.id();
        let owner_id = owner.id();
        let active = activate(&prepared, Vec::new(), Vec::new());
        let replay = prepare(
            &checked_report(REQUIRED_UNIQUE_REFERENCE_SOURCE, active.catalogue()),
            active.pair(),
            &active,
        )
        .unwrap();
        let replay_assignment = replay
            .candidate()
            .object_type_by_name(&semantic_name(&["relations", "assignment"]))
            .unwrap();
        let replay_owner = replay
            .candidate()
            .object_type_by_name(&semantic_name(&["relations", "owner"]))
            .unwrap();
        let replay_field = replay_assignment.field_by_name("owner").unwrap();
        assert_eq!(replay_assignment.id(), assignment_id);
        assert_eq!(replay_owner.id(), owner_id);
        assert_eq!(replay_field.id(), field_id);
        assert!(replay_field.unique());
        assert_eq!(replay_field.resolved_type(), field.resolved_type());

        for (semantic_type, nullable) in [
            (SemanticType::scalar(StandardScalar::Boolean), false),
            (SemanticType::reference(checked_owner), true),
        ] {
            let mut malformed = report.clone();
            assert!(malformed.replace_checked_field_facts_for_test(
                checked_assignment.id(),
                checked_field.id(),
                semantic_type,
                nullable,
                true,
            ));
            assert_preparation_reason(
                prepare(&malformed, empty.pair(), &empty),
                REQUIRED_UNIQUE_REFERENCE_MESSAGE,
            );
        }
    }

    #[test]
    fn preserves_complete_multi_unit_source_order_and_exact_bytes() {
        let active = empty_active();
        let first = "-- first\nCREATE SCHEMA multi;\n";
        let second = "-- second\r\nCREATE TYPE multi.item AS OBJECT (value INT);\r\n";
        let bundle = SourceBundle::new([
            SourceUnit::new("01-schema.orna", first),
            SourceUnit::new("02-type.orna", second),
        ])
        .unwrap();
        let report = check(&bundle, active.catalogue());

        let prepared = prepare(&report, active.pair(), &active).unwrap();

        assert_eq!(
            prepared
                .source()
                .units()
                .iter()
                .map(|unit| (unit.ordinal(), unit.logical_path(), unit.content()))
                .collect::<Vec<_>>(),
            vec![(0, "01-schema.orna", first), (1, "02-type.orna", second),]
        );
    }

    #[test]
    fn rejects_incomplete_and_stale_inputs_before_preparation() {
        let active = empty_active();
        let failed = checked_report("CREATE SCHEMA ;", active.catalogue());
        assert!(matches!(
            prepare(&failed, active.pair(), &active),
            Err(PrepareError::CheckNotComplete {
                diagnostic_count: 1
            })
        ));

        let report = checked_report(SOURCE, active.catalogue());
        let stale_source = RevisionPair::new(SourceRevisionId::new(), active.pair().catalogue());
        assert!(matches!(
            prepare(&report, stale_source, &active),
            Err(PrepareError::ExpectedBaseMismatch { .. })
        ));
        let stale_catalogue = RevisionPair::new(active.pair().source(), CatalogueRevisionId::new());
        assert!(matches!(
            prepare(&report, stale_catalogue, &active),
            Err(PrepareError::ExpectedBaseMismatch { .. })
        ));

        let other_base = empty_active();
        let mismatched = checked_report(SOURCE, other_base.catalogue());
        assert!(matches!(
            prepare(&mismatched, active.pair(), &active),
            Err(PrepareError::CheckedBaseMismatch { .. })
        ));
    }

    #[test]
    fn rejects_existing_identities_absent_from_the_exact_active_catalogue() {
        let active = empty_active();
        let schema_id = SchemaId::new();
        let false_base = CatalogueSnapshot::new(
            active.catalogue().revision(),
            vec![SchemaDefinition::new(schema_id, semantic_name(&["tasks"]))],
            Vec::new(),
        )
        .unwrap();
        let report = checked_report(SOURCE, &false_base);

        assert!(matches!(
            prepare(&report, active.pair(), &active),
            Err(PrepareError::ExistingDefinitionMismatch {
                definition: DefinitionIdentity::Schema(id),
            }) if id == schema_id
        ));
    }

    #[test]
    fn retains_one_identical_artifact_for_a_shared_existing_expression() {
        let active = shared_expression_active();
        let report = checked_report(SHARED_EXPRESSION_SOURCE, active.catalogue());

        let prepared = prepare(&report, active.pair(), &active).unwrap();

        assert_eq!(prepared.expressions().len(), 1);
        let fields = prepared.candidate().object_types()[0].fields();
        assert_eq!(
            fields[0].default_expression(),
            fields[1].default_expression()
        );
        let expression_origins = prepared
            .origins()
            .iter()
            .filter(|origin| matches!(origin.identity(), DefinitionIdentity::Expression(_)))
            .count();
        assert_eq!(expression_origins, 1);

        let inconsistent = checked_report(
            "CREATE SCHEMA demo; CREATE TYPE demo.item AS OBJECT (\
             first INT DEFAULT 1, second INT DEFAULT 2);",
            active.catalogue(),
        );
        assert!(matches!(
            prepare(&inconsistent, active.pair(), &active),
            Err(PrepareError::InvalidCheckedBundle {
                reason: "shared checked expression has inconsistent values",
            })
        ));
    }

    #[test]
    fn source_only_edits_reuse_the_immutable_function_revision() {
        let active = empty_active();
        let initial = prepare(
            &checked_report(SOURCE, active.catalogue()),
            active.pair(),
            &active,
        )
        .unwrap();
        let current_revision = initial.new_function_revisions()[0].clone();
        let active = activate(&initial, vec![current_revision.clone()], Vec::new());
        let report = checked_report(REFORMATTED_SOURCE, active.catalogue());

        let prepared = prepare(&report, active.pair(), &active).unwrap();

        assert!(prepared.new_function_revisions().is_empty());
        assert_eq!(
            prepared.candidate().schemas()[0].id(),
            active.catalogue().schemas()[0].id()
        );
        for (candidate, previous) in prepared
            .candidate()
            .object_types()
            .iter()
            .zip(active.catalogue().object_types())
        {
            assert_eq!(candidate.id(), previous.id());
            assert_eq!(
                candidate
                    .fields()
                    .iter()
                    .map(FieldDefinition::id)
                    .collect::<Vec<_>>(),
                previous
                    .fields()
                    .iter()
                    .map(FieldDefinition::id)
                    .collect::<Vec<_>>()
            );
        }
        assert_eq!(
            prepared.candidate().functions()[0].current_revision(),
            current_revision.id()
        );
        let current_origin = prepared
            .origins()
            .iter()
            .find(|origin| {
                origin.identity() == DefinitionIdentity::Function(current_revision.function())
            })
            .unwrap()
            .source();
        assert_ne!(
            current_origin.source_unit(),
            current_revision.declaration_origin().source_unit()
        );
        assert_eq!(
            current_revision.declaration_content_hash(),
            active.function_revisions()[0].declaration_content_hash()
        );
        assert_eq!(
            catalogue_digest(
                prepared.candidate(),
                active.function_revisions(),
                prepared.expressions(),
                prepared.origins(),
                prepared.references(),
            )
            .unwrap(),
            prepared.catalogue_hash()
        );
    }

    #[test]
    fn field_rename_preparation_preserves_field_and_function_identities_on_replay() {
        let original_source = "CREATE SCHEMA people;\nCREATE TYPE people.person AS OBJECT (email TEXT NOT NULL);\nCREATE SERVER FUNCTION people.list_emails() RETURNS ROWS (email TEXT) AS SELECT p.email FROM people.person p;\n";
        let renamed_source = "CREATE SCHEMA people;\nCREATE TYPE people.person AS OBJECT (primary_email TEXT NOT NULL);\nALTER TYPE people.person RENAME FIELD email TO primary_email;\nCREATE SERVER FUNCTION people.list_emails() RETURNS ROWS (email TEXT) AS SELECT p.primary_email FROM people.person p;\n";
        let empty = empty_active();
        let original = prepare(
            &checked_report(original_source, empty.catalogue()),
            empty.pair(),
            &empty,
        )
        .unwrap();
        let original_revision = original.new_function_revisions()[0].clone();
        let original_field = original.candidate().object_types()[0].fields()[0].id();
        let owner = original.candidate().object_types()[0].id();
        let active = activate(&original, vec![original_revision.clone()], Vec::new());

        let renamed = prepare(
            &checked_report(renamed_source, active.catalogue()),
            active.pair(),
            &active,
        )
        .unwrap();
        let field = &renamed.candidate().object_types()[0].fields()[0];
        assert_eq!(field.name(), "primary_email");
        assert_eq!(field.id(), original_field);
        let field_origin = renamed
            .origins()
            .iter()
            .find(|origin| {
                origin.identity()
                    == DefinitionIdentity::Field {
                        owner,
                        field: original_field,
                    }
            })
            .unwrap()
            .source();
        let create_field = renamed_source.find("primary_email TEXT").unwrap();
        assert_eq!(field_origin.byte_start() as usize, create_field);
        assert_eq!(
            field_origin.byte_end() as usize,
            create_field + "primary_email TEXT NOT NULL".len()
        );
        assert_ne!(
            field_origin.byte_start() as usize,
            renamed_source.find("TO primary_email").unwrap() + 3
        );
        let reference = renamed
            .references()
            .iter()
            .find(|reference| reference.kind() == DefinitionReferenceKind::QueryField)
            .unwrap();
        assert_eq!(
            reference.target(),
            DefinitionReferenceTarget::Field {
                owner,
                field: original_field
            }
        );
        let dependent_token = renamed_source.find("p.primary_email").unwrap() + 2;
        assert_eq!(
            reference.source_origin().byte_start() as usize,
            dependent_token
        );
        assert_eq!(
            reference.source_origin().byte_end() as usize,
            dependent_token + "primary_email".len()
        );
        assert_ne!(renamed.source().bundle(), active.source().bundle());
        assert_ne!(
            renamed.source().bundle_hash(),
            active.source().bundle_hash()
        );
        assert_ne!(
            renamed.source().revision_hash(),
            active.source().revision_hash()
        );
        assert_ne!(renamed.catalogue_hash(), active.catalogue_hash());
        assert!(renamed.new_function_revisions().is_empty());
        assert_eq!(
            renamed.candidate().functions()[0].current_revision(),
            original_revision.id()
        );
        assert_eq!(
            active.function_revisions(),
            std::slice::from_ref(&original_revision)
        );

        let replay_active = activate(&renamed, vec![original_revision.clone()], Vec::new());
        assert_eq!(
            replay_active.function_revisions(),
            std::slice::from_ref(&original_revision)
        );
        assert_eq!(
            replay_active.function_revisions()[0].artifact(),
            original_revision.artifact()
        );
        let replay = prepare(
            &checked_report(renamed_source, replay_active.catalogue()),
            replay_active.pair(),
            &replay_active,
        )
        .unwrap();
        assert_eq!(
            replay.candidate().object_types()[0].fields()[0].id(),
            original_field
        );
        assert_eq!(
            replay.candidate().functions()[0].current_revision(),
            original_revision.id()
        );
        assert!(replay.new_function_revisions().is_empty());
    }

    #[test]
    fn changed_semantics_use_the_max_history_revision_number_plus_one() {
        let active = empty_active();
        let initial = prepare(
            &checked_report(SOURCE, active.catalogue()),
            active.pair(),
            &active,
        )
        .unwrap();
        let current = initial.new_function_revisions()[0].clone();
        let history = FunctionRevisionRecord::new(
            current.function(),
            FunctionRevisionId::new(),
            7,
            SourceOrigin::new(SourceUnitId::new(), 0, 1).unwrap(),
            digest(71),
            digest(72),
            SERVER_PLAN_LANGUAGE_VERSION,
            current.artifact().clone(),
        )
        .unwrap();
        let active = activate(&initial, vec![current], vec![history]);

        let prepared = prepare(
            &checked_report(CHANGED_SOURCE, active.catalogue()),
            active.pair(),
            &active,
        )
        .unwrap();

        assert_eq!(prepared.new_function_revisions().len(), 1);
        assert_eq!(prepared.new_function_revisions()[0].revision_number(), 8);
        assert_ne!(
            prepared.new_function_revisions()[0].semantic_hash(),
            active.function_revisions()[0].semantic_hash()
        );
    }

    #[test]
    fn semantic_history_reuse_selects_the_lowest_matching_revision() {
        let empty = empty_active();
        let initial = prepare(
            &checked_report(SOURCE, empty.catalogue()),
            empty.pair(),
            &empty,
        )
        .unwrap();
        let old = initial.new_function_revisions()[0].clone();
        let active_v1 = activate(&initial, vec![old.clone()], Vec::new());
        let changed = prepare(
            &checked_report(CHANGED_SOURCE, active_v1.catalogue()),
            active_v1.pair(),
            &active_v1,
        )
        .unwrap();
        let current = changed.new_function_revisions()[0].clone();
        let equivalent_later = FunctionRevisionRecord::new(
            old.function(),
            FunctionRevisionId::new(),
            3,
            SourceOrigin::new(SourceUnitId::new(), 0, 1).unwrap(),
            digest(73),
            old.semantic_hash(),
            old.language_version(),
            old.artifact().clone(),
        )
        .unwrap();
        let active = activate(&changed, vec![current], vec![old.clone(), equivalent_later]);

        let prepared = prepare(
            &checked_report(REFORMATTED_SOURCE, active.catalogue()),
            active.pair(),
            &active,
        )
        .unwrap();

        assert!(prepared.new_function_revisions().is_empty());
        assert_eq!(
            prepared.candidate().functions()[0].current_revision(),
            old.id()
        );
    }

    #[test]
    fn legacy_reuse_remains_semantic_hash_only() {
        let empty = empty_active();
        let initial = prepare(
            &checked_report(SOURCE, empty.catalogue()),
            empty.pair(),
            &empty,
        )
        .unwrap();
        let old = initial.new_function_revisions()[0].clone();
        let active_v1 = activate(&initial, vec![old.clone()], Vec::new());
        let changed = prepare(
            &checked_report(CHANGED_SOURCE, active_v1.catalogue()),
            active_v1.pair(),
            &active_v1,
        )
        .unwrap();
        let current = changed.new_function_revisions()[0].clone();
        let legacy_match = FunctionRevisionRecord::new(
            old.function(),
            old.id(),
            old.revision_number(),
            old.declaration_origin(),
            old.declaration_content_hash(),
            old.semantic_hash(),
            "legacy.claimed-language",
            old.artifact().clone(),
        )
        .unwrap();
        let active = activate(&changed, vec![current], vec![legacy_match.clone()]);

        let error = prepare(
            &checked_report(REFORMATTED_SOURCE, active.catalogue()),
            active.pair(),
            &active,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            PrepareError::CanonicalHash(CanonicalHashError::FunctionSemanticHashMismatch {
                function,
                revision,
            }) if function == legacy_match.function() && revision == legacy_match.id()
        ));
    }

    #[test]
    fn standard_upgrade_gate_seven_retains_the_current_version_one_revision() {
        let empty = empty_active();
        let initial = prepare(
            &checked_report(SOURCE, empty.catalogue()),
            empty.pair(),
            &empty,
        )
        .unwrap();
        let first = initial.new_function_revisions()[0].clone();
        let first_active = activate(&initial, vec![first], Vec::new());
        let changed = prepare(
            &checked_report(CHANGED_SOURCE, first_active.catalogue()),
            first_active.pair(),
            &first_active,
        )
        .unwrap();
        let current = changed.new_function_revisions()[0].clone();
        let older_equal = FunctionRevisionRecord::new(
            current.function(),
            FunctionRevisionId::new(),
            1,
            SourceOrigin::new(SourceUnitId::new(), 0, 1).unwrap(),
            digest(74),
            current.semantic_hash(),
            current.language_version(),
            current.artifact().clone(),
        )
        .unwrap();
        let active = activate(&changed, vec![current.clone()], vec![older_equal.clone()]);
        let definition = active.catalogue().functions()[0].clone();
        let make_plan = |current_only| {
            FunctionRevisionPlan::new(
                &active,
                current.function(),
                FunctionRevisionPlanInput {
                    semantic_hash_version: FunctionSemanticHashVersion::Version1,
                    definition: &definition,
                    language_version: current.language_version(),
                    artifact: current.artifact(),
                    expressions: active.expressions(),
                    references: active.references(),
                    current_only,
                    reuse_policy: FunctionRevisionReusePolicy::Complete,
                },
            )
            .unwrap()
        };

        assert_eq!(
            make_plan(false).reusable.unwrap().id(),
            older_equal.id(),
            "the fixture must expose the historical rollback"
        );
        assert_eq!(
            make_plan(standard_upgrade_reuse_is_current_only(
                FunctionSemanticHashVersion::Version1,
            ))
            .reusable
            .unwrap()
            .id(),
            current.id()
        );
        assert!(!standard_upgrade_reuse_is_current_only(
            FunctionSemanticHashVersion::Version2
        ));
    }

    fn checked_report(source: &str, base: &CatalogueSnapshot) -> CheckReport {
        let bundle = SourceBundle::new([SourceUnit::new("tasks.orna", source)]).unwrap();
        check(&bundle, base)
    }

    type DistinctFixture = (
        crate::relational::DistinctQueryIr<TypeId, FieldId>,
        FunctionDefinition,
        Vec<ObjectTypeDefinition>,
        Vec<(DefinitionReferenceKind, DefinitionReferenceTarget)>,
    );

    type VersionOneFixture = (
        crate::relational::RelationalQueryIr<TypeId, FieldId>,
        FunctionDefinition,
        Vec<ObjectTypeDefinition>,
        Vec<(DefinitionReferenceKind, DefinitionReferenceTarget)>,
    );

    fn mapped_version_one_fixture() -> VersionOneFixture {
        mapped_version_one_fixture_for(SOURCE)
    }

    fn mapped_version_one_fixture_for(source: &str) -> VersionOneFixture {
        let active = empty_active();
        let report = checked_report(source, active.catalogue());
        let prepared = prepare(&report, active.pair(), &active).unwrap();
        let checked = report.checked_bundle().unwrap();
        let mut type_ids = std::collections::HashMap::new();
        let mut field_ids = std::collections::HashMap::new();
        for checked_object in checked.object_types() {
            let candidate = prepared
                .candidate()
                .object_type_by_name(checked_object.name())
                .unwrap();
            type_ids.insert(checked_object.id(), candidate.id());
            for checked_field in checked_object.fields() {
                field_ids.insert(
                    checked_field.id(),
                    candidate.field_by_name(checked_field.name()).unwrap().id(),
                );
            }
        }
        let plan = checked.server_functions()[0]
            .query_plan()
            .unwrap()
            .try_map_identities(
                |id| {
                    type_ids
                        .get(&id)
                        .copied()
                        .ok_or(PrepareError::InvalidCheckedBundle {
                            reason: "type mapping is absent",
                        })
                },
                |id| {
                    field_ids
                        .get(&id)
                        .copied()
                        .ok_or(PrepareError::InvalidCheckedBundle {
                            reason: "field mapping is absent",
                        })
                },
            )
            .unwrap();
        let function = prepared.candidate().functions()[0].clone();
        let object_types = prepared.candidate().object_types().to_vec();
        let references = version_one_query_reference_sequence(&plan, &function);
        (plan, function, object_types, references)
    }

    fn mapped_distinct_fixture() -> DistinctFixture {
        mapped_distinct_fixture_for(DISTINCT_SOURCE)
    }

    fn mapped_distinct_fixture_for(source: &str) -> DistinctFixture {
        let active = empty_active();
        let report = checked_report(source, active.catalogue());
        let prepared = prepare(&report, active.pair(), &active).unwrap();
        let checked = report.checked_bundle().unwrap();
        let mut type_ids = std::collections::HashMap::new();
        let mut field_ids = std::collections::HashMap::new();
        for checked_object in checked.object_types() {
            let candidate = prepared
                .candidate()
                .object_type_by_name(checked_object.name())
                .unwrap();
            type_ids.insert(checked_object.id(), candidate.id());
            for checked_field in checked_object.fields() {
                field_ids.insert(
                    checked_field.id(),
                    candidate.field_by_name(checked_field.name()).unwrap().id(),
                );
            }
        }
        let plan = checked.server_functions()[0]
            .distinct_query_plan()
            .unwrap()
            .try_map_identities(
                |id| {
                    type_ids
                        .get(&id)
                        .copied()
                        .ok_or(PrepareError::InvalidCheckedBundle {
                            reason: "type mapping is absent",
                        })
                },
                |id| {
                    field_ids
                        .get(&id)
                        .copied()
                        .ok_or(PrepareError::InvalidCheckedBundle {
                            reason: "field mapping is absent",
                        })
                },
            )
            .unwrap();
        let function = prepared.candidate().functions()[0].clone();
        let object_types = prepared.candidate().object_types().to_vec();
        let references = distinct_query_reference_sequence(&plan, &function);
        (plan, function, object_types, references)
    }

    fn object_types_with_distinct_completed_type(
        object_types: &[ObjectTypeDefinition],
        resolved_type: ResolvedType,
    ) -> Vec<ObjectTypeDefinition> {
        object_types
            .iter()
            .map(|object_type| {
                if object_type.name() != &semantic_name(&["tasks", "task"]) {
                    return object_type.clone();
                }
                ObjectTypeDefinition::new(
                    object_type.id(),
                    object_type.name().clone(),
                    object_type
                        .fields()
                        .iter()
                        .map(|field| {
                            if field.name() == "completed" {
                                FieldDefinition::new(
                                    field.id(),
                                    field.name(),
                                    field.ordinal(),
                                    resolved_type,
                                    field.nullable(),
                                    field.unique(),
                                    field.default_expression(),
                                    field.on_delete(),
                                )
                            } else {
                                field.clone()
                            }
                        })
                        .collect(),
                )
            })
            .collect()
    }

    fn object_types_with_task_field(
        object_types: &[ObjectTypeDefinition],
        field_name: &str,
        resolved_type: ResolvedType,
        nullable: bool,
    ) -> Vec<ObjectTypeDefinition> {
        object_types
            .iter()
            .map(|object_type| {
                if object_type.name() != &semantic_name(&["tasks", "task"]) {
                    return object_type.clone();
                }
                ObjectTypeDefinition::new(
                    object_type.id(),
                    object_type.name().clone(),
                    object_type
                        .fields()
                        .iter()
                        .map(|field| {
                            if field.name() != field_name {
                                return field.clone();
                            }
                            FieldDefinition::new(
                                field.id(),
                                field.name(),
                                field.ordinal(),
                                resolved_type,
                                nullable,
                                field.unique(),
                                field.default_expression(),
                                field.on_delete(),
                            )
                        })
                        .collect(),
                )
            })
            .collect()
    }

    fn distinct_function_with_completed_type(
        function: &FunctionDefinition,
        resolved_type: ResolvedType,
    ) -> FunctionDefinition {
        let FunctionReturn::Rows(columns) = function.return_type() else {
            panic!("DISTINCT fixture function must return ROWS");
        };
        let mut columns = columns.to_vec();
        let completed = &columns[2];
        columns[2] = FunctionReturnColumnDefinition::new(
            completed.name(),
            completed.ordinal(),
            resolved_type,
        );
        FunctionDefinition::new(
            function.id(),
            function.name().clone(),
            function.domain(),
            function.parameters().to_vec(),
            FunctionReturn::Rows(columns),
            function.current_revision(),
            function.security(),
            function.transaction(),
            function.volatility(),
        )
    }

    fn mapped_distinct_plan(
        plan: &crate::relational::DistinctQueryIr<TypeId, FieldId>,
        map_type: impl FnMut(TypeId) -> Result<TypeId, PrepareError>,
        map_field: impl FnMut(FieldId) -> Result<FieldId, PrepareError>,
    ) -> Result<crate::relational::DistinctQueryIr<TypeId, FieldId>, PrepareError> {
        plan.try_map_identities(map_type, map_field)
    }

    fn assert_preparation_reason<T>(result: Result<T, PrepareError>, reason: &'static str) {
        assert!(matches!(
            result,
            Err(PrepareError::InvalidCheckedBundle { reason: actual }) if actual == reason
        ));
    }

    fn empty_active() -> ActiveDatabaseRevision {
        let source_bundle = SourceBundleId::new();
        let source_revision = SourceRevisionId::new();
        let bundle_hash = source_bundle_digest(&[]).unwrap();
        let source = StoredSourceRevision::new(
            source_bundle,
            source_revision,
            None,
            Vec::new(),
            bundle_hash,
            source_revision_record_digest(source_bundle, None, bundle_hash).unwrap(),
        )
        .unwrap();
        let catalogue =
            CatalogueSnapshot::new(CatalogueRevisionId::new(), Vec::new(), Vec::new()).unwrap();
        let pair = RevisionPair::new(source.id(), catalogue.revision());
        let catalogue_hash = catalogue_digest(&catalogue, &[], &[], &[], &[]).unwrap();
        ActiveDatabaseRevision::new(
            pair,
            source,
            catalogue,
            catalogue_hash,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap()
    }

    fn shared_expression_active() -> ActiveDatabaseRevision {
        let schema = SchemaDefinition::new(SchemaId::new(), semantic_name(&["demo"]));
        let object_type_id = TypeId::new();
        let first_field = FieldId::new();
        let second_field = FieldId::new();
        let expression_id = ExpressionId::new();
        let catalogue = CatalogueSnapshot::new(
            CatalogueRevisionId::new(),
            vec![schema.clone()],
            vec![ObjectTypeDefinition::new(
                object_type_id,
                semantic_name(&["demo", "item"]),
                vec![
                    FieldDefinition::new(
                        first_field,
                        "first",
                        0,
                        ResolvedType::scalar(StandardScalar::Integer),
                        true,
                        false,
                        Some(expression_id),
                        None,
                    ),
                    FieldDefinition::new(
                        second_field,
                        "second",
                        1,
                        ResolvedType::scalar(StandardScalar::Integer),
                        true,
                        false,
                        Some(expression_id),
                        None,
                    ),
                ],
            )],
        )
        .unwrap();

        let source_bundle = SourceBundleId::new();
        let source_revision = SourceRevisionId::new();
        let source_unit = SourceUnitId::new();
        let content_hash = source_unit_content_digest(SHARED_EXPRESSION_SOURCE).unwrap();
        let unit = StoredSourceUnit::new(
            source_unit,
            0,
            "tasks.orna",
            SHARED_EXPRESSION_SOURCE,
            content_hash,
        )
        .unwrap();
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
        let source = StoredSourceRevision::new(
            source_bundle,
            source_revision,
            None,
            vec![unit],
            bundle_hash,
            source_revision_record_digest(source_bundle, None, bundle_hash).unwrap(),
        )
        .unwrap();
        let origin =
            SourceOrigin::new(source_unit, 0, SHARED_EXPRESSION_SOURCE.len() as u32).unwrap();
        let origins = vec![
            DefinitionOrigin::new(DefinitionIdentity::Schema(schema.id()), origin),
            DefinitionOrigin::new(DefinitionIdentity::ObjectType(object_type_id), origin),
            DefinitionOrigin::new(
                DefinitionIdentity::Field {
                    owner: object_type_id,
                    field: first_field,
                },
                origin,
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::Field {
                    owner: object_type_id,
                    field: second_field,
                },
                origin,
            ),
            DefinitionOrigin::new(DefinitionIdentity::Expression(expression_id), origin),
        ];
        let payload = ConstantExpression::Integer(1).encode().unwrap();
        let artifact = ExpressionArtifact::new(
            expression_id,
            CONSTANT_FORMAT,
            CONSTANT_VERSION,
            payload.clone(),
            artifact_payload_digest(&payload).unwrap(),
        )
        .unwrap();
        let expressions = vec![artifact];
        let pair = RevisionPair::new(source.id(), catalogue.revision());
        let catalogue_hash =
            catalogue_digest(&catalogue, &[], &expressions, &origins, &[]).unwrap();
        ActiveDatabaseRevision::new(
            pair,
            source,
            catalogue,
            catalogue_hash,
            expressions,
            Vec::new(),
            origins,
            Vec::new(),
        )
        .unwrap()
    }

    fn activate(
        prepared: &DeployableRevision,
        current: Vec<FunctionRevisionRecord>,
        history: Vec<FunctionRevisionRecord>,
    ) -> ActiveDatabaseRevision {
        ActiveDatabaseRevision::new_with_history(
            prepared.candidate_pair(),
            prepared.source().clone(),
            prepared.candidate().clone(),
            prepared.catalogue_hash(),
            prepared.expressions().to_vec(),
            current,
            history,
            prepared.origins().to_vec(),
            prepared.references().to_vec(),
        )
        .unwrap()
    }

    fn semantic_name(parts: &[&str]) -> orna_core::catalogue::QualifiedSemanticName {
        orna_core::catalogue::QualifiedSemanticName::new(parts.iter().copied()).unwrap()
    }

    fn expression<'a>(
        expressions: &'a [ExpressionArtifact],
        field: &FieldDefinition,
    ) -> &'a ExpressionArtifact {
        let id = field.default_expression().unwrap();
        expressions
            .iter()
            .find(|expression| expression.id() == id)
            .unwrap()
    }

    const fn digest(byte: u8) -> Sha256Digest {
        Sha256Digest::from_bytes([byte; 32])
    }
}
