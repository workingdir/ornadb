//! Semantic resolution for parsed source bundles.
//!
//! The resolver consumes the `Parse` values retained by [`super::parse_bundle`].
//! It does not parse source text or expose syntax implementation values.

mod identity;
mod model;

pub use identity::{
    CheckedExpressionId, CheckedFieldId, CheckedFunctionId, CheckedParameterId, CheckedSchemaId,
    CheckedTypeId, ProvisionalExpressionId, ProvisionalFieldId, ProvisionalFunctionId,
    ProvisionalParameterId, ProvisionalSchemaId, ProvisionalTypeId,
};
pub use model::{
    CheckReport, CheckedBundle, CheckedDefault, CheckedDefinitionReference,
    CheckedDefinitionReferenceTarget, CheckedField, CheckedObjectType, CheckedSchema,
    CheckedServerFunction, CheckedServerFunctionParameter, CheckedServerFunctionReturnColumn,
    ConstantValue, SemanticType,
};
pub(crate) use model::{QueryCatalogue, QueryField};

use std::collections::{HashMap, HashSet};

use orna_core::{
    ExpressionId,
    catalogue::{
        CatalogueSnapshot, FunctionSecurity as CatalogueFunctionSecurity,
        FunctionTransaction as CatalogueFunctionTransaction,
        FunctionVolatility as CatalogueFunctionVolatility, OnDeleteAction, QualifiedSemanticName,
    },
    revision::DefinitionReferenceKind,
    source::SourceBundle,
    types::StandardScalar,
};
use orna_syntax::{
    FunctionReturnType, FunctionSecurity as SyntaxFunctionSecurity,
    FunctionTransaction as SyntaxFunctionTransaction,
    FunctionVolatility as SyntaxFunctionVolatility, ObjectTypeDeclaration, OnDeletePolicy,
    QualifiedName, ServerFunctionBody, ServerFunctionDeclaration, SourceSlice, SourceSpan,
    StandardLargeObjectKind, TypeSpecification,
};

use crate::relational::{QueryReference, QueryReferenceKind, QueryReferenceTarget, check_query_in};
use crate::{
    CompilerDiagnostic, DiagnosticCode, ParseReport, SourceLocation,
    normalise_name_part as semantic_part, normalise_qualified_name as semantic_name, parse_bundle,
};

use self::{
    identity::{CheckAssignments, IdentityAssignments},
    model::{QueryObjectType, ResolutionCatalogue},
};

/// Checks one source bundle against an immutable catalogue snapshot.
///
/// This function parses the bundle exactly once. Resolution consumes the owned
/// `Parse` values that [`parse_bundle`] retains in the resulting report.
pub fn check(bundle: &SourceBundle, base: &CatalogueSnapshot) -> CheckReport {
    check_parsed(parse_bundle(bundle), base)
}

#[derive(Clone, Copy)]
struct Header<'a> {
    declaration: &'a ObjectTypeDeclaration,
    logical_path: &'a str,
    id: CheckedTypeId,
}

/// Resolved metadata for a SERVER function before relational planning.
#[derive(Clone, Copy)]
struct ServerFunctionHeader<'a> {
    declaration: &'a ServerFunctionDeclaration,
    logical_path: &'a str,
    id: CheckedFunctionId,
    security: CatalogueFunctionSecurity,
    transaction: Option<CatalogueFunctionTransaction>,
    volatility: CatalogueFunctionVolatility,
}

/// One resolved parameter accepted by this SERVER function slice.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ResolvedServerFunctionParameter {
    id: CheckedParameterId,
    name: String,
    ordinal: u32,
    semantic_type: SemanticType<CheckedTypeId>,
    location: SourceLocation,
    reference_location: Option<SourceLocation>,
}

/// One resolved column in a `ROWS` result.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ResolvedServerFunctionReturnColumn {
    name: String,
    ordinal: u32,
    semantic_type: SemanticType<CheckedTypeId>,
    location: SourceLocation,
    reference_location: Option<SourceLocation>,
}

/// The resolved result shape before relational planning.
#[derive(Clone, Debug, Eq, PartialEq)]
enum ResolvedServerFunctionReturn {
    Single {
        location: SourceLocation,
    },
    Rows {
        columns: Vec<ResolvedServerFunctionReturnColumn>,
        location: SourceLocation,
    },
}

/// A resolved SERVER function that is ready for relational body checking.
#[derive(Clone, Debug)]
struct ResolvedServerFunctionInput<'a> {
    id: CheckedFunctionId,
    name: QualifiedSemanticName,
    parameters: Vec<ResolvedServerFunctionParameter>,
    return_type: ResolvedServerFunctionReturn,
    security: CatalogueFunctionSecurity,
    transaction: Option<CatalogueFunctionTransaction>,
    volatility: CatalogueFunctionVolatility,
    body: &'a ServerFunctionBody,
    location: SourceLocation,
}

fn check_parsed(parse_report: ParseReport, base: &CatalogueSnapshot) -> CheckReport {
    let mut diagnostics = parse_report.diagnostics().to_vec();
    if !diagnostics.is_empty() {
        return failed(parse_report, diagnostics);
    }

    let mut assignments = CheckAssignments::new();
    let mut checked_schemas = Vec::new();
    let mut known_schemas = HashSet::new();
    let mut submitted_schemas = HashSet::new();
    for unit in parse_report.units() {
        for declaration in unit.parsed().schemas() {
            let name = semantic_name(&declaration.name);
            if !submitted_schemas.insert(name.clone()) {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DuplicateDefinition,
                    format!("duplicate schema definition {name}"),
                    unit.logical_path(),
                    &declaration.name.span,
                ));
                continue;
            }
            known_schemas.insert(name.clone());
            checked_schemas.push(CheckedSchema {
                id: assignments.schema_id(base.schema_by_name(&name).map(|schema| schema.id())),
                name,
                location: location(unit.logical_path(), &declaration.span),
            });
        }
    }

    let mut headers = Vec::new();
    let mut declarations_by_name = HashMap::<QualifiedSemanticName, usize>::new();
    for unit in parse_report.units() {
        for declaration in unit.parsed().object_types() {
            let name = semantic_name(&declaration.name);
            let Some(namespace) = namespace_of(&name) else {
                diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    format!("object type {name} has no declared schema"),
                    unit.logical_path(),
                    &declaration.name.span,
                ));
                continue;
            };
            if !known_schemas.contains(&namespace) {
                diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    format!("unknown schema {namespace} for object type {name}"),
                    unit.logical_path(),
                    &declaration.name.span,
                ));
                continue;
            }
            if declarations_by_name.contains_key(&name) {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DuplicateDefinition,
                    format!("duplicate object type definition {name}"),
                    unit.logical_path(),
                    &declaration.name.span,
                ));
                continue;
            }
            declarations_by_name.insert(name.clone(), headers.len());
            let id = assignments.type_id(
                base.object_type_by_name(&name)
                    .map(|object_type| object_type.id()),
            );
            headers.push(Header {
                declaration,
                logical_path: unit.logical_path(),
                id,
            });
        }
    }

    let mut submitted_ids = HashMap::new();
    for header in &headers {
        submitted_ids.insert(semantic_name(&header.declaration.name), header.id);
    }

    let mut checked_types = Vec::with_capacity(headers.len());
    for header in headers {
        let type_name = semantic_name(&header.declaration.name);
        let base_type = base.object_type_by_name(&type_name);
        let mut field_names = HashSet::new();
        let mut checked_fields = Vec::with_capacity(header.declaration.fields.len());

        for field in &header.declaration.fields {
            let name = semantic_part(&field.name);
            if !field_names.insert(name.clone()) {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DuplicateDefinition,
                    format!(
                        "duplicate field definition {name} in {}",
                        semantic_name(&header.declaration.name)
                    ),
                    header.logical_path,
                    &field.name.span,
                ));
                continue;
            }

            let semantic_type = resolve_type(
                &field.type_specification,
                &submitted_ids,
                header.logical_path,
                &mut diagnostics,
            );
            let on_delete = map_on_delete(field.on_delete);
            if on_delete.is_some()
                && !matches!(
                    field.type_specification,
                    TypeSpecification::Reference { .. }
                )
            {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    "ON DELETE is only valid for REF fields",
                    header.logical_path,
                    &field.span,
                ));
            }
            if matches!(on_delete, Some(OnDeleteAction::SetNull)) && !field.nullable {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    "ON DELETE SET NULL requires a nullable field",
                    header.logical_path,
                    &field.span,
                ));
            }

            let id = assignments.field_id(
                base_type
                    .and_then(|object_type| object_type.field_by_name(&name))
                    .map(|field| field.id()),
            );
            let existing_default = base_type
                .and_then(|object_type| object_type.field_by_name(&name))
                .and_then(|field| field.default_expression());
            let default = match (field.default_expression.as_ref(), semantic_type) {
                (Some(source), Some(semantic_type)) => checked_default(
                    source,
                    semantic_type,
                    field.nullable,
                    existing_default,
                    header.logical_path,
                    &mut assignments,
                    &mut diagnostics,
                ),
                _ => None,
            };

            if let Some(semantic_type) = semantic_type {
                checked_fields.push(CheckedField {
                    id,
                    name,
                    ordinal: field.order as u32,
                    semantic_type,
                    nullable: field.nullable,
                    unique: field.unique,
                    default,
                    on_delete,
                    location: location(header.logical_path, &field.span),
                });
            }
        }

        checked_types.push(CheckedObjectType {
            id: header.id,
            name: semantic_name(&header.declaration.name),
            fields: checked_fields,
            location: location(header.logical_path, &header.declaration.span),
        });
    }

    if !diagnostics.is_empty() {
        return failed(parse_report, diagnostics);
    }

    reject_unplanned_server_function_features(&parse_report, &mut diagnostics);
    if !diagnostics.is_empty() {
        return failed(parse_report, diagnostics);
    }

    let query_catalogue = checked_query_catalogue(&checked_types);

    let function_headers = if diagnostics.is_empty() {
        resolve_server_function_headers(
            &parse_report,
            base,
            &known_schemas,
            &mut assignments,
            &mut diagnostics,
        )
    } else {
        Vec::new()
    };
    let function_inputs = if diagnostics.is_empty() {
        resolve_server_function_inputs(
            &function_headers,
            &submitted_ids,
            base,
            &mut assignments,
            &mut diagnostics,
        )
    } else {
        Vec::new()
    };
    let checked_functions = if diagnostics.is_empty() {
        check_server_functions(&function_inputs, &query_catalogue, &mut diagnostics)
    } else {
        Vec::new()
    };

    if !diagnostics.is_empty() {
        return failed(parse_report, diagnostics);
    }

    CheckReport {
        parse_report,
        diagnostics,
        checked_bundle: Some(CheckedBundle {
            base_catalogue_revision: base.revision(),
            schemas: checked_schemas,
            object_types: checked_types,
            server_functions: checked_functions,
        }),
    }
}

