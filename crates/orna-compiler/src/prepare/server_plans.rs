use super::*;

pub(super) fn server_mutation_plan(
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
pub(super) fn query_planning_function(function: &FunctionDefinition) -> FunctionDefinition {
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

pub(super) fn identity_selected_query_plan(
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

pub(super) fn unique_text_selected_query_plan(
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

pub(super) fn version_one_query_plan(
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

pub(super) fn distinct_query_plan(
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

pub(super) fn identity_selected_query_reference_sequence(
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

pub(super) fn distinct_query_reference_sequence(
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

pub(super) fn version_one_query_reference_sequence(
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

pub(super) fn server_delete_plan(
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

pub(super) fn validate_mutation_selector(
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

pub(super) fn validate_mutation_parameters(
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
pub(super) fn validate_mutation_assignments(
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

pub(super) fn server_mutation_expression(
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

pub(super) fn delete_reference_sequence(
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

pub(super) fn signature_reference_sequence(
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

pub(super) fn validate_reference_sequence(
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

pub(super) fn is_sealed_inspect_type_id(id: TypeId) -> bool {
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
