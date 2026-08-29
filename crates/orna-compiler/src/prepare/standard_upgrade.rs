//! Preparation of checked standard-library upgrades.

use super::*;

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
pub(super) fn admits_append_only_standard_child(
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