fn resolve_server_function_headers<'a>(
    parse_report: &'a ParseReport,
    base: &CatalogueSnapshot,
    known_schemas: &HashSet<QualifiedSemanticName>,
    assignments: &mut CheckAssignments,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) -> Vec<ServerFunctionHeader<'a>> {
    let mut headers = Vec::new();
    let mut declarations_by_name = HashSet::new();

    for unit in parse_report.units() {
        for declaration in unit.parsed().server_functions() {
            let name = semantic_name(&declaration.name);
            if !declarations_by_name.insert(name.clone()) {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DuplicateDefinition,
                    format!("duplicate server function definition {name}"),
                    unit.logical_path(),
                    &declaration.name.span,
                ));
                continue;
            }
            let Some(namespace) = namespace_of(&name) else {
                diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    format!("server function {name} has no declared schema"),
                    unit.logical_path(),
                    &declaration.name.span,
                ));
                continue;
            };
            if !known_schemas.contains(&namespace) {
                diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    format!("unknown schema {namespace} for server function {name}"),
                    unit.logical_path(),
                    &declaration.name.span,
                ));
                continue;
            }

            let security = map_function_security(declaration.security);
            let transaction = map_function_transaction(declaration.transaction);
            let volatility = map_function_volatility(declaration.volatility);
            if transaction == Some(CatalogueFunctionTransaction::Manual) {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DomainIncompatible,
                    "TRANSACTION MANUAL is outside the MVP SERVER function domain",
                    unit.logical_path(),
                    &declaration.span,
                ));
                continue;
            }

            headers.push(ServerFunctionHeader {
                declaration,
                logical_path: unit.logical_path(),
                id: assignments
                    .function_id(base.function_by_name(&name).map(|function| function.id())),
                security,
                transaction,
                volatility,
            });
        }
    }

    headers
}

fn resolve_server_function_inputs<'a>(
    headers: &[ServerFunctionHeader<'a>],
    submitted_ids: &HashMap<QualifiedSemanticName, CheckedTypeId>,
    base: &CatalogueSnapshot,
    assignments: &mut CheckAssignments,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) -> Vec<ResolvedServerFunctionInput<'a>> {
    let mut inputs = Vec::with_capacity(headers.len());

    for header in headers {
        let diagnostics_before = diagnostics.len();
        let name = semantic_name(&header.declaration.name);
        let base_function = base.function_by_name(&name);
        let mut parameter_names = HashSet::new();
        let mut parameters = Vec::with_capacity(header.declaration.parameters.len());

        for parameter in &header.declaration.parameters {
            let parameter_name = semantic_part(&parameter.name);
            if !parameter_names.insert(parameter_name.clone()) {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DuplicateDefinition,
                    format!("duplicate parameter definition {parameter_name} in {name}"),
                    header.logical_path,
                    &parameter.name.span,
                ));
                continue;
            }

            let Some(semantic_type) = resolve_type(
                &parameter.type_specification,
                submitted_ids,
                header.logical_path,
                diagnostics,
            ) else {
                continue;
            };
            let id = assignments.parameter_id(
                base_function
                    .and_then(|function| function.parameter_by_name(&parameter_name))
                    .map(|parameter| parameter.id()),
            );
            parameters.push(ResolvedServerFunctionParameter {
                id,
                name: parameter_name,
                ordinal: parameter.order as u32,
                semantic_type,
                location: location(header.logical_path, &parameter.span),
                reference_location: reference_location(
                    &parameter.type_specification,
                    header.logical_path,
                ),
            });
        }

        let return_type = resolve_server_function_return(
            &header.declaration.return_type,
            submitted_ids,
            header.logical_path,
            diagnostics,
        );
        if diagnostics.len() != diagnostics_before {
            continue;
        }

        let Some(return_type) = return_type else {
            continue;
        };
        inputs.push(ResolvedServerFunctionInput {
            id: header.id,
            name,
            parameters,
            return_type,
            security: header.security,
            transaction: header.transaction,
            volatility: header.volatility,
            body: &header.declaration.body,
            location: location(header.logical_path, &header.declaration.span),
        });
    }

    inputs
}

fn reject_unplanned_server_function_features(
    parse_report: &ParseReport,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) {
    for unit in parse_report.units() {
        for declaration in unit.parsed().server_functions() {
            for parameter in &declaration.parameters {
                if let Some(default) = &parameter.default_expression {
                    diagnostics.push(diagnostic(
                        DiagnosticCode::TypeMismatch,
                        "SERVER default-expression planning is not implemented",
                        unit.logical_path(),
                        &default.span,
                    ));
                }
            }
            for capability in &declaration.capabilities {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    "SERVER capability checking/planning is not implemented",
                    unit.logical_path(),
                    &capability.span,
                ));
            }
        }
    }
}

fn resolve_server_function_return(
    return_type: &FunctionReturnType,
    submitted_ids: &HashMap<QualifiedSemanticName, CheckedTypeId>,
    logical_path: &str,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) -> Option<ResolvedServerFunctionReturn> {
    match return_type {
        FunctionReturnType::Single(specification) => {
            resolve_type(specification, submitted_ids, logical_path, diagnostics).map(|_| {
                ResolvedServerFunctionReturn::Single {
                    location: location(logical_path, specification.span()),
                }
            })
        }
        FunctionReturnType::Rows { columns, span } => {
            if columns.is_empty() {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    "ROWS return type must contain at least one column",
                    logical_path,
                    span,
                ));
                return None;
            }

            let diagnostics_before = diagnostics.len();
            let mut names = HashSet::new();
            let mut resolved_columns = Vec::with_capacity(columns.len());
            for column in columns {
                let name = semantic_part(&column.name);
                if !names.insert(name.clone()) {
                    diagnostics.push(diagnostic(
                        DiagnosticCode::DuplicateDefinition,
                        format!("duplicate ROWS return column definition {name}"),
                        logical_path,
                        &column.name.span,
                    ));
                    continue;
                }
                let Some(semantic_type) = resolve_type(
                    &column.type_specification,
                    submitted_ids,
                    logical_path,
                    diagnostics,
                ) else {
                    continue;
                };
                resolved_columns.push(ResolvedServerFunctionReturnColumn {
                    name,
                    ordinal: column.order as u32,
                    semantic_type,
                    location: location(logical_path, &column.span),
                    reference_location: reference_location(
                        &column.type_specification,
                        logical_path,
                    ),
                });
            }
            if diagnostics.len() != diagnostics_before {
                return None;
            }
            Some(ResolvedServerFunctionReturn::Rows {
                columns: resolved_columns,
                location: location(logical_path, span),
            })
        }
    }
}

