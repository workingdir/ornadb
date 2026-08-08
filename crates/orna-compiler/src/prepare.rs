//! Construction of complete durable revisions from successful compiler checks.

use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt,
};

use orna_artifact::{
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
    SchemaId, SourceBundleId, SourceRevisionId, SourceUnitId, TypeId,
    canonical_hash::{
        CanonicalHashError, artifact_payload_digest, catalogue_digest, function_declaration_digest,
        function_semantic_digest, source_bundle_digest, source_revision_record_digest,
        source_unit_content_digest,
    },
    catalogue::{
        CatalogueSnapshot, CatalogueSnapshotError, FieldDefinition, FunctionDefinition,
        FunctionDomain, FunctionReturn, FunctionReturnColumnDefinition, FunctionSecurity,
        FunctionTransaction, FunctionVolatility, ObjectTypeDefinition, ParameterDefinition,
        SchemaDefinition,
    },
    revision::{
        ActiveDatabaseRevision, DefinitionIdentity, DefinitionOrigin, DefinitionReference,
        DefinitionReferenceKind, DefinitionReferenceTarget, DeployableRevision, ExecutableArtifact,
        ExecutableArtifactKind, ExpressionArtifact, FunctionRevisionRecord, RevisionInvariantError,
        RevisionPair, SourceOrigin, StoredSourceRevision, StoredSourceUnit,
    },
    types::{ResolvedType, StandardScalar},
};

use crate::{
    CheckReport, CheckedBundle, CheckedDefinitionReferenceTarget, CheckedExpressionId,
    CheckedFieldId, CheckedFunctionId, CheckedParameterId, CheckedSchemaId, CheckedTypeId,
    ConstantValue, SemanticType, SourceLocation,
};
use crate::{
    mutation::{DeletePlanIr, MutationExpressionKind, MutationOperation, MutationPlanIr},
    resolver::CheckedFieldRename,
};

/// One encoded SERVER artifact with the language version that defines it.
struct PreparedServerArtifact {
    artifact: ExecutableArtifact,
    language_version: &'static str,
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

    preflight(report, checked, active)?;
    let identities = IdentityMap::build(checked, active)?;
    let source = PreparedSource::new(report, expected_base.source())?;
    CandidateBuilder::new(report, checked, active, identities, source).build()
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
    validate_mutation_reference_sequence(&mutation_reference_sequence(plan, function), references)?;
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
    validate_mutation_reference_sequence(&delete_reference_sequence(plan, function), references)?;

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
        match parameter.resolved_type() {
            ResolvedType::Scalar(
                StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::Float
                | StandardScalar::CharacterLargeObject
                | StandardScalar::BinaryLargeObject,
            ) => {}
            ResolvedType::Reference { target }
                if object_types
                    .iter()
                    .any(|object_type| object_type.id() == target) => {}
            ResolvedType::Reference { .. } => {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "mutation parameter REF target is absent from the candidate catalogue",
                });
            }
            ResolvedType::Scalar(_) | ResolvedType::Named(_) => {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "mutation parameter has an unsupported runtime type",
                });
            }
        }
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

