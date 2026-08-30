use super::*;

pub(super) fn check_server_functions(
    inputs: &[ResolvedServerFunctionInput<'_>],
    catalogue: &ResolutionCatalogue<CheckedTypeId, CheckedFieldId>,
    record_value_types: &[CheckedRecordValueType],
    enum_types: &[CheckedEnumType],
    diagnostics: &mut Vec<CompilerDiagnostic>,
    standard: Option<&CheckedStandardLibrary>,
    uses: &mut Vec<CheckedApplicationTypeUse>,
) -> Vec<CheckedServerFunction> {
    let diagnostics_before = diagnostics.len();
    let mut functions = Vec::with_capacity(inputs.len());
    let intrinsic_boolean = intrinsic_boolean_type(standard);
    let mutation_catalogue = RecordAwareMutationCatalogue {
        objects: catalogue,
        record_value_types,
        enum_types,
        standard_field_types: uses
            .iter()
            .filter_map(|type_use| {
                let CheckedTypeUseKind::Field { owner, field } = type_use.kind() else {
                    return None;
                };
                type_use
                    .value()
                    .map(|value| ((owner, field), value.type_id()))
            })
            .collect(),
    };

    for input in inputs {
        let body_name = if input.body.as_sql_query().is_some() {
            "SELECT"
        } else if input.body.as_sql_insert().is_some() {
            "INSERT"
        } else if input.body.as_sql_update().is_some() {
            "UPDATE"
        } else if input.body.as_sql_delete().is_some() {
            "DELETE"
        } else {
            diagnostics.push(DiagnosticCode::semantic(
                DiagnosticCode::DomainIncompatible,
                "SERVER functions do not yet support this body form",
                input.location.clone(),
            ));
            continue;
        };
        let return_location = match &input.return_type {
            ResolvedServerFunctionReturn::Single { location, .. }
            | ResolvedServerFunctionReturn::Stream { location, .. }
            | ResolvedServerFunctionReturn::Rows { location, .. } => location,
        };
        let columns: &[ResolvedServerFunctionReturnColumn] = match &input.return_type {
            ResolvedServerFunctionReturn::Rows { columns, .. } => columns,
            ResolvedServerFunctionReturn::Stream { .. } if body_name == "SELECT" => &[],
            ResolvedServerFunctionReturn::Stream { .. } => {
                diagnostics.push(DiagnosticCode::semantic(
                    DiagnosticCode::TypeMismatch,
                    "STREAM SERVER functions require a SELECT body",
                    return_location.clone(),
                ));
                continue;
            }
            ResolvedServerFunctionReturn::Single {
                semantic_type: SemanticType::Scalar(_),
                ..
            } if body_name == "SELECT" => &[],
            ResolvedServerFunctionReturn::Single { location, .. } => {
                diagnostics.push(DiagnosticCode::semantic(
                    DiagnosticCode::TypeMismatch,
                    if body_name == "SELECT" {
                        "SERVER SELECT functions with scalar returns require a scalar projection"
                    } else if body_name == "DELETE" {
                        "DELETE SERVER functions require RETURNS ROWS (...)"
                    } else {
                        "SERVER functions require RETURNS ROWS (...)"
                    },
                    location.clone(),
                ));
                continue;
            }
        };

        let (body, body_references) = if let Some(query_body) = input.body.as_sql_query() {
            match &query_body.query.quantifier {
                SelectQuantifier::Distinct { .. } => {
                    let query_check = match check_distinct_query_with_intrinsic_boolean_in(
                        &query_body.query,
                        catalogue,
                        input.location.logical_path(),
                        intrinsic_boolean,
                    ) {
                        Ok(query_check) => query_check,
                        Err(query_diagnostics) => {
                            diagnostics.extend(query_diagnostics);
                            continue;
                        }
                    };
                    if !query_return_matches(
                        query_check.plan().projections(),
                        &input.return_type,
                        return_location,
                        diagnostics,
                    ) {
                        continue;
                    }
                    if !distinct_query_execution_shape_is_valid(input, diagnostics) {
                        continue;
                    }
                    let mut recorder = StandardTypeUseRecorder::new(
                        uses,
                        standard,
                        input.id,
                        input.location.logical_path(),
                    );
                    recorder.record_query_body(
                        &query_body.query,
                        query_check.plan().projections(),
                        query_check.plan().selection(),
                        &[],
                        &[],
                    );
                    (
                        CheckedServerFunctionBody::DistinctQuery(query_check.plan().clone()),
                        query_check
                            .references()
                            .iter()
                            .map(query_reference)
                            .collect::<Vec<_>>(),
                    )
                }
                SelectQuantifier::All => {
                    let has_selector = matches!(
                        &query_body.query.predicate,
                        Some(orna_syntax::QueryExpression::Equality { right, .. })
                            if matches!(right.as_ref(), orna_syntax::QueryExpression::ParameterRead { .. })
                    );
                    let has_unique_text_selector_shape = matches!(
                        &query_body.query.predicate,
                        Some(orna_syntax::QueryExpression::Equality { left, right, .. })
                            if matches!(
                                left.as_ref(),
                                orna_syntax::QueryExpression::FieldPath { members, .. } if members.len() == 1
                            ) && matches!(right.as_ref(), orna_syntax::QueryExpression::ParameterRead { .. })
                    );
                    if input.parameters.is_empty() && !has_selector {
                        let query_check = match check_query_with_intrinsic_boolean_in(
                            &query_body.query,
                            catalogue,
                            input.location.logical_path(),
                            intrinsic_boolean,
                        ) {
                            Ok(query_check) => query_check,
                            Err(query_diagnostics) => {
                                diagnostics.extend(query_diagnostics);
                                continue;
                            }
                        };
                        if !query_return_matches(
                            query_check.plan().projections(),
                            &input.return_type,
                            return_location,
                            diagnostics,
                        ) {
                            continue;
                        }
                        let mut recorder = StandardTypeUseRecorder::new(
                            uses,
                            standard,
                            input.id,
                            input.location.logical_path(),
                        );
                        recorder.record_query_body(
                            &query_body.query,
                            query_check.plan().projections(),
                            query_check.plan().selection(),
                            &query_body.query.ordering,
                            query_check.plan().ordering(),
                        );
                        (
                            CheckedServerFunctionBody::Query(query_check.plan().clone()),
                            query_check
                                .references()
                                .iter()
                                .map(query_reference)
                                .collect::<Vec<_>>(),
                        )
                    } else if has_unique_text_selector_shape {
                        if !identity_selected_query_execution_mode_is_valid(input, diagnostics) {
                            continue;
                        }
                        let parameters = unique_text_selected_query_parameters(input);
                        let query_check =
                            match check_unique_text_selected_query_with_intrinsic_boolean_in(
                                &query_body.query,
                                catalogue,
                                input.id,
                                &parameters,
                                input.location.logical_path(),
                                intrinsic_boolean,
                            ) {
                                Ok(query_check) => query_check,
                                Err(query_diagnostics) => {
                                    diagnostics.extend(query_diagnostics);
                                    continue;
                                }
                            };
                        if !query_return_matches(
                            query_check.plan().projections(),
                            &input.return_type,
                            return_location,
                            diagnostics,
                        ) {
                            continue;
                        }
                        let mut recorder = StandardTypeUseRecorder::new(
                            uses,
                            standard,
                            input.id,
                            input.location.logical_path(),
                        );
                        recorder.record_query_body(
                            &query_body.query,
                            query_check.plan().projections(),
                            None,
                            &[],
                            &[],
                        );
                        recorder.record_unique_text_selector(
                            &query_body.query,
                            intrinsic_boolean_id(intrinsic_boolean),
                            query_check
                                .plan()
                                .selector()
                                .text_type()
                                .standard_value_type(),
                        );
                        (
                            CheckedServerFunctionBody::UniqueTextSelectedQuery(
                                query_check.plan().clone(),
                            ),
                            query_check
                                .references()
                                .iter()
                                .map(unique_text_selected_query_reference)
                                .collect::<Vec<_>>(),
                        )
                    } else {
                        if !identity_selected_query_execution_mode_is_valid(input, diagnostics) {
                            continue;
                        }
                        let parameters = identity_selected_query_parameters(input);
                        let query_check =
                            match check_identity_selected_query_with_intrinsic_boolean_in(
                                &query_body.query,
                                catalogue,
                                input.id,
                                &parameters,
                                input.location.logical_path(),
                                intrinsic_boolean,
                            ) {
                                Ok(query_check) => query_check,
                                Err(query_diagnostics) => {
                                    diagnostics.extend(query_diagnostics);
                                    continue;
                                }
                            };
                        if !query_return_matches(
                            query_check.plan().projections(),
                            &input.return_type,
                            return_location,
                            diagnostics,
                        ) {
                            continue;
                        }
                        let mut recorder = StandardTypeUseRecorder::new(
                            uses,
                            standard,
                            input.id,
                            input.location.logical_path(),
                        );
                        recorder.record_query_body(
                            &query_body.query,
                            query_check.plan().projections(),
                            None,
                            &[],
                            &[],
                        );
                        recorder.record_identity_selector(
                            &query_body.query,
                            query_check.plan().scan().object_type(),
                            intrinsic_boolean_id(intrinsic_boolean),
                        );
                        (
                            CheckedServerFunctionBody::IdentitySelectedQuery(
                                query_check.plan().clone(),
                            ),
                            query_check
                                .references()
                                .iter()
                                .map(identity_selected_query_reference)
                                .collect::<Vec<_>>(),
                        )
                    }
                }
                _ => {
                    diagnostics.push(DiagnosticCode::semantic(
                        DiagnosticCode::DomainIncompatible,
                        "this SELECT form is not available yet",
                        location(input.location.logical_path(), &query_body.query.span),
                    ));
                    continue;
                }
            }
        } else if let Some(delete_body) = input.body.as_sql_delete() {
            if !mutation_execution_mode_is_valid(input, "DELETE", diagnostics) {
                continue;
            }
            if columns.len() != 1 {
                diagnostics.push(DiagnosticCode::semantic(
                    DiagnosticCode::TypeMismatch,
                    "A DELETE SERVER function must declare exactly one column in RETURNS ROWS (...)" ,
                    return_location.clone(),
                ));
                continue;
            }
            let column = &columns[0];
            if column.semantic_type != SemanticType::Scalar(StandardScalar::Boolean)
                && !matches!(intrinsic_boolean, IntrinsicBooleanType::Missing)
            {
                diagnostics.push(DiagnosticCode::semantic(
                    DiagnosticCode::TypeMismatch,
                    "The RETURNS ROWS (...) column for a DELETE SERVER function must use BOOLEAN",
                    column.location.clone(),
                ));
                continue;
            }
            let parameters = mutation_parameters(input);
            let delete_check = match if standard.is_some() {
                check_delete_with_intrinsic_boolean_in(
                    &delete_body.delete,
                    catalogue,
                    input.id,
                    &parameters,
                    input.location.logical_path(),
                    intrinsic_boolean,
                )
            } else {
                check_delete_in(
                    &delete_body.delete,
                    catalogue,
                    input.id,
                    &parameters,
                    input.location.logical_path(),
                )
            } {
                Ok(delete_check) => delete_check,
                Err(delete_diagnostics) => {
                    diagnostics.extend(delete_diagnostics);
                    continue;
                }
            };
            let mut type_uses = StandardTypeUseRecorder::new(
                uses,
                standard,
                input.id,
                input.location.logical_path(),
            );
            type_uses.record_delete(
                &delete_body.delete,
                &delete_check,
                intrinsic_boolean_id(intrinsic_boolean),
            );
            (
                CheckedServerFunctionBody::Delete(delete_check.plan().clone()),
                delete_check
                    .references()
                    .iter()
                    .map(mutation_reference)
                    .collect(),
            )
        } else if input.body.as_sql_insert().is_some() || input.body.as_sql_update().is_some() {
            let mutation_name = if input.body.as_sql_insert().is_some() {
                "INSERT"
            } else {
                "UPDATE"
            };
            if !mutation_execution_mode_is_valid(input, mutation_name, diagnostics) {
                continue;
            }
            if columns.len() != 1 {
                diagnostics.push(DiagnosticCode::semantic(
                    DiagnosticCode::TypeMismatch,
                    format!(
                        "An {mutation_name} SERVER function must declare exactly one column in RETURNS ROWS (...)"
                    ),
                    return_location.clone(),
                ));
                continue;
            }
            let column = &columns[0];
            let SemanticType::Reference {
                target: declared_target,
            } = column.semantic_type
            else {
                diagnostics.push(DiagnosticCode::semantic(
                    DiagnosticCode::TypeMismatch,
                    format!(
                        "The RETURNS ROWS (...) column for an {mutation_name} SERVER function must use REF"
                    ),
                    column.location.clone(),
                ));
                continue;
            };
            let parameters = mutation_parameters(input);
            let checked_mutation = if let Some(insert_body) = input.body.as_sql_insert() {
                if standard.is_some() {
                    check_insert_with_intrinsic_boolean_in(
                        &insert_body.insert,
                        &mutation_catalogue,
                        input.id,
                        &parameters,
                        input.location.logical_path(),
                        intrinsic_boolean,
                    )
                } else {
                    check_insert_in(
                        &insert_body.insert,
                        &mutation_catalogue,
                        input.id,
                        &parameters,
                        input.location.logical_path(),
                    )
                }
            } else if let Some(update_body) = input.body.as_sql_update() {
                if standard.is_some() {
                    check_update_with_intrinsic_boolean_in(
                        &update_body.update,
                        catalogue,
                        input.id,
                        &parameters,
                        input.location.logical_path(),
                        intrinsic_boolean,
                    )
                } else {
                    check_update_in(
                        &update_body.update,
                        catalogue,
                        input.id,
                        &parameters,
                        input.location.logical_path(),
                    )
                }
            } else {
                continue;
            };
            let mutation_check = match checked_mutation {
                Ok(mutation_check) => mutation_check,
                Err(mutation_diagnostics) => {
                    diagnostics.extend(mutation_diagnostics);
                    continue;
                }
            };
            let mutation_plan = mutation_check.plan();
            if declared_target != mutation_plan.returned_object() {
                diagnostics.push(DiagnosticCode::semantic(
                    DiagnosticCode::TypeMismatch,
                    format!(
                        "The returned REF must point to the object type being {}",
                        if mutation_name == "INSERT" {
                            "inserted"
                        } else {
                            "updated"
                        }
                    ),
                    column
                        .reference_location
                        .clone()
                        .unwrap_or_else(|| column.location.clone()),
                ));
                continue;
            }
            let mut type_uses = StandardTypeUseRecorder::new(
                uses,
                standard,
                input.id,
                input.location.logical_path(),
            );
            if let Some(insert_body) = input.body.as_sql_insert() {
                type_uses.record_insert(&insert_body.insert, &mutation_check);
            } else if let Some(update_body) = input.body.as_sql_update() {
                type_uses.record_update(
                    &update_body.update,
                    &mutation_check,
                    intrinsic_boolean_id(intrinsic_boolean),
                );
            }
            (
                CheckedServerFunctionBody::Mutation(mutation_plan.clone()),
                mutation_check
                    .references()
                    .iter()
                    .map(mutation_reference)
                    .collect(),
            )
        } else {
            diagnostics.push(DiagnosticCode::semantic(
                DiagnosticCode::DomainIncompatible,
                "SERVER functions do not yet support this body form",
                input.location.clone(),
            ));
            continue;
        };

        let mut references = signature_references(&input.parameters, &input.return_type);
        references.extend(body_references);
        functions.push(checked_server_function(input, body, references));
    }

    if diagnostics.len() != diagnostics_before {
        return Vec::new();
    }

    functions
}

fn mutation_execution_mode_is_valid(
    input: &ResolvedServerFunctionInput<'_>,
    mutation_name: &str,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) -> bool {
    let mut valid = true;
    if input.security != CatalogueFunctionSecurity::Invoker {
        diagnostics.push(DiagnosticCode::semantic(
            DiagnosticCode::DomainIncompatible,
            format!("{mutation_name} SERVER functions require SECURITY INVOKER"),
            input.location.clone(),
        ));
        valid = false;
    }
    if input.transaction != Some(CatalogueFunctionTransaction::Atomic) {
        diagnostics.push(DiagnosticCode::semantic(
            DiagnosticCode::DomainIncompatible,
            format!("{mutation_name} SERVER functions require TRANSACTION ATOMIC"),
            input.location.clone(),
        ));
        valid = false;
    }
    if input.volatility != CatalogueFunctionVolatility::Volatile {
        diagnostics.push(DiagnosticCode::semantic(
            DiagnosticCode::DomainIncompatible,
            format!("{mutation_name} SERVER functions require VOLATILITY VOLATILE"),
            input.location.clone(),
        ));
        valid = false;
    }
    valid
}

fn distinct_query_execution_shape_is_valid(
    input: &ResolvedServerFunctionInput<'_>,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) -> bool {
    let mut valid = true;
    if !input.parameters.is_empty() {
        diagnostics.push(DiagnosticCode::semantic(
            DiagnosticCode::DomainIncompatible,
            "SELECT DISTINCT SERVER functions require zero declared parameters",
            input.location.clone(),
        ));
        valid = false;
    }
    if input.security != CatalogueFunctionSecurity::Invoker {
        diagnostics.push(DiagnosticCode::semantic(
            DiagnosticCode::DomainIncompatible,
            "SELECT DISTINCT SERVER functions require SECURITY INVOKER",
            input.location.clone(),
        ));
        valid = false;
    }
    if input.transaction != Some(CatalogueFunctionTransaction::ReadOnly) {
        diagnostics.push(DiagnosticCode::semantic(
            DiagnosticCode::DomainIncompatible,
            "SELECT DISTINCT SERVER functions require TRANSACTION READ ONLY",
            input.location.clone(),
        ));
        valid = false;
    }
    if input.volatility != CatalogueFunctionVolatility::Stable {
        diagnostics.push(DiagnosticCode::semantic(
            DiagnosticCode::DomainIncompatible,
            "SELECT DISTINCT SERVER functions require VOLATILITY STABLE",
            input.location.clone(),
        ));
        valid = false;
    }
    valid
}

fn identity_selected_query_execution_mode_is_valid(
    input: &ResolvedServerFunctionInput<'_>,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) -> bool {
    let mut valid = true;
    if input.security != CatalogueFunctionSecurity::Invoker {
        diagnostics.push(DiagnosticCode::semantic(
            DiagnosticCode::DomainIncompatible,
            "parameterised SELECT SERVER functions require SECURITY INVOKER",
            input.location.clone(),
        ));
        valid = false;
    }
    if input.transaction != Some(CatalogueFunctionTransaction::ReadOnly) {
        diagnostics.push(DiagnosticCode::semantic(
            DiagnosticCode::DomainIncompatible,
            "parameterised SELECT SERVER functions require TRANSACTION READ ONLY",
            input.location.clone(),
        ));
        valid = false;
    }
    if input.volatility != CatalogueFunctionVolatility::Stable {
        diagnostics.push(DiagnosticCode::semantic(
            DiagnosticCode::DomainIncompatible,
            "parameterised SELECT SERVER functions require VOLATILITY STABLE",
            input.location.clone(),
        ));
        valid = false;
    }
    valid
}

fn identity_selected_query_parameters(
    input: &ResolvedServerFunctionInput<'_>,
) -> Vec<QueryParameter<CheckedTypeId, CheckedParameterId>> {
    input
        .parameters
        .iter()
        .map(|parameter| {
            QueryParameter::new(
                parameter.name.clone(),
                parameter.id,
                parameter.semantic_type,
            )
        })
        .collect()
}

fn unique_text_selected_query_parameters(
    input: &ResolvedServerFunctionInput<'_>,
) -> Vec<QueryParameter<CheckedTypeId, CheckedParameterId>> {
    input
        .parameters
        .iter()
        .map(|parameter| {
            let query_parameter = QueryParameter::new(
                parameter.name.clone(),
                parameter.id,
                parameter.semantic_type,
            )
            .with_required_non_null();
            if let Some(type_id) = parameter.standard_value_type {
                query_parameter.with_standard_value_type(type_id)
            } else {
                query_parameter
            }
        })
        .collect()
}

fn query_return_matches(
    projections: &[ExpressionIr<CheckedTypeId, CheckedFieldId>],
    return_type: &ResolvedServerFunctionReturn,
    return_location: &SourceLocation,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) -> bool {
    match return_type {
        ResolvedServerFunctionReturn::Rows { columns, .. } => {
            if projections.len() != columns.len() {
                diagnostics.push(DiagnosticCode::semantic(
                    DiagnosticCode::TypeMismatch,
                    format!(
                        "SELECT returns {} {}, but RETURNS ROWS (...) declares {} {}",
                        projections.len(),
                        if projections.len() == 1 {
                            "column"
                        } else {
                            "columns"
                        },
                        columns.len(),
                        if columns.len() == 1 {
                            "column"
                        } else {
                            "columns"
                        }
                    ),
                    return_location.clone(),
                ));
                return false;
            }

            let mut matches_return = true;
            for (projection, column) in projections.iter().zip(columns) {
                if projection.value_type().semantic_type() != column.semantic_type {
                    diagnostics.push(DiagnosticCode::semantic(
                        DiagnosticCode::TypeMismatch,
                        format!(
                            "SELECT column {} does not have the same type as RETURNS ROWS column {}",
                            column.ordinal + 1,
                            column.name
                        ),
                        column.location.clone(),
                    ));
                    matches_return = false;
                }
            }
            matches_return
        }
        ResolvedServerFunctionReturn::Stream { semantic_type, .. } => {
            if projections.len() != 1 {
                diagnostics.push(DiagnosticCode::semantic(
                    DiagnosticCode::TypeMismatch,
                    format!(
                        "SELECT returns {} {}, but RETURNS STREAM<T> declares one element",
                        projections.len(),
                        if projections.len() == 1 {
                            "column"
                        } else {
                            "columns"
                        }
                    ),
                    return_location.clone(),
                ));
                return false;
            }
            if projections[0].value_type().semantic_type() != *semantic_type {
                diagnostics.push(DiagnosticCode::semantic(
                    DiagnosticCode::TypeMismatch,
                    "SELECT column 1 does not have the same type as RETURNS STREAM<T> element",
                    return_location.clone(),
                ));
                return false;
            }
            true
        }
        ResolvedServerFunctionReturn::Single { semantic_type, .. } => {
            if projections.len() != 1 {
                diagnostics.push(DiagnosticCode::semantic(
                    DiagnosticCode::TypeMismatch,
                    format!(
                        "SELECT returns {} {}, but RETURNS scalar declares one column",
                        projections.len(),
                        if projections.len() == 1 {
                            "column"
                        } else {
                            "columns"
                        }
                    ),
                    return_location.clone(),
                ));
                return false;
            }
            if projections[0].value_type().semantic_type() != *semantic_type {
                diagnostics.push(DiagnosticCode::semantic(
                    DiagnosticCode::TypeMismatch,
                    "SELECT column 1 does not have the same type as RETURNS scalar",
                    return_location.clone(),
                ));
                return false;
            }
            true
        }
    }
}

fn mutation_parameters(
    input: &ResolvedServerFunctionInput<'_>,
) -> Vec<MutationParameter<CheckedTypeId, CheckedParameterId>> {
    input
        .parameters
        .iter()
        .map(|source_parameter| {
            let parameter = MutationParameter::new(
                source_parameter.name.clone(),
                source_parameter.id,
                source_parameter.semantic_type,
                source_parameter.name_span.clone(),
            );
            if let Some(type_id) = source_parameter.standard_value_type {
                parameter.with_standard_value_type(type_id)
            } else {
                parameter
            }
        })
        .collect()
}

fn checked_server_function(
    input: &ResolvedServerFunctionInput<'_>,
    body: CheckedServerFunctionBody,
    references: Vec<CheckedDefinitionReference>,
) -> CheckedServerFunction {
    CheckedServerFunction {
        id: input.id,
        name: input.name.clone(),
        parameters: input
            .parameters
            .iter()
            .cloned()
            .map(|parameter| CheckedServerFunctionParameter {
                id: parameter.id,
                name: parameter.name,
                ordinal: parameter.ordinal,
                semantic_type: parameter.semantic_type,
                location: parameter.location,
            })
            .collect(),
        return_type: match &input.return_type {
            ResolvedServerFunctionReturn::Single {
                semantic_type,
                standard_value_type,
                location,
            } => CheckedServerFunctionReturn::Single {
                semantic_type: *semantic_type,
                standard_value_type: *standard_value_type,
                location: location.clone(),
            },
            ResolvedServerFunctionReturn::Rows { columns, .. } => {
                CheckedServerFunctionReturn::Rows(
                    columns
                        .iter()
                        .cloned()
                        .map(|column| CheckedServerFunctionReturnColumn {
                            name: column.name,
                            ordinal: column.ordinal,
                            semantic_type: column.semantic_type,
                            location: column.location,
                        })
                        .collect(),
                )
            }
            ResolvedServerFunctionReturn::Stream {
                semantic_type,
                standard_value_type,
                location,
                ..
            } => CheckedServerFunctionReturn::Stream {
                semantic_type: *semantic_type,
                standard_value_type: *standard_value_type,
                location: location.clone(),
            },
        },
        security: input.security,
        transaction: input.transaction,
        volatility: input.volatility,
        location: input.location.clone(),
        body,
        references,
    }
}