fn check_server_functions(
    inputs: &[ResolvedServerFunctionInput<'_>],
    catalogue: &ResolutionCatalogue<CheckedTypeId, CheckedFieldId>,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) -> Vec<CheckedServerFunction> {
    let diagnostics_before = diagnostics.len();
    let mut functions = Vec::with_capacity(inputs.len());

    for input in inputs {
        let ResolvedServerFunctionReturn::Rows { columns, location } = &input.return_type else {
            let ResolvedServerFunctionReturn::Single { location } = &input.return_type else {
                unreachable!("SERVER function return resolution has two variants")
            };
            diagnostics.push(DiagnosticCode::semantic(
                DiagnosticCode::TypeMismatch,
                "SELECT SERVER functions require RETURNS ROWS (...)",
                location.clone(),
            ));
            continue;
        };
        let ServerFunctionBody::SqlQuery(body) = input.body;
        let query_check =
            match check_query_in(&body.query, catalogue, input.location.logical_path()) {
                Ok(query_check) => query_check,
                Err(query_diagnostics) => {
                    diagnostics.extend(query_diagnostics);
                    continue;
                }
            };
        let plan = query_check.plan();

        if plan.projections().len() != columns.len() {
            diagnostics.push(DiagnosticCode::semantic(
                DiagnosticCode::TypeMismatch,
                format!(
                    "SELECT projection count {} does not match RETURNS ROWS column count {}",
                    plan.projections().len(),
                    columns.len()
                ),
                location.clone(),
            ));
            continue;
        }

        let mut matches_return = true;
        for (projection, column) in plan.projections().iter().zip(columns) {
            if projection.value_type().semantic_type() != column.semantic_type {
                diagnostics.push(DiagnosticCode::semantic(
                    DiagnosticCode::TypeMismatch,
                    format!(
                        "SELECT projection {} type does not match RETURNS ROWS column {}",
                        column.ordinal + 1,
                        column.name
                    ),
                    column.location.clone(),
                ));
                matches_return = false;
            }
        }
        if !matches_return {
            continue;
        }

        let mut references = signature_references(&input.parameters, columns);
        references.extend(query_check.references().iter().map(query_reference));

        functions.push(CheckedServerFunction {
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
            return_columns: columns
                .iter()
                .cloned()
                .map(|column| CheckedServerFunctionReturnColumn {
                    name: column.name,
                    ordinal: column.ordinal,
                    semantic_type: column.semantic_type,
                    location: column.location,
                })
                .collect(),
            security: input.security,
            transaction: input.transaction,
            volatility: input.volatility,
            location: input.location.clone(),
            plan: plan.clone(),
            references,
        });
    }

    if diagnostics.len() != diagnostics_before {
        return Vec::new();
    }

    functions
}

fn checked_query_catalogue(
    object_types: &[CheckedObjectType],
) -> ResolutionCatalogue<CheckedTypeId, CheckedFieldId> {
    ResolutionCatalogue::new(
        object_types
            .iter()
            .map(|object_type| {
                QueryObjectType::new(
                    object_type.id,
                    object_type.name.clone(),
                    object_type
                        .fields
                        .iter()
                        .map(|field| {
                            (
                                field.name.clone(),
                                QueryField::new(field.id, field.semantic_type, field.nullable),
                            )
                        })
                        .collect(),
                )
            })
            .collect(),
    )
    .expect("checked definitions satisfy resolver-local query catalogue invariants")
}

fn signature_references(
    parameters: &[ResolvedServerFunctionParameter],
    columns: &[ResolvedServerFunctionReturnColumn],
) -> Vec<CheckedDefinitionReference> {
    let mut references = Vec::new();
    references.extend(parameters.iter().filter_map(|parameter| {
        object_reference(
            parameter.semantic_type,
            parameter.reference_location.as_ref(),
        )
    }));
    references.extend(columns.iter().filter_map(|column| {
        object_reference(column.semantic_type, column.reference_location.as_ref())
    }));
    references
}

fn object_reference(
    semantic_type: SemanticType<CheckedTypeId>,
    location: Option<&SourceLocation>,
) -> Option<CheckedDefinitionReference> {
    let SemanticType::Reference { target } = semantic_type else {
        return None;
    };
    let location = location?.clone();
    Some(CheckedDefinitionReference {
        target: CheckedDefinitionReferenceTarget::ObjectType(target),
        kind: DefinitionReferenceKind::ObjectReference,
        location,
    })
}

fn query_reference(
    reference: &QueryReference<CheckedTypeId, CheckedFieldId>,
) -> CheckedDefinitionReference {
    let (target, kind) = match (reference.kind(), *reference.target()) {
        (QueryReferenceKind::QueryObject, QueryReferenceTarget::Object(object_type)) => (
            CheckedDefinitionReferenceTarget::ObjectType(object_type),
            DefinitionReferenceKind::QueryObject,
        ),
        (QueryReferenceKind::ObjectReference, QueryReferenceTarget::Object(object_type)) => (
            CheckedDefinitionReferenceTarget::ObjectType(object_type),
            DefinitionReferenceKind::ObjectReference,
        ),
        (QueryReferenceKind::QueryField, QueryReferenceTarget::Field { owner, field }) => (
            CheckedDefinitionReferenceTarget::Field { owner, field },
            DefinitionReferenceKind::QueryField,
        ),
        _ => unreachable!("relational query evidence has an invalid kind and target pair"),
    };
    CheckedDefinitionReference {
        target,
        kind,
        location: reference.location().clone(),
    }
}

fn reference_location(
    specification: &TypeSpecification,
    logical_path: &str,
) -> Option<SourceLocation> {
    let TypeSpecification::Reference { target, .. } = specification else {
        return None;
    };
    Some(location(logical_path, &target.span))
}

fn map_function_security(mode: Option<SyntaxFunctionSecurity>) -> CatalogueFunctionSecurity {
    match mode {
        Some(SyntaxFunctionSecurity::Definer) => CatalogueFunctionSecurity::Definer,
        Some(SyntaxFunctionSecurity::Invoker) | None => CatalogueFunctionSecurity::Invoker,
    }
}

fn map_function_transaction(
    mode: Option<SyntaxFunctionTransaction>,
) -> Option<CatalogueFunctionTransaction> {
    match mode {
        Some(SyntaxFunctionTransaction::Atomic) => Some(CatalogueFunctionTransaction::Atomic),
        Some(SyntaxFunctionTransaction::ReadOnly) => Some(CatalogueFunctionTransaction::ReadOnly),
        Some(SyntaxFunctionTransaction::Manual) => Some(CatalogueFunctionTransaction::Manual),
        None => None,
    }
}

fn map_function_volatility(mode: Option<SyntaxFunctionVolatility>) -> CatalogueFunctionVolatility {
    match mode {
        Some(SyntaxFunctionVolatility::Immutable) => CatalogueFunctionVolatility::Immutable,
        Some(SyntaxFunctionVolatility::Stable) => CatalogueFunctionVolatility::Stable,
        Some(SyntaxFunctionVolatility::Volatile) | None => CatalogueFunctionVolatility::Volatile,
    }
}

fn failed(parse_report: ParseReport, diagnostics: Vec<CompilerDiagnostic>) -> CheckReport {
    CheckReport {
        parse_report,
        diagnostics,
        checked_bundle: None,
    }
}

fn resolve_type(
    specification: &TypeSpecification,
    submitted_ids: &HashMap<QualifiedSemanticName, CheckedTypeId>,
    logical_path: &str,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) -> Option<SemanticType<CheckedTypeId>> {
    match specification {
        TypeSpecification::Named(name) => {
            if let Some(scalar) = resolve_closed_scalar(name) {
                return Some(SemanticType::scalar(scalar));
            }
            let semantic_name = semantic_name(name);
            if submitted_ids.contains_key(&semantic_name) {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    format!("object type {semantic_name} must be declared with REF"),
                    logical_path,
                    &name.span,
                ));
            } else {
                diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    format!("unknown type name {semantic_name}"),
                    logical_path,
                    &name.span,
                ));
            }
            None
        }
        TypeSpecification::StandardLargeObject { kind, .. } => {
            let scalar = match kind {
                StandardLargeObjectKind::Character => StandardScalar::CharacterLargeObject,
                StandardLargeObjectKind::Binary => StandardScalar::BinaryLargeObject,
            };
            Some(SemanticType::scalar(scalar))
        }
        TypeSpecification::Reference { target, .. } => {
            if resolve_closed_scalar(target).is_some() {
                diagnostics.push(diagnostic(
                    DiagnosticCode::InvalidReferenceTarget,
                    format!("REF target {} is a scalar type", semantic_name(target)),
                    logical_path,
                    &target.span,
                ));
                return None;
            }
            let name = semantic_name(target);
            if let Some(id) = submitted_ids.get(&name).copied() {
                Some(SemanticType::reference(id))
            } else {
                diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    format!("unknown object type {name}"),
                    logical_path,
                    &target.span,
                ));
                None
            }
        }
    }
}

fn resolve_closed_scalar(name: &QualifiedName) -> Option<StandardScalar> {
    if name.parts.len() != 1 || name.parts[0].text.starts_with('"') {
        return None;
    }
    StandardScalar::from_source_spelling(&name.parts[0].text).ok()
}