fn validate_mutation_reference_sequence(
    expected: &[(DefinitionReferenceKind, DefinitionReferenceTarget)],
    actual: &[(DefinitionReferenceKind, DefinitionReferenceTarget)],
) -> Result<(), PrepareError> {
    if expected == actual {
        Ok(())
    } else {
        Err(PrepareError::InvalidCheckedBundle {
            reason: "mutation definition references differ from the checked body",
        })
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
    report: &CheckReport,
    checked: &CheckedBundle,
    active: &ActiveDatabaseRevision,
) -> Result<(), PrepareError> {
    let units = report.parse_report().units();
    if u32::try_from(units.len()).is_err() {
        return Err(PrepareError::SourceUnitCountExceedsU32 { count: units.len() });
    }
    let mut sources = HashMap::with_capacity(units.len());
    for unit in units {
        if sources
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

    for location in checked_locations(checked) {
        validate_location(location, &sources)?;
    }
    validate_field_renames(checked, active)?;
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
struct IdentityMap {
    schemas: HashMap<CheckedSchemaId, SchemaId>,
    types: HashMap<CheckedTypeId, TypeId>,
    fields: HashMap<CheckedFieldId, FieldId>,
    expressions: HashMap<CheckedExpressionId, ExpressionId>,
    functions: HashMap<CheckedFunctionId, FunctionId>,
    parameters: HashMap<CheckedParameterId, ParameterId>,
}

impl IdentityMap {
    fn build(
        checked: &CheckedBundle,
        active: &ActiveDatabaseRevision,
    ) -> Result<Self, PrepareError> {
        Self::validate_existing(checked, active)?;
        let mut result = Self::default();
        for schema in checked.schemas() {
            let id = match schema.id() {
                CheckedSchemaId::Existing(id) => id,
                CheckedSchemaId::Provisional(_) => SchemaId::new(),
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
                CheckedTypeId::Provisional(_) => TypeId::new(),
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
                    CheckedFieldId::Provisional(_) => FieldId::new(),
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
                        CheckedExpressionId::Provisional(_) => ExpressionId::new(),
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

        for function in checked.server_functions() {
            let function_id = match function.id() {
                CheckedFunctionId::Existing(id) => id,
                CheckedFunctionId::Provisional(_) => FunctionId::new(),
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
                    CheckedParameterId::Provisional(_) => ParameterId::new(),
                };
                insert_consistent(
                    &mut result.parameters,
                    parameter.id(),
                    parameter_id,
                    "checked parameter identity maps inconsistently",
                )?;
            }
        }
        Ok(result)
    }

    fn validate_existing(
        checked: &CheckedBundle,
        active: &ActiveDatabaseRevision,
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

        for function in checked.server_functions() {
            let owner = match function.id() {
                CheckedFunctionId::Existing(id) => {
                    let matches = active
                        .catalogue()
                        .function_by_id(id)
                        .is_some_and(|base| base.name() == function.name());
                    if !matches {
                        return Err(existing_mismatch(DefinitionIdentity::Function(id)));
                    }
                    Some(id)
                }
                CheckedFunctionId::Provisional(_) => None,
            };

            for parameter in function.parameters() {
                let CheckedParameterId::Existing(id) = parameter.id() else {
                    continue;
                };
                let owner = owner.ok_or(PrepareError::InvalidCheckedBundle {
                    reason: "existing checked parameter belongs to a provisional function",
                })?;
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

impl PreparedSource {
    fn new(report: &CheckReport, parent: SourceRevisionId) -> Result<Self, PrepareError> {
        let bundle = SourceBundleId::new();
        let revision_id = SourceRevisionId::new();
        let mut unit_ids = HashMap::new();
        let mut units = Vec::with_capacity(report.parse_report().units().len());
        for (ordinal, unit) in report.parse_report().units().iter().enumerate() {
            let id = SourceUnitId::new();
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
                    count: report.parse_report().units().len(),
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
        report: &'a CheckReport,
        location: &SourceLocation,
    ) -> Result<&'a [u8], PrepareError> {
        let unit = report
            .parse_report()
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
    report: &'a CheckReport,
    active: &'a ActiveDatabaseRevision,
    identities: IdentityMap,
    source: PreparedSource,
    catalogue_revision: CatalogueRevisionId,
    origins: Vec<DefinitionOrigin>,
    expressions: Vec<ExpressionArtifact>,
    functions: Vec<FunctionDefinition>,
    current_function_revisions: Vec<FunctionRevisionRecord>,
    new_function_revisions: Vec<FunctionRevisionRecord>,
    references: Vec<DefinitionReference>,
}

impl<'a> CandidateBuilder<'a> {
    fn new(
        report: &'a CheckReport,
        checked: &'a CheckedBundle,
        active: &'a ActiveDatabaseRevision,
        identities: IdentityMap,
        source: PreparedSource,
    ) -> Self {
        Self {
            checked,
            report,
            active,
            identities,
            source,
            catalogue_revision: CatalogueRevisionId::new(),
            origins: Vec::new(),
            expressions: Vec::new(),
            functions: Vec::new(),
            current_function_revisions: Vec::new(),
            new_function_revisions: Vec::new(),
            references: Vec::new(),
        }
    }

    fn build(mut self) -> Result<DeployableRevision, PrepareError> {
        let schemas = self.build_schemas()?;
        let object_types = self.build_object_types()?;
        self.build_functions(&object_types)?;

        let catalogue = CatalogueSnapshot::new_with_functions(
            self.catalogue_revision,
            schemas,
            object_types,
            self.functions,
        )?;
        let catalogue_hash = catalogue_digest(
            &catalogue,
            &self.current_function_revisions,
            &self.expressions,
            &self.origins,
            &self.references,
        )?;

        Ok(DeployableRevision::new(
            self.active.pair(),
            self.source.revision,
            self.active.pair().catalogue(),
            catalogue,
            catalogue_hash,
            self.origins,
            self.expressions,
            self.new_function_revisions,
            self.references,
        )?)
    }

    fn build_schemas(&mut self) -> Result<Vec<SchemaDefinition>, PrepareError> {
        let mut schemas = Vec::with_capacity(self.checked.schemas().len());
        for checked in self.checked.schemas() {
            let id = self.identities.schema(checked.id())?;
            schemas.push(SchemaDefinition::new(id, checked.name().clone()));
            self.push_origin(DefinitionIdentity::Schema(id), checked.location())?;
        }
        Ok(schemas)
    }

    fn build_object_types(&mut self) -> Result<Vec<ObjectTypeDefinition>, PrepareError> {
        let mut object_types = Vec::with_capacity(self.checked.object_types().len());
        for checked_type in self.checked.object_types() {
            let type_id = self.identities.type_id(checked_type.id())?;
            let mut fields = Vec::with_capacity(checked_type.fields().len());
            for checked_field in checked_type.fields() {
                let field_id = self.identities.field(checked_field.id())?;
                let default_expression = if let Some(default) = checked_field.default() {
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
                    Some(expression_id)
                } else {
                    None
                };

                fields.push(FieldDefinition::new(
                    field_id,
                    checked_field.name(),
                    checked_field.ordinal(),
                    self.identities
                        .resolved_type(checked_field.semantic_type())?,
                    checked_field.nullable(),
                    checked_field.unique(),
                    default_expression,
                    checked_field.on_delete(),
                ));
                self.push_origin(
                    DefinitionIdentity::Field {
                        owner: type_id,
                        field: field_id,
                    },
                    checked_field.location(),
                )?;
            }
            object_types.push(ObjectTypeDefinition::new(
                type_id,
                checked_type.name().clone(),
                fields,
            ));
            self.push_origin(
                DefinitionIdentity::ObjectType(type_id),
                checked_type.location(),
            )?;
        }
        Ok(object_types)
    }

    fn build_functions(
        &mut self,
        object_types: &[ObjectTypeDefinition],
    ) -> Result<(), PrepareError> {
        for checked in self.checked.server_functions() {
            let function_id = self.identities.function(checked.id())?;
            let initial_revision = match checked.id() {
                CheckedFunctionId::Existing(_) => self
                    .active
                    .catalogue()
                    .function_by_id(function_id)
                    .ok_or(existing_mismatch(DefinitionIdentity::Function(function_id)))?
                    .current_revision(),
                CheckedFunctionId::Provisional(_) => FunctionRevisionId::new(),
            };
            let initial_definition = self.function_definition(checked, initial_revision)?;
            let prepared_artifact =
                self.server_artifact(checked, &initial_definition, object_types)?;
            let initial_references =
                self.function_references(checked, function_id, initial_revision)?;
            let semantic_hash = function_semantic_digest(
                &initial_definition,
                prepared_artifact.language_version,
                &prepared_artifact.artifact,
                &self.expressions,
                &initial_references,
            )?;

            let reusable = self
                .active
                .function_revisions()
                .iter()
                .chain(self.active.historical_function_revisions())
                .filter(|revision| {
                    revision.function() == function_id && revision.semantic_hash() == semantic_hash
                })
                .min_by_key(|revision| (revision.revision_number(), revision.id().to_bytes()))
                .cloned();

            let (revision_id, current_revision) = if let Some(revision) = reusable {
                (revision.id(), revision)
            } else {
                let revision_id = match checked.id() {
                    CheckedFunctionId::Existing(_) => FunctionRevisionId::new(),
                    CheckedFunctionId::Provisional(_) => initial_revision,
                };
                let revision_number = self.next_revision_number(function_id)?;
                let declaration_origin = self.source.origin(checked.location())?;
                let declaration = self.source.declaration(self.report, checked.location())?;
                let revision = FunctionRevisionRecord::new(
                    function_id,
                    revision_id,
                    revision_number,
                    declaration_origin,
                    function_declaration_digest(declaration)?,
                    semantic_hash,
                    prepared_artifact.language_version,
                    prepared_artifact.artifact,
                )?;
                self.new_function_revisions.push(revision.clone());
                (revision_id, revision)
            };

            let definition = self.function_definition(checked, revision_id)?;
            let references = self.function_references(checked, function_id, revision_id)?;
            self.push_function_origins(checked, function_id)?;
            self.functions.push(definition);
            self.current_function_revisions.push(current_revision);
            self.references.extend(references);
        }
        Ok(())
    }

    fn function_definition(
        &self,
        checked: &crate::CheckedServerFunction,
        current_revision: FunctionRevisionId,
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
                    self.identities.resolved_type(parameter.semantic_type())?,
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
                    self.identities.resolved_type(column.semantic_type())?,
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
    ) -> Result<PreparedServerArtifact, PrepareError> {
        if let Some(checked_plan) = checked.query_plan() {
            let plan = checked_plan.try_map_identities(
                |id| self.identities.type_id(id),
                |id| self.identities.field(id),
            )?;
            let payload = plan.encode_server_plan()?;
            let hash = artifact_payload_digest(&payload)?;
            return Ok(PreparedServerArtifact {
                artifact: ExecutableArtifact::new(
                    ExecutableArtifactKind::Server,
                    SERVER_PLAN_FORMAT,
                    SERVER_PLAN_VERSION,
                    payload,
                    hash,
                )?,
                language_version: SERVER_PLAN_LANGUAGE_VERSION,
            });
        }

        let references = checked
            .references()
            .iter()
            .map(|reference| {
                Ok((
                    reference.kind(),
                    self.identities.reference_target(reference.target())?,
                ))
            })
            .collect::<Result<Vec<_>, PrepareError>>()?;

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
        Ok(PreparedServerArtifact {
            artifact: ExecutableArtifact::new(
                ExecutableArtifactKind::Server,
                SERVER_MUTATION_PLAN_FORMAT,
                format_version,
                payload,
                hash,
            )?,
            language_version: SERVER_MUTATION_PLAN_LANGUAGE_VERSION,
        })
    }

    fn function_references(
        &self,
        checked: &crate::CheckedServerFunction,
        function: FunctionId,
        revision: FunctionRevisionId,
    ) -> Result<Vec<DefinitionReference>, PrepareError> {
        checked
            .references()
            .iter()
            .enumerate()
            .map(|(ordinal, reference)| {
                Ok(DefinitionReference::new(
                    function,
                    revision,
                    u32::try_from(ordinal).map_err(|_| PrepareError::ReferenceCountExceedsU32 {
                        function: checked.id(),
                        count: checked.references().len(),
                    })?,
                    self.identities.reference_target(reference.target())?,
                    reference.kind(),
                    self.source.origin(reference.location())?,
                ))
            })
            .collect()
    }

    fn next_revision_number(&self, function: FunctionId) -> Result<u64, PrepareError> {
        self.active
            .function_revisions()
            .iter()
            .chain(self.active.historical_function_revisions())
            .filter(|revision| revision.function() == function)
            .map(FunctionRevisionRecord::revision_number)
            .max()
            .map_or(Ok(1), |maximum| {
                maximum
                    .checked_add(1)
                    .ok_or(PrepareError::FunctionRevisionNumberExhausted { function })
            })
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

#[cfg(test)]
mod tests {
    use orna_artifact::{
        constant_expression::ConstantExpression,
        server_mutation_plan::{
            MutationExpressionKind as DurableMutationExpressionKind, ServerDeletePlan,
            ServerMutationOperation, ServerMutationPlan,
        },
        server_plan::{ExpressionKind, ServerPlan},
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
        assert!(validate_mutation_reference_sequence(&expected, &expected).is_ok());
        let mut reordered = expected.clone();
        reordered.reverse();
        assert!(validate_mutation_reference_sequence(&expected, &reordered).is_err());
        assert!(validate_mutation_reference_sequence(&expected, &expected[..1]).is_err());
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

    fn checked_report(source: &str, base: &CatalogueSnapshot) -> CheckReport {
        let bundle = SourceBundle::new([SourceUnit::new("tasks.orna", source)]).unwrap();
        check(&bundle, base)
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