fn checked_default(
    source: &SourceSlice,
    semantic_type: SemanticType<CheckedTypeId>,
    nullable: bool,
    existing_id: Option<ExpressionId>,
    logical_path: &str,
    assignments: &mut CheckAssignments,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) -> Option<CheckedDefault> {
    let value = match parse_constant(&source.text) {
        Some(value) => value,
        None => {
            diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                "only constant NULL, TRUE, FALSE, text, and integer defaults are supported",
                logical_path,
                &source.span,
            ));
            return None;
        }
    };
    let valid = match (&value, semantic_type) {
        (ConstantValue::Null, _) => nullable,
        (ConstantValue::Boolean(_), SemanticType::Scalar(StandardScalar::Boolean)) => true,
        (
            ConstantValue::Integer(_),
            SemanticType::Scalar(StandardScalar::Integer | StandardScalar::BigInt),
        ) => true,
        (ConstantValue::Text(_), SemanticType::Scalar(StandardScalar::CharacterLargeObject)) => {
            true
        }
        _ => false,
    };
    if !valid {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            "default constant does not match the field type and nullability",
            logical_path,
            &source.span,
        ));
        return None;
    }
    Some(CheckedDefault {
        id: assignments.expression_id(existing_id),
        value,
        location: location(logical_path, &source.span),
    })
}

fn parse_constant(source: &str) -> Option<ConstantValue> {
    let source = source.trim();
    if source.eq_ignore_ascii_case("NULL") {
        return Some(ConstantValue::Null);
    }
    if source.eq_ignore_ascii_case("TRUE") {
        return Some(ConstantValue::Boolean(true));
    }
    if source.eq_ignore_ascii_case("FALSE") {
        return Some(ConstantValue::Boolean(false));
    }
    if source.len() >= 2 && source.starts_with('\'') && source.ends_with('\'') {
        return Some(ConstantValue::Text(
            source[1..source.len() - 1].replace("''", "'"),
        ));
    }
    source.parse::<i64>().ok().map(ConstantValue::Integer)
}

fn map_on_delete(policy: Option<OnDeletePolicy>) -> Option<OnDeleteAction> {
    match policy {
        Some(OnDeletePolicy::Restrict) => Some(OnDeleteAction::Restrict),
        Some(OnDeletePolicy::SetNull) => Some(OnDeleteAction::SetNull),
        Some(OnDeletePolicy::Cascade) => Some(OnDeleteAction::Cascade),
        None => None,
    }
}

fn namespace_of(name: &QualifiedSemanticName) -> Option<QualifiedSemanticName> {
    let namespace_parts = name.parts().get(..name.parts().len().checked_sub(1)?)?;
    if namespace_parts.is_empty() {
        return None;
    }
    QualifiedSemanticName::new(namespace_parts.iter().cloned()).ok()
}

fn location(logical_path: &str, span: &SourceSpan) -> SourceLocation {
    SourceLocation::from_syntax(logical_path, span)
}

fn diagnostic(
    code: DiagnosticCode,
    message: impl Into<String>,
    logical_path: &str,
    span: &SourceSpan,
) -> CompilerDiagnostic {
    DiagnosticCode::semantic(code, message, location(logical_path, span))
}

#[cfg(test)]
mod tests {
    use orna_core::{
        CatalogueRevisionId, ExpressionId, FieldId, FunctionId, FunctionRevisionId, ParameterId,
        SchemaId, TypeId,
        catalogue::{
            CatalogueSnapshot, FieldDefinition, FunctionDefinition, FunctionDomain, FunctionReturn,
            FunctionReturnColumnDefinition, FunctionSecurity, FunctionTransaction,
            FunctionVolatility, ObjectTypeDefinition, OnDeleteAction, ParameterDefinition,
            QualifiedSemanticName, SchemaDefinition,
        },
        revision::DefinitionReferenceKind,
        source::{SourceBundle, SourceUnit},
        types::{ResolvedType, StandardScalar},
    };

    use crate::relational::ExpressionKind;

    use super::{
        CheckedDefinitionReferenceTarget, ConstantValue, DiagnosticCode, SemanticType, check,
    };

    fn empty_catalogue() -> CatalogueSnapshot {
        catalogue(Vec::new(), Vec::new(), Vec::new())
    }

    fn catalogue(
        schemas: Vec<SchemaDefinition>,
        object_types: Vec<ObjectTypeDefinition>,
        functions: Vec<FunctionDefinition>,
    ) -> CatalogueSnapshot {
        CatalogueSnapshot::new_with_functions(
            CatalogueRevisionId::from_bytes([1; 16]),
            schemas,
            object_types,
            functions,
        )
        .unwrap()
    }

    fn schema(id: u8, parts: &[&str]) -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::from_bytes([id; 16]),
            QualifiedSemanticName::new(parts.iter().copied()).unwrap(),
        )
    }

    fn object_type(id: u8, parts: &[&str], fields: Vec<FieldDefinition>) -> ObjectTypeDefinition {
        ObjectTypeDefinition::new(
            TypeId::from_bytes([id; 16]),
            QualifiedSemanticName::new(parts.iter().copied()).unwrap(),
            fields,
        )
    }

    fn field(
        id: u8,
        name: &str,
        ordinal: u32,
        resolved_type: ResolvedType,
        default_expression: Option<ExpressionId>,
    ) -> FieldDefinition {
        FieldDefinition::new(
            FieldId::from_bytes([id; 16]),
            name,
            ordinal,
            resolved_type,
            true,
            false,
            default_expression,
            None,
        )
    }

    fn parameter(
        id: u8,
        name: &str,
        ordinal: u32,
        resolved_type: ResolvedType,
    ) -> ParameterDefinition {
        ParameterDefinition::new(
            ParameterId::from_bytes([id; 16]),
            name,
            ordinal,
            resolved_type,
            None,
        )
    }

    fn rows_column(
        name: &str,
        ordinal: u32,
        resolved_type: ResolvedType,
    ) -> FunctionReturnColumnDefinition {
        FunctionReturnColumnDefinition::new(name, ordinal, resolved_type)
    }

    #[allow(clippy::too_many_arguments)]
    fn server_function(
        id: u8,
        parts: &[&str],
        parameters: Vec<ParameterDefinition>,
        return_columns: Vec<FunctionReturnColumnDefinition>,
        security: FunctionSecurity,
        transaction: Option<FunctionTransaction>,
        volatility: FunctionVolatility,
    ) -> FunctionDefinition {
        FunctionDefinition::new(
            FunctionId::from_bytes([id; 16]),
            QualifiedSemanticName::new(parts.iter().copied()).unwrap(),
            FunctionDomain::Server,
            parameters,
            FunctionReturn::Rows(return_columns),
            FunctionRevisionId::from_bytes([id.saturating_add(100); 16]),
            security,
            transaction,
            volatility,
        )
    }

    fn bundle(units: impl IntoIterator<Item = (&'static str, &'static str)>) -> SourceBundle {
        SourceBundle::new(
            units
                .into_iter()
                .map(|(path, source)| SourceUnit::new(path, source)),
        )
        .unwrap()
    }

    fn assert_no_checked_bundle(report: &super::CheckReport) {
        assert!(!report.diagnostics().is_empty());
        assert!(report.checked_bundle().is_none());
    }

    #[test]
    fn resolves_forward_references_across_source_units() {
        let report = check(
            &bundle([
                (
                    "tasks.orna",
                    "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (assignee REF people.person);",
                ),
                (
                    "people.orna",
                    "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (name TEXT NOT NULL);",
                ),
            ]),
            &empty_catalogue(),
        );

        assert!(report.diagnostics().is_empty());
        let checked = report.checked_bundle().unwrap();
        assert_eq!(checked.schemas().len(), 2);
        assert_eq!(checked.schemas()[0].name().to_string(), "tasks");
        assert_eq!(checked.schemas()[1].name().to_string(), "people");
        let task = &checked.object_types()[0];
        let person = &checked.object_types()[1];
        assert_eq!(
            task.fields()[0].semantic_type(),
            SemanticType::reference(person.id())
        );
        assert_eq!(task.id().to_string(), "provisional:type:0");
        assert_eq!(person.id().to_string(), "provisional:type:1");
    }

    #[test]
    fn empty_schema_declaration_persists_with_a_stable_identity() {
        let schema_id = SchemaId::from_bytes([2; 16]);
        let base = catalogue(vec![schema(2, &["crm"])], Vec::new(), Vec::new());
        let report = check(&bundle([("schema.orna", "CREATE SCHEMA CRM;")]), &base);

        assert!(report.diagnostics().is_empty());
        let checked = report.checked_bundle().unwrap();
        assert_eq!(checked.base_catalogue_revision(), base.revision());
        assert_eq!(checked.schemas().len(), 1);
        assert_eq!(checked.schemas()[0].name().to_string(), "crm");
        assert_eq!(checked.schemas()[0].id().existing(), Some(schema_id));
    }

    #[test]
    fn requires_submitted_schema_declarations_even_when_base_has_them() {
        let base = catalogue(vec![schema(1, &["crm"])], Vec::new(), Vec::new());

        let object_report = check(
            &bundle([(
                "types.orna",
                "CREATE TYPE crm.contact AS OBJECT (name TEXT);",
            )]),
            &base,
        );
        assert_eq!(object_report.diagnostics().len(), 1);
        assert_eq!(
            object_report.diagnostics()[0].code(),
            DiagnosticCode::UnknownQualifiedName
        );
        assert_no_checked_bundle(&object_report);

        let function_report = check(
            &bundle([(
                "functions.orna",
                "CREATE SERVER FUNCTION crm.probe_status() RETURNS ROWS (enabled BOOL) \
                 AS SELECT p.enabled FROM crm.probe p;",
            )]),
            &base,
        );
        assert_eq!(function_report.diagnostics().len(), 1);
        assert_eq!(
            function_report.diagnostics()[0].code(),
            DiagnosticCode::UnknownQualifiedName
        );
        assert_no_checked_bundle(&function_report);
    }

    #[test]
    fn maps_alias_defaults_nullability_and_delete_policies() {
        let report = check(
            &bundle([(
                "schema.orna",
                "CREATE SCHEMA people; CREATE SCHEMA tasks;\
                 CREATE TYPE people.person AS OBJECT (name TEXT NOT NULL);\
                 CREATE TYPE tasks.task AS OBJECT (\
                     done BOOL NOT NULL DEFAULT FALSE,\
                     count INT DEFAULT 7,\
                     note TEXT DEFAULT 'it''s fine',\
                     owner REF people.person ON DELETE SET NULL,\
                     document CLOB,\
                     payload BLOB\
                 );",
            )]),
            &empty_catalogue(),
        );

        assert!(report.diagnostics().is_empty());
        let fields = report.checked_bundle().unwrap().object_types()[1].fields();
        assert_eq!(
            fields[0].semantic_type(),
            SemanticType::scalar(StandardScalar::Boolean)
        );
        assert!(!fields[0].nullable());
        assert_eq!(
            fields[0].default().unwrap().value(),
            &ConstantValue::Boolean(false)
        );
        assert_eq!(
            fields[1].semantic_type(),
            SemanticType::scalar(StandardScalar::Integer)
        );
        assert_eq!(
            fields[1].default().unwrap().value(),
            &ConstantValue::Integer(7)
        );
        assert_eq!(
            fields[2].default().unwrap().value(),
            &ConstantValue::Text("it's fine".to_owned())
        );
        assert!(fields[3].nullable());
        assert_eq!(fields[3].on_delete(), Some(OnDeleteAction::SetNull));
        assert_eq!(
            fields[4].semantic_type(),
            SemanticType::scalar(StandardScalar::CharacterLargeObject)
        );
        assert_eq!(
            fields[5].semantic_type(),
            SemanticType::scalar(StandardScalar::BinaryLargeObject)
        );
    }

    #[test]
    fn resolves_canonical_multiword_large_object_scalars() {
        let report = check(
            &bundle([(
                "schema.orna",
                "CREATE SCHEMA files; CREATE TYPE files.document AS OBJECT (body cHaRaCtEr /* retained */ LaRgE ObJeCt, content bInArY LARGE object);",
            )]),
            &empty_catalogue(),
        );

        assert!(report.diagnostics().is_empty());
        let fields = report.checked_bundle().unwrap().object_types()[0].fields();
        assert_eq!(
            fields[0].semantic_type(),
            SemanticType::scalar(StandardScalar::CharacterLargeObject)
        );
        assert_eq!(
            fields[1].semantic_type(),
            SemanticType::scalar(StandardScalar::BinaryLargeObject)
        );
    }

    #[test]
    fn repeated_checks_preserve_matching_ids_even_when_fields_reorder() {
        let name_id = FieldId::from_bytes([3; 16]);
        let age_id = FieldId::from_bytes([4; 16]);
        let default_id = ExpressionId::from_bytes([5; 16]);
        let base = catalogue(
            vec![schema(1, &["people"])],
            vec![object_type(
                2,
                &["people", "person"],
                vec![
                    field(
                        3,
                        "name",
                        0,
                        ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                        None,
                    ),
                    field(
                        4,
                        "age",
                        1,
                        ResolvedType::scalar(StandardScalar::Integer),
                        Some(default_id),
                    ),
                ],
            )],
            Vec::new(),
        );

        let report = check(
            &bundle([(
                "renamed-file.orna",
                "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (age INT DEFAULT 1, name TEXT);",
            )]),
            &base,
        );

        assert!(report.diagnostics().is_empty());
        let revised = &report.checked_bundle().unwrap().object_types()[0];
        assert_eq!(revised.id().existing(), Some(TypeId::from_bytes([2; 16])));
        assert_eq!(revised.fields()[0].name(), "age");
        assert_eq!(revised.fields()[0].id().existing(), Some(age_id));
        assert_eq!(revised.fields()[1].name(), "name");
        assert_eq!(revised.fields()[1].id().existing(), Some(name_id));
        assert_eq!(
            revised.fields()[0].default().unwrap().id().existing(),
            Some(default_id)
        );
    }

    #[test]
    fn added_field_gets_a_new_identity() {
        let name_id = FieldId::from_bytes([3; 16]);
        let base = catalogue(
            vec![schema(1, &["people"])],
            vec![object_type(
                2,
                &["people", "person"],
                vec![field(
                    3,
                    "name",
                    0,
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    None,
                )],
            )],
            Vec::new(),
        );
        let report = check(
            &bundle([(
                "schema.orna",
                "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (name TEXT, email TEXT);",
            )]),
            &base,
        );

        assert!(report.diagnostics().is_empty());
        let revised = &report.checked_bundle().unwrap().object_types()[0];
        assert_eq!(revised.fields()[0].id().existing(), Some(name_id));
        assert_eq!(revised.fields()[1].id().to_string(), "provisional:field:0");
    }

    #[test]
    fn identical_checks_return_equal_checked_bundles() {
        let source = "CREATE SCHEMA demo; CREATE TYPE demo.item AS OBJECT (value INT DEFAULT 1);";
        let first = check(&bundle([("demo.orna", source)]), &empty_catalogue());
        let second = check(&bundle([("demo.orna", source)]), &empty_catalogue());

        assert!(first.diagnostics().is_empty());
        assert_eq!(first.checked_bundle(), second.checked_bundle());
    }

    #[test]
    fn syntax_errors_do_not_return_a_checked_bundle() {
        let report = check(
            &bundle([("broken.orna", "CREATE SCHEMA ;")]),
            &empty_catalogue(),
        );

        assert_no_checked_bundle(&report);
    }

    #[test]
    fn assigns_exact_kind_local_provisional_counters() {
        let source = "CREATE SCHEMA alpha; CREATE SCHEMA beta; \
            CREATE TYPE alpha.one AS OBJECT (number INT DEFAULT 1); \
            CREATE TYPE beta.two AS OBJECT (one REF alpha.one, number INT DEFAULT 2); \
            CREATE SERVER FUNCTION alpha.first(p_one REF alpha.one, p_number INT) \
            RETURNS ROWS (number INT) AS SELECT o.number FROM alpha.one o; \
            CREATE SERVER FUNCTION beta.second(p_two REF beta.two) \
            RETURNS ROWS (number INT) AS SELECT t.number FROM beta.two t;";
        let report = check(&bundle([("counters.orna", source)]), &empty_catalogue());

        assert!(report.diagnostics().is_empty());
        let checked = report.checked_bundle().unwrap();
        assert_eq!(
            checked.schemas()[0].id().to_string(),
            "provisional:schema:0"
        );
        assert_eq!(
            checked.schemas()[1].id().to_string(),
            "provisional:schema:1"
        );
        assert_eq!(
            checked.object_types()[0].id().to_string(),
            "provisional:type:0"
        );
        assert_eq!(
            checked.object_types()[1].id().to_string(),
            "provisional:type:1"
        );
        assert_eq!(
            checked.object_types()[0].fields()[0].id().to_string(),
            "provisional:field:0"
        );
        assert_eq!(
            checked.object_types()[1].fields()[0].id().to_string(),
            "provisional:field:1"
        );
        assert_eq!(
            checked.object_types()[1].fields()[1].id().to_string(),
            "provisional:field:2"
        );
        assert_eq!(
            checked.object_types()[0].fields()[0]
                .default()
                .unwrap()
                .id()
                .to_string(),
            "provisional:expression:0"
        );
        assert_eq!(
            checked.object_types()[1].fields()[1]
                .default()
                .unwrap()
                .id()
                .to_string(),
            "provisional:expression:1"
        );
        assert_eq!(
            checked.server_functions()[0].id().to_string(),
            "provisional:function:0"
        );
        assert_eq!(
            checked.server_functions()[1].id().to_string(),
            "provisional:function:1"
        );
        assert_eq!(
            checked.server_functions()[0].parameters()[0]
                .id()
                .to_string(),
            "provisional:parameter:0"
        );
        assert_eq!(
            checked.server_functions()[0].parameters()[1]
                .id()
                .to_string(),
            "provisional:parameter:1"
        );
        assert_eq!(
            checked.server_functions()[1].parameters()[0]
                .id()
                .to_string(),
            "provisional:parameter:2"
        );
    }

    #[test]
    fn preserves_existing_schema_type_field_default_function_and_parameter_identities() {
        let schema_id = SchemaId::from_bytes([1; 16]);
        let type_id = TypeId::from_bytes([2; 16]);
        let field_id = FieldId::from_bytes([3; 16]);
        let default_id = ExpressionId::from_bytes([4; 16]);
        let function_id = FunctionId::from_bytes([5; 16]);
        let parameter_id = ParameterId::from_bytes([6; 16]);
        let base = catalogue(
            vec![schema(1, &["tasks"])],
            vec![object_type(
                2,
                &["tasks", "task"],
                vec![field(
                    3,
                    "title",
                    0,
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    Some(default_id),
                )],
            )],
            vec![server_function(
                5,
                &["tasks", "open"],
                vec![parameter(
                    6,
                    "p_limit",
                    0,
                    ResolvedType::scalar(StandardScalar::Integer),
                )],
                vec![rows_column(
                    "title",
                    0,
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                )],
                FunctionSecurity::Invoker,
                None,
                FunctionVolatility::Volatile,
            )],
        );
        let report = check(
            &bundle([(
                "tasks.orna",
                "CREATE SCHEMA TASKS; CREATE TYPE tasks.task AS OBJECT (title TEXT DEFAULT 'old'); \
                 CREATE SERVER FUNCTION TASKS.OPEN(P_LIMIT INT) RETURNS ROWS (title TEXT) \
                 AS SELECT t.title FROM tasks.task t;",
            )]),
            &base,
        );

        assert!(report.diagnostics().is_empty());
        let checked = report.checked_bundle().unwrap();
        assert_eq!(checked.schemas()[0].id().existing(), Some(schema_id));
        assert_eq!(checked.object_types()[0].id().existing(), Some(type_id));
        assert_eq!(
            checked.object_types()[0].fields()[0].id().existing(),
            Some(field_id)
        );
        assert_eq!(
            checked.object_types()[0].fields()[0]
                .default()
                .unwrap()
                .id()
                .existing(),
            Some(default_id)
        );
        assert_eq!(
            checked.server_functions()[0].id().existing(),
            Some(function_id)
        );
        assert_eq!(
            checked.server_functions()[0].parameters()[0]
                .id()
                .existing(),
            Some(parameter_id)
        );
    }

    #[test]
    fn distinct_new_defaults_receive_distinct_provisional_expression_ids() {
        let report = check(
            &bundle([(
                "defaults.orna",
                "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (first INT DEFAULT 1, second INT DEFAULT 2);",
            )]),
            &empty_catalogue(),
        );

        assert!(report.diagnostics().is_empty());
        let fields = report.checked_bundle().unwrap().object_types()[0].fields();
        assert_eq!(
            fields[0].default().unwrap().id().to_string(),
            "provisional:expression:0"
        );
        assert_eq!(
            fields[1].default().unwrap().id().to_string(),
            "provisional:expression:1"
        );
        assert_ne!(
            fields[0].default().unwrap().id(),
            fields[1].default().unwrap().id()
        );
    }

    #[test]
    fn checked_function_plan_uses_checked_type_and_field_ids() {
        let report = check(
            &bundle([(
                "tasks.orna",
                "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); \
                 CREATE SERVER FUNCTION tasks.open() RETURNS ROWS (title TEXT) \
                 AS SELECT t.title FROM tasks.task t;",
            )]),
            &empty_catalogue(),
        );

        assert!(report.diagnostics().is_empty());
        let checked = report.checked_bundle().unwrap();
        let task = &checked.object_types()[0];
        let title = &task.fields()[0];
        let plan = checked.server_functions()[0].plan();
        assert_eq!(plan.scan().object_type(), task.id());
        let ExpressionKind::FieldPath { steps, .. } = plan.projections()[0].kind() else {
            panic!("expected a field projection");
        };
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].owner(), task.id());
        assert_eq!(steps[0].field(), title.id());
        assert_eq!(
            plan.projections()[0].value_type().semantic_type(),
            title.semantic_type()
        );
    }

    #[test]
    fn records_signature_and_query_references_in_order_with_exact_spans() {
        let source = "CREATE SCHEMA people; CREATE SCHEMA tasks; \
            CREATE TYPE people.person AS OBJECT (name TEXT); \
            CREATE TYPE tasks.task AS OBJECT (assignee REF people.person, completed BOOL NOT NULL); \
            CREATE SERVER FUNCTION tasks.find(p_person REF people.person) \
            RETURNS ROWS (task REF tasks.task, name TEXT) \
            AS SELECT REF(t), t.assignee.name FROM tasks.task t \
            WHERE t.completed = t.completed ORDER BY t.assignee.name DESC;";
        let report = check(&bundle([("references.orna", source)]), &empty_catalogue());

        assert!(report.diagnostics().is_empty());
        let checked = report.checked_bundle().unwrap();
        let person = &checked.object_types()[0];
        let task = &checked.object_types()[1];
        let assignee = &task.fields()[0];
        let completed = &task.fields()[1];
        let name = &person.fields()[0];
        let function = &checked.server_functions()[0];
        let query_start = source.find("SELECT REF(t)").unwrap();
        let assignee_starts = source
            .match_indices("t.assignee.name")
            .map(|(start, _)| start)
            .collect::<Vec<_>>();
        let completed_starts = source
            .match_indices("t.completed")
            .map(|(start, _)| start)
            .collect::<Vec<_>>();
        let parameter_target_start =
            source.find("p_person REF people.person").unwrap() + "p_person REF ".len();
        let return_target_start = source.find("task REF tasks.task").unwrap() + "task REF ".len();
        let query_object_start = query_start + source[query_start..].find("tasks.task").unwrap();
        let object_reference_start =
            query_start + source[query_start..].find("REF(t)").unwrap() + 4;
        let expected = [
            (
                DefinitionReferenceKind::ObjectReference,
                CheckedDefinitionReferenceTarget::ObjectType(person.id()),
                parameter_target_start,
                "people.person".len(),
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                CheckedDefinitionReferenceTarget::ObjectType(task.id()),
                return_target_start,
                "tasks.task".len(),
            ),
            (
                DefinitionReferenceKind::QueryObject,
                CheckedDefinitionReferenceTarget::ObjectType(task.id()),
                query_object_start,
                "tasks.task".len(),
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                CheckedDefinitionReferenceTarget::ObjectType(task.id()),
                object_reference_start,
                1,
            ),
            (
                DefinitionReferenceKind::QueryField,
                CheckedDefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: assignee.id(),
                },
                assignee_starts[0] + 2,
                "assignee".len(),
            ),
            (
                DefinitionReferenceKind::QueryField,
                CheckedDefinitionReferenceTarget::Field {
                    owner: person.id(),
                    field: name.id(),
                },
                assignee_starts[0] + 11,
                "name".len(),
            ),
            (
                DefinitionReferenceKind::QueryField,
                CheckedDefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: completed.id(),
                },
                completed_starts[0] + 2,
                "completed".len(),
            ),
            (
                DefinitionReferenceKind::QueryField,
                CheckedDefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: completed.id(),
                },
                completed_starts[1] + 2,
                "completed".len(),
            ),
            (
                DefinitionReferenceKind::QueryField,
                CheckedDefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: assignee.id(),
                },
                assignee_starts[1] + 2,
                "assignee".len(),
            ),
            (
                DefinitionReferenceKind::QueryField,
                CheckedDefinitionReferenceTarget::Field {
                    owner: person.id(),
                    field: name.id(),
                },
                assignee_starts[1] + 11,
                "name".len(),
            ),
        ];

        assert_eq!(
            function.parameters()[0].location().span().start(),
            source.find("p_person REF").unwrap()
        );
        assert_eq!(
            function.return_columns()[0].location().span().start(),
            source.find("task REF").unwrap()
        );
        assert_eq!(function.references().len(), expected.len());
        for (reference, (kind, target, start, length)) in function.references().iter().zip(expected)
        {
            assert_eq!(reference.kind(), kind);
            assert_eq!(reference.target(), target);
            assert_eq!(reference.location().logical_path(), "references.orna");
            assert_eq!(reference.location().span().start(), start);
            assert_eq!(reference.location().span().end(), start + length);
        }
    }

    #[test]
    fn rejects_duplicates_unknown_names_invalid_references_and_defaults() {
        let report = check(
            &bundle([(
                "invalid.orna",
                "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (\
                     duplicated TEXT, duplicated INT,\
                     unknown missing.type,\
                     ref_scalar REF TEXT,\
                     plain_person people.person ON DELETE RESTRICT,\
                     required_ref REF people.person NOT NULL ON DELETE SET NULL,\
                     bad_default INT DEFAULT TRUE\
                 );\
                 CREATE TYPE people.person AS OBJECT (name TEXT);",
            )]),
            &empty_catalogue(),
        );

        let codes = report
            .diagnostics()
            .iter()
            .map(|diagnostic| diagnostic.code())
            .collect::<Vec<_>>();
        assert!(codes.contains(&DiagnosticCode::DuplicateDefinition));
        assert!(codes.contains(&DiagnosticCode::UnknownQualifiedName));
        assert!(codes.contains(&DiagnosticCode::InvalidReferenceTarget));
        assert!(codes.contains(&DiagnosticCode::TypeMismatch));
        assert_no_checked_bundle(&report);
    }

    #[test]
    fn checked_bundle_contains_only_submitted_schemas_and_object_types() {
        let base = catalogue(
            vec![schema(1, &["people"]), schema(2, &["tasks"])],
            vec![
                object_type(
                    3,
                    &["people", "person"],
                    vec![field(
                        4,
                        "name",
                        0,
                        ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                        None,
                    )],
                ),
                object_type(
                    5,
                    &["tasks", "task"],
                    vec![field(
                        6,
                        "title",
                        0,
                        ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                        None,
                    )],
                ),
            ],
            Vec::new(),
        );
        let report = check(
            &bundle([(
                "schema.orna",
                "CREATE SCHEMA people; CREATE TYPE people.customer AS OBJECT (name TEXT);",
            )]),
            &base,
        );

        assert!(report.diagnostics().is_empty());
        let checked = report.checked_bundle().unwrap();
        assert_eq!(checked.schemas().len(), 1);
        assert_eq!(checked.schemas()[0].name().to_string(), "people");
        assert_eq!(checked.object_types().len(), 1);
        assert_eq!(
            checked.object_types()[0].name().to_string(),
            "people.customer"
        );
        assert_eq!(
            checked.object_types()[0].id().to_string(),
            "provisional:type:0"
        );
        assert!(checked.server_functions().is_empty());
    }

    #[test]
    fn rejects_references_to_base_object_types_omitted_from_the_bundle() {
        let base = catalogue(
            vec![schema(1, &["people"])],
            vec![object_type(
                2,
                &["people", "person"],
                vec![field(
                    3,
                    "name",
                    0,
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    None,
                )],
            )],
            Vec::new(),
        );
        let source = "CREATE SCHEMA tasks; \
            CREATE TYPE tasks.task AS OBJECT (owner REF people.person);";

        let report = check(&bundle([("tasks.orna", source)]), &base);

        assert_eq!(report.diagnostics().len(), 1);
        assert_eq!(
            report.diagnostics()[0].code(),
            DiagnosticCode::UnknownQualifiedName
        );
        assert_eq!(
            report.diagnostics()[0].location().span().start(),
            source.find("people.person").unwrap()
        );
        assert_no_checked_bundle(&report);
    }

    #[test]
    fn rejects_single_return_select_at_the_declared_return() {
        let source = "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (name TEXT); \
            CREATE SERVER FUNCTION people.find() RETURNS TEXT AS SELECT p.name FROM people.person p;";
        let report = check(&bundle([("functions.orna", source)]), &empty_catalogue());

        assert_eq!(report.diagnostics().len(), 1);
        assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);
        assert_eq!(
            report.diagnostics()[0].message(),
            "SELECT SERVER functions require RETURNS ROWS (...)"
        );
        assert_eq!(
            report.diagnostics()[0].location().span().start(),
            source.rfind("TEXT AS").unwrap()
        );
        assert_no_checked_bundle(&report);
    }

    #[test]
    fn rejects_invalid_server_function_headers_before_body_planning() {
        let report = check(
            &bundle([(
                "functions.orna",
                "CREATE SERVER FUNCTION find() RETURNS TEXT AS SELECT TRUE FROM people.person p;\
                 CREATE SCHEMA people;\
                 CREATE SERVER FUNCTION people.find() RETURNS TEXT TRANSACTION MANUAL AS SELECT TRUE FROM people.person p;",
            )]),
            &empty_catalogue(),
        );

        let diagnostics = report.diagnostics();
        assert_eq!(diagnostics[0].code(), DiagnosticCode::UnknownQualifiedName);
        assert_eq!(diagnostics[1].code(), DiagnosticCode::DomainIncompatible);
        assert!(diagnostics.iter().all(|diagnostic| {
            diagnostic.message() != "SERVER function body planning is not implemented"
        }));
        assert_no_checked_bundle(&report);
    }

    #[test]
    fn rejects_duplicate_server_function_names_after_normalisation() {
        let report = check(
            &bundle([(
                "functions.orna",
                "CREATE SCHEMA people;\
                 CREATE SERVER FUNCTION People.Find() RETURNS TEXT AS SELECT TRUE FROM people.person p;\
                 CREATE SERVER FUNCTION people.find() RETURNS TEXT AS SELECT FALSE FROM people.person p;",
            )]),
            &empty_catalogue(),
        );

        let diagnostics = report.diagnostics();
        assert_eq!(diagnostics[0].code(), DiagnosticCode::DuplicateDefinition);
        assert_eq!(diagnostics.len(), 1);
        assert_no_checked_bundle(&report);
    }

    #[test]
    fn accepts_a_checked_server_function_with_a_relational_plan() {
        let source = "CREATE SCHEMA tasks; \
            CREATE SERVER FUNCTION tasks.open(p_limit INT) RETURNS ROWS (title TEXT, completed BOOL) \
            SECURITY DEFINER TRANSACTION READ ONLY VOLATILITY STABLE \
            AS SELECT t.title, t.completed FROM tasks.task t WHERE t.completed = FALSE; \
            CREATE TYPE tasks.task AS OBJECT (title TEXT, completed BOOL NOT NULL);";
        let report = check(&bundle([("functions.orna", source)]), &empty_catalogue());

        assert!(report.diagnostics().is_empty());
        let checked = &report.checked_bundle().unwrap().server_functions()[0];
        assert_eq!(checked.security(), FunctionSecurity::Definer);
        assert_eq!(checked.transaction(), Some(FunctionTransaction::ReadOnly));
        assert_eq!(checked.volatility(), FunctionVolatility::Stable);
        assert_eq!(checked.parameters().len(), 1);
        assert_eq!(checked.return_columns().len(), 2);
        assert_eq!(checked.plan().projections().len(), 2);
        assert!(checked.plan().selection().is_some());
    }

    #[test]
    fn rejects_select_projection_count_and_type_at_rows_declarations() {
        let source = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); \
            CREATE SERVER FUNCTION tasks.count() RETURNS ROWS (first TEXT, second TEXT) \
            AS SELECT t.title FROM tasks.task t; \
            CREATE SERVER FUNCTION tasks.kind() RETURNS ROWS (title BOOL) \
            AS SELECT t.title FROM tasks.task t;";
        let report = check(&bundle([("functions.orna", source)]), &empty_catalogue());

        assert_eq!(report.diagnostics().len(), 2);
        assert_eq!(
            report.diagnostics()[0].message(),
            "SELECT projection count 1 does not match RETURNS ROWS column count 2"
        );
        assert_eq!(
            report.diagnostics()[0].location().span().start(),
            source.find("ROWS (first").unwrap()
        );
        assert_eq!(
            report.diagnostics()[1].message(),
            "SELECT projection 1 type does not match RETURNS ROWS column title"
        );
        assert_eq!(
            report.diagnostics()[1].location().span().start(),
            source.rfind("title BOOL").unwrap()
        );
        assert_no_checked_bundle(&report);
    }

    #[test]
    fn preserves_existing_function_and_reordered_parameter_ids_by_semantic_name() {
        let function_id = FunctionId::from_bytes([4; 16]);
        let parameter_id = ParameterId::from_bytes([5; 16]);
        let offset_parameter_id = ParameterId::from_bytes([6; 16]);
        let base = catalogue(
            vec![schema(1, &["tasks"])],
            vec![object_type(
                2,
                &["tasks", "task"],
                vec![field(
                    3,
                    "title",
                    0,
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    None,
                )],
            )],
            vec![server_function(
                4,
                &["tasks", "open"],
                vec![
                    parameter(
                        5,
                        "p_limit",
                        0,
                        ResolvedType::scalar(StandardScalar::Integer),
                    ),
                    parameter(
                        6,
                        "p_offset",
                        1,
                        ResolvedType::scalar(StandardScalar::Integer),
                    ),
                ],
                vec![rows_column(
                    "title",
                    0,
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                )],
                FunctionSecurity::Invoker,
                None,
                FunctionVolatility::Volatile,
            )],
        );

        let report = check(
            &bundle([(
                "functions.orna",
                "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); \
                 CREATE SERVER FUNCTION tasks.open(p_offset INT, p_limit INT) RETURNS ROWS (title TEXT) \
                 AS SELECT t.title FROM tasks.task t;",
            )]),
            &base,
        );

        assert!(report.diagnostics().is_empty());
        let revised = &report.checked_bundle().unwrap().server_functions()[0];
        assert_eq!(revised.id().existing(), Some(function_id));
        assert_eq!(revised.parameters()[0].name(), "p_offset");
        assert_eq!(
            revised.parameters()[0].id().existing(),
            Some(offset_parameter_id)
        );
        assert_eq!(revised.parameters()[1].name(), "p_limit");
        assert_eq!(revised.parameters()[1].id().existing(), Some(parameter_id));
    }

    #[test]
    fn any_server_function_error_rejects_all_checked_definitions() {
        let source = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); \
            CREATE SERVER FUNCTION tasks.valid() RETURNS ROWS (title TEXT) \
            AS SELECT t.title FROM tasks.task t; \
            CREATE SERVER FUNCTION tasks.invalid() RETURNS ROWS (title BOOL) \
            AS SELECT t.title FROM tasks.task t;";
        let report = check(&bundle([("functions.orna", source)]), &empty_catalogue());

        assert_eq!(report.diagnostics().len(), 1);
        assert_no_checked_bundle(&report);
    }

    #[test]
    fn does_not_add_body_planning_diagnostics_after_object_errors() {
        let report = check(
            &bundle([(
                "functions.orna",
                "CREATE SCHEMA people;\
                 CREATE TYPE people.person AS OBJECT (manager REF missing.person);\
                 CREATE SERVER FUNCTION people.find() RETURNS TEXT AS SELECT TRUE FROM people.person p;",
            )]),
            &empty_catalogue(),
        );

        assert_eq!(report.diagnostics().len(), 1);
        assert_eq!(
            report.diagnostics()[0].code(),
            DiagnosticCode::UnknownQualifiedName
        );
        assert_ne!(
            report.diagnostics()[0].message(),
            "SERVER function body planning is not implemented"
        );
        assert_no_checked_bundle(&report);
    }

    #[test]
    fn rejects_definitions_in_base_schemas_that_are_omitted_from_the_bundle() {
        let base = catalogue(
            vec![schema(1, &["sys"])],
            Vec::new(),
            vec![server_function(
                2,
                &["sys", "health"],
                Vec::new(),
                vec![rows_column(
                    "enabled",
                    0,
                    ResolvedType::scalar(StandardScalar::Boolean),
                )],
                FunctionSecurity::Invoker,
                None,
                FunctionVolatility::Volatile,
            )],
        );

        let report = check(
            &bundle([(
                "functions.orna",
                "CREATE TYPE sys.probe AS OBJECT (enabled BOOL); \
                 CREATE SERVER FUNCTION sys.probe_status() RETURNS ROWS (enabled BOOL) \
                 AS SELECT p.enabled FROM sys.probe p;",
            )]),
            &base,
        );

        assert_eq!(report.diagnostics().len(), 1);
        assert_eq!(
            report.diagnostics()[0].code(),
            DiagnosticCode::UnknownQualifiedName
        );
        assert_no_checked_bundle(&report);
    }

    #[test]
    fn server_function_metadata_preserves_ids_and_maps_modifiers() {
        let base = catalogue(
            vec![schema(1, &["sys"])],
            vec![object_type(
                2,
                &["sys", "health"],
                vec![field(
                    3,
                    "enabled",
                    0,
                    ResolvedType::scalar(StandardScalar::Boolean),
                    None,
                )],
            )],
            vec![server_function(
                4,
                &["sys", "health"],
                Vec::new(),
                vec![rows_column(
                    "enabled",
                    0,
                    ResolvedType::scalar(StandardScalar::Boolean),
                )],
                FunctionSecurity::Invoker,
                None,
                FunctionVolatility::Volatile,
            )],
        );
        let report = check(
            &bundle([(
                "functions.orna",
                "CREATE SCHEMA sys; CREATE TYPE sys.health AS OBJECT (enabled BOOL);\
                 CREATE SERVER FUNCTION Sys.Health() RETURNS ROWS (enabled BOOL) SECURITY DEFINER TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT h.enabled FROM sys.health h;\
                 CREATE SERVER FUNCTION sys.defaults() RETURNS ROWS (enabled BOOL) AS SELECT h.enabled FROM sys.health h;",
            )]),
            &base,
        );

        assert!(report.diagnostics().is_empty());
        let functions = report.checked_bundle().unwrap().server_functions();
        assert_eq!(functions.len(), 2);
        assert_eq!(
            functions[0].id().existing(),
            Some(FunctionId::from_bytes([4; 16]))
        );
        assert_eq!(functions[0].security(), FunctionSecurity::Definer);
        assert_eq!(
            functions[0].transaction(),
            Some(FunctionTransaction::ReadOnly)
        );
        assert_eq!(functions[0].volatility(), FunctionVolatility::Stable);
        assert_eq!(functions[1].id().to_string(), "provisional:function:0");
        assert_eq!(functions[1].security(), FunctionSecurity::Invoker);
        assert_eq!(functions[1].transaction(), None);
        assert_eq!(functions[1].volatility(), FunctionVolatility::Volatile);
    }

    #[test]
    fn rejects_duplicate_server_function_parameters_and_rows_columns() {
        let report = check(
            &bundle([(
                "functions.orna",
                "CREATE SCHEMA people;\
                 CREATE SERVER FUNCTION people.duplicate(p_value TEXT, P_VALUE INT)\
                 RETURNS ROWS (value TEXT, VALUE INT) AS SELECT TRUE FROM people.person p;\
                 CREATE SERVER FUNCTION people.empty() RETURNS ROWS () AS SELECT TRUE FROM people.person p;",
            )]),
            &empty_catalogue(),
        );

        let diagnostics = report.diagnostics();
        assert_eq!(
            diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code() == DiagnosticCode::DuplicateDefinition)
                .count(),
            2
        );
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code() == DiagnosticCode::TypeMismatch
                && diagnostic.message() == "ROWS return type must contain at least one column"
        }));
        assert!(diagnostics.iter().all(|diagnostic| {
            diagnostic.message() != "SERVER function body planning is not implemented"
        }));
        assert_no_checked_bundle(&report);
    }

    #[test]
    fn rejects_server_defaults_and_capabilities_at_their_source() {
        let source = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); \
            CREATE SERVER FUNCTION tasks.find(p_name TEXT DEFAULT 'open') \
            RETURNS ROWS (title TEXT) REQUIRES CAPABILITY sys.fs.read(p_name) \
            AS SELECT t.title FROM tasks.task t;";
        let report = check(&bundle([("functions.orna", source)]), &empty_catalogue());

        assert_eq!(report.diagnostics().len(), 2);
        assert_eq!(
            report.diagnostics()[0].message(),
            "SERVER default-expression planning is not implemented"
        );
        assert_eq!(
            report.diagnostics()[0].location().span().start(),
            source.find("'open'").unwrap()
        );
        assert_eq!(
            report.diagnostics()[1].message(),
            "SERVER capability checking/planning is not implemented"
        );
        assert_eq!(
            report.diagnostics()[1].location().span().start(),
            source.find("sys.fs.read").unwrap()
        );
        assert_no_checked_bundle(&report);
    }

    #[test]
    fn checked_bundle_omits_unsubmitted_base_functions_and_schemas() {
        let base = catalogue(
            vec![schema(1, &["sys"])],
            Vec::new(),
            vec![server_function(
                2,
                &["sys", "health"],
                Vec::new(),
                vec![rows_column(
                    "enabled",
                    0,
                    ResolvedType::scalar(StandardScalar::Boolean),
                )],
                FunctionSecurity::Invoker,
                None,
                FunctionVolatility::Stable,
            )],
        );

        let report = check(
            &bundle([(
                "people.orna",
                "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (name TEXT);",
            )]),
            &base,
        );

        assert!(report.diagnostics().is_empty());
        let checked = report.checked_bundle().unwrap();
        assert!(checked.server_functions().is_empty());
        assert_eq!(checked.schemas().len(), 1);
        assert_eq!(checked.schemas()[0].name().to_string(), "people");
    }

    #[test]
    fn rejects_duplicate_and_unknown_schema_names_after_normalisation() {
        let report = check(
            &bundle([(
                "schemas.orna",
                "CREATE SCHEMA People;\
                 CREATE SCHEMA people;\
                 CREATE TYPE missing.contact AS OBJECT (name TEXT);",
            )]),
            &empty_catalogue(),
        );

        let codes = report
            .diagnostics()
            .iter()
            .map(|diagnostic| diagnostic.code())
            .collect::<Vec<_>>();
        assert!(codes.contains(&DiagnosticCode::DuplicateDefinition));
        assert!(codes.contains(&DiagnosticCode::UnknownQualifiedName));
        assert_no_checked_bundle(&report);
    }
}
