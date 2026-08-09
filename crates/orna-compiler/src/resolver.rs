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
pub(crate) use model::{CheckedFieldRename, CheckedServerFunctionBody, QueryCatalogue, QueryField};

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
    FieldRenameDeclaration, FunctionReturnType, FunctionSecurity as SyntaxFunctionSecurity,
    FunctionTransaction as SyntaxFunctionTransaction,
    FunctionVolatility as SyntaxFunctionVolatility, ObjectTypeDeclaration, OnDeletePolicy,
    QualifiedName, SelectQuantifier, ServerFunctionBody, ServerFunctionDeclaration, SourceSlice,
    SourceSpan, StandardLargeObjectKind, TypeSpecification,
};

use crate::mutation::{
    MutationParameter, MutationReference, check_delete_in, check_insert_in, check_update_in,
};
use crate::relational::{
    ExpressionIr, IdentitySelectedQueryReference, QueryParameter, QueryReference,
    QueryReferenceKind, QueryReferenceTarget, check_distinct_query_in,
    check_identity_selected_query_in, check_query_in,
};
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

#[derive(Clone, Copy)]
struct FieldRenameInput<'a> {
    declaration: &'a FieldRenameDeclaration,
    logical_path: &'a str,
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
    name_span: SourceSpan,
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

    let field_renames = check_field_renames(&parse_report, base, &headers, &mut diagnostics);
    let rename_bindings: HashMap<_, _> = field_renames
        .iter()
        .map(|rename| ((rename.owner, rename.new_name.clone()), rename))
        .collect();

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

            let rename_bound = rename_bindings.get(&(header.id, name.clone()));
            let existing_field = rename_bound
                .and_then(|rename| rename.field.existing())
                .and_then(|id| base_type.and_then(|object_type| object_type.field_by_id(id)))
                .or_else(|| base_type.and_then(|object_type| object_type.field_by_name(&name)));
            let id = assignments.field_id(existing_field.map(|field| field.id()));
            let existing_default = existing_field.and_then(|field| field.default_expression());
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
            field_renames: field_renames
                .into_iter()
                .map(|rename| CheckedFieldRename {
                    owner: rename.owner,
                    field: rename.field,
                    old_name: rename.old_name,
                    new_name: rename.new_name,
                })
                .collect(),
        }),
    }
}

#[derive(Clone)]
struct AcceptedFieldRename {
    owner: CheckedTypeId,
    field: CheckedFieldId,
    old_name: String,
    new_name: String,
}

fn check_field_renames(
    parse_report: &ParseReport,
    base: &CatalogueSnapshot,
    headers: &[Header<'_>],
    diagnostics: &mut Vec<CompilerDiagnostic>,
) -> Vec<AcceptedFieldRename> {
    let candidates: HashMap<_, _> = headers
        .iter()
        .map(|header| (semantic_name(&header.declaration.name), header))
        .collect();
    let mut inputs = Vec::new();
    for unit in parse_report.units() {
        inputs.extend(
            unit.parsed()
                .field_renames()
                .iter()
                .map(|declaration| FieldRenameInput {
                    declaration,
                    logical_path: unit.logical_path(),
                }),
        );
    }
    let mut consumed = HashSet::new();
    let mut produced = HashSet::new();
    let mut valid = Vec::new();
    for input in inputs {
        let owner_name = semantic_name(&input.declaration.type_name);
        let old_name = semantic_part(&input.declaration.old_field_name);
        let new_name = semantic_part(&input.declaration.new_field_name);
        let Some(header) = candidates.get(&owner_name) else {
            diagnostics.push(diagnostic(
                DiagnosticCode::UnknownQualifiedName,
                format!("object type {owner_name} must be declared in this source"),
                input.logical_path,
                &input.declaration.type_name.span,
            ));
            continue;
        };
        let Some(base_type) = base.object_type_by_name(&owner_name) else {
            diagnostics.push(diagnostic(
                DiagnosticCode::UnknownQualifiedName,
                format!("field rename requires existing object type {owner_name}"),
                input.logical_path,
                &input.declaration.type_name.span,
            ));
            continue;
        };
        if old_name == new_name {
            diagnostics.push(diagnostic(
                DiagnosticCode::DuplicateDefinition,
                format!("field {old_name} cannot be renamed to the same name"),
                input.logical_path,
                &input.declaration.old_field_name.span,
            ));
            continue;
        }
        if !consumed.insert((header.id, old_name.clone())) {
            diagnostics.push(diagnostic(
                DiagnosticCode::DuplicateDefinition,
                format!("field {old_name} is renamed more than once"),
                input.logical_path,
                &input.declaration.old_field_name.span,
            ));
            continue;
        }
        if !produced.insert((header.id, new_name.clone())) {
            diagnostics.push(diagnostic(
                DiagnosticCode::DuplicateDefinition,
                format!("more than one field is renamed to {new_name}"),
                input.logical_path,
                &input.declaration.new_field_name.span,
            ));
            continue;
        }
        let final_names: HashSet<_> = header
            .declaration
            .fields
            .iter()
            .map(|field| semantic_part(&field.name))
            .collect();
        valid.push((input, header.id, base_type, final_names, old_name, new_name));
    }

    let mut chained = HashSet::new();
    for index in 0..valid.len() {
        for other_index in index + 1..valid.len() {
            let (input, owner, _, _, old_name, new_name) = &valid[index];
            let (_, other_owner, _, _, other_old_name, other_new_name) = &valid[other_index];
            if owner == other_owner && (new_name == other_old_name || old_name == other_new_name) {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DuplicateDefinition,
                    format!(
                        "field rename chain or swap is not supported: {old_name} to {new_name}"
                    ),
                    input.logical_path,
                    &input.declaration.new_field_name.span,
                ));
                chained.insert(index);
                chained.insert(other_index);
            }
        }
    }
    let mut accepted = Vec::new();
    for (index, (input, owner, base_type, final_names, old_name, new_name)) in
        valid.into_iter().enumerate()
    {
        if chained.contains(&index) {
            continue;
        }
        let owner_name = semantic_name(&input.declaration.type_name);
        if final_names.contains(&old_name) {
            diagnostics.push(diagnostic(
                DiagnosticCode::DuplicateDefinition,
                format!("object type {owner_name} still declares old field {old_name}"),
                input.logical_path,
                &input.declaration.old_field_name.span,
            ));
            continue;
        }
        if !final_names.contains(&new_name) {
            diagnostics.push(diagnostic(
                DiagnosticCode::UnknownQualifiedName,
                format!("object type {owner_name} must declare renamed field {new_name}"),
                input.logical_path,
                &input.declaration.new_field_name.span,
            ));
            continue;
        }
        let old = base_type.field_by_name(&old_name);
        let new = base_type.field_by_name(&new_name);
        let Some(field) = (match (old, new) {
            (Some(_), Some(_)) => {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DuplicateDefinition,
                    format!(
                        "object type {owner_name} already has a different field named {new_name}"
                    ),
                    input.logical_path,
                    &input.declaration.new_field_name.span,
                ));
                None
            }
            (None, None) => {
                diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    format!("object type {owner_name} has no field named {old_name}"),
                    input.logical_path,
                    &input.declaration.old_field_name.span,
                ));
                None
            }
            (Some(field), None) | (None, Some(field)) => Some(field),
        }) else {
            continue;
        };
        accepted.push(AcceptedFieldRename {
            owner,
            field: CheckedFieldId::Existing(field.id()),
            old_name,
            new_name,
        });
    }
    accepted
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
                    "SERVER functions do not yet support TRANSACTION MANUAL",
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
                name_span: parameter.name.span.clone(),
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
                        "SERVER function parameters do not yet support default values",
                        unit.logical_path(),
                        &default.span,
                    ));
                }
            }
            for capability in &declaration.capabilities {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    "SERVER functions do not yet support REQUIRES CAPABILITY",
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
        let (columns, return_location) = match &input.return_type {
            ResolvedServerFunctionReturn::Rows { columns, location } => (columns, location),
            ResolvedServerFunctionReturn::Single { location } => {
                diagnostics.push(DiagnosticCode::semantic(
                    DiagnosticCode::TypeMismatch,
                    format!("{body_name} SERVER functions require RETURNS ROWS (...)"),
                    location.clone(),
                ));
                continue;
            }
        };

        let (body, body_references) = if let Some(query_body) = input.body.as_sql_query() {
            match &query_body.query.quantifier {
                SelectQuantifier::Distinct { .. } => {
                    let query_check = match check_distinct_query_in(
                        &query_body.query,
                        catalogue,
                        input.location.logical_path(),
                    ) {
                        Ok(query_check) => query_check,
                        Err(query_diagnostics) => {
                            diagnostics.extend(query_diagnostics);
                            continue;
                        }
                    };
                    if !query_return_matches(
                        query_check.plan().projections(),
                        columns,
                        return_location,
                        diagnostics,
                    ) {
                        continue;
                    }
                    if !distinct_query_execution_shape_is_valid(input, diagnostics) {
                        continue;
                    }
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
                    if input.parameters.is_empty() && !has_selector {
                        let query_check = match check_query_in(
                            &query_body.query,
                            catalogue,
                            input.location.logical_path(),
                        ) {
                            Ok(query_check) => query_check,
                            Err(query_diagnostics) => {
                                diagnostics.extend(query_diagnostics);
                                continue;
                            }
                        };
                        if !query_return_matches(
                            query_check.plan().projections(),
                            columns,
                            return_location,
                            diagnostics,
                        ) {
                            continue;
                        }
                        (
                            CheckedServerFunctionBody::Query(query_check.plan().clone()),
                            query_check
                                .references()
                                .iter()
                                .map(query_reference)
                                .collect::<Vec<_>>(),
                        )
                    } else {
                        if !identity_selected_query_execution_mode_is_valid(input, diagnostics) {
                            continue;
                        }
                        let parameters = identity_selected_query_parameters(input);
                        let query_check = match check_identity_selected_query_in(
                            &query_body.query,
                            catalogue,
                            input.id,
                            &parameters,
                            input.location.logical_path(),
                        ) {
                            Ok(query_check) => query_check,
                            Err(query_diagnostics) => {
                                diagnostics.extend(query_diagnostics);
                                continue;
                            }
                        };
                        if !query_return_matches(
                            query_check.plan().projections(),
                            columns,
                            return_location,
                            diagnostics,
                        ) {
                            continue;
                        }
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
            if column.semantic_type != SemanticType::Scalar(StandardScalar::Boolean) {
                diagnostics.push(DiagnosticCode::semantic(
                    DiagnosticCode::TypeMismatch,
                    "The RETURNS ROWS (...) column for a DELETE SERVER function must use BOOLEAN",
                    column.location.clone(),
                ));
                continue;
            }
            let parameters = mutation_parameters(input);
            let delete_check = match check_delete_in(
                &delete_body.delete,
                catalogue,
                input.id,
                &parameters,
                input.location.logical_path(),
            ) {
                Ok(delete_check) => delete_check,
                Err(delete_diagnostics) => {
                    diagnostics.extend(delete_diagnostics);
                    continue;
                }
            };
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
                check_insert_in(
                    &insert_body.insert,
                    catalogue,
                    input.id,
                    &parameters,
                    input.location.logical_path(),
                )
            } else if let Some(update_body) = input.body.as_sql_update() {
                check_update_in(
                    &update_body.update,
                    catalogue,
                    input.id,
                    &parameters,
                    input.location.logical_path(),
                )
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

        let mut references = signature_references(&input.parameters, columns);
        references.extend(body_references);
        functions.push(checked_server_function(input, columns, body, references));
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

fn query_return_matches(
    projections: &[ExpressionIr<CheckedTypeId, CheckedFieldId>],
    columns: &[ResolvedServerFunctionReturnColumn],
    return_location: &SourceLocation,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) -> bool {
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

fn mutation_parameters(
    input: &ResolvedServerFunctionInput<'_>,
) -> Vec<MutationParameter<CheckedTypeId, CheckedParameterId>> {
    input
        .parameters
        .iter()
        .map(|parameter| {
            MutationParameter::new(
                parameter.name.clone(),
                parameter.id,
                parameter.semantic_type,
                parameter.name_span.clone(),
            )
        })
        .collect()
}

fn checked_server_function(
    input: &ResolvedServerFunctionInput<'_>,
    columns: &[ResolvedServerFunctionReturnColumn],
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
        body,
        references,
    }
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

fn identity_selected_query_reference(
    reference: &IdentitySelectedQueryReference<
        CheckedTypeId,
        CheckedFieldId,
        CheckedFunctionId,
        CheckedParameterId,
    >,
) -> CheckedDefinitionReference {
    let (target, kind, location) = match reference {
        IdentitySelectedQueryReference::QueryObject {
            object_type,
            location,
        } => (
            CheckedDefinitionReferenceTarget::ObjectType(*object_type),
            DefinitionReferenceKind::QueryObject,
            location,
        ),
        IdentitySelectedQueryReference::ObjectReference {
            object_type,
            location,
        } => (
            CheckedDefinitionReferenceTarget::ObjectType(*object_type),
            DefinitionReferenceKind::ObjectReference,
            location,
        ),
        IdentitySelectedQueryReference::QueryField {
            owner,
            field,
            location,
        } => (
            CheckedDefinitionReferenceTarget::Field {
                owner: *owner,
                field: *field,
            },
            DefinitionReferenceKind::QueryField,
            location,
        ),
        IdentitySelectedQueryReference::ParameterRead {
            owner,
            parameter,
            location,
        } => (
            CheckedDefinitionReferenceTarget::Parameter {
                owner: *owner,
                parameter: *parameter,
            },
            DefinitionReferenceKind::ParameterRead,
            location,
        ),
    };
    CheckedDefinitionReference {
        target,
        kind,
        location: location.clone(),
    }
}

fn mutation_reference(
    reference: &MutationReference<
        CheckedTypeId,
        CheckedFieldId,
        CheckedFunctionId,
        CheckedParameterId,
    >,
) -> CheckedDefinitionReference {
    let (target, kind) = match reference {
        MutationReference::WriteObject { object_type, .. } => (
            CheckedDefinitionReferenceTarget::ObjectType(*object_type),
            DefinitionReferenceKind::WriteObject,
        ),
        MutationReference::WriteField { owner, field, .. } => (
            CheckedDefinitionReferenceTarget::Field {
                owner: *owner,
                field: *field,
            },
            DefinitionReferenceKind::WriteField,
        ),
        MutationReference::ParameterRead {
            owner, parameter, ..
        } => (
            CheckedDefinitionReferenceTarget::Parameter {
                owner: *owner,
                parameter: *parameter,
            },
            DefinitionReferenceKind::ParameterRead,
        ),
        MutationReference::ObjectReference { object_type, .. } => (
            CheckedDefinitionReferenceTarget::ObjectType(*object_type),
            DefinitionReferenceKind::ObjectReference,
        ),
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

    fn rename_base(fields: Vec<FieldDefinition>) -> CatalogueSnapshot {
        catalogue(
            vec![schema(1, &["people"])],
            vec![object_type(2, &["people", "person"], fields)],
            Vec::new(),
        )
    }

    #[test]
    fn field_rename_binds_the_old_identity_default_and_quoted_name() {
        let field_id = FieldId::from_bytes([3; 16]);
        let expression_id = ExpressionId::from_bytes([4; 16]);
        let base = rename_base(vec![field(
            3,
            "Email",
            0,
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            Some(expression_id),
        )]);
        let source = "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (\"Primary Email\" TEXT DEFAULT 'x'); ALTER TYPE people.person RENAME FIELD \"Email\" TO \"Primary Email\";";

        let report = check(&bundle([("rename.orna", source)]), &base);

        assert!(report.diagnostics().is_empty());
        let checked = report.checked_bundle().unwrap();
        let field = &checked.object_types()[0].fields()[0];
        assert_eq!(field.id().existing(), Some(field_id));
        assert_eq!(
            field.default().unwrap().id().existing(),
            Some(expression_id)
        );
        assert_eq!(field.name(), "Primary Email");
        assert_eq!(checked.field_renames().len(), 1);
        assert_eq!(checked.field_renames()[0].old_name, "Email");
        assert_eq!(checked.field_renames()[0].new_name, "Primary Email");
    }

    #[test]
    fn field_rename_is_source_order_independent_and_replay_safe() {
        let field_id = FieldId::from_bytes([3; 16]);
        let base = rename_base(vec![field(
            3,
            "email",
            0,
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            None,
        )]);
        let create_then_alter = "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (primary_email TEXT); ALTER TYPE people.person RENAME FIELD email TO primary_email;";
        let alter_then_create = "ALTER TYPE people.person RENAME FIELD email TO primary_email; CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (primary_email TEXT);";
        let first = check(&bundle([("rename.orna", create_then_alter)]), &base);
        let second = check(&bundle([("rename.orna", alter_then_create)]), &base);
        let first_checked = first.checked_bundle().unwrap();
        let second_checked = second.checked_bundle().unwrap();
        assert_eq!(
            first_checked.object_types()[0].id(),
            second_checked.object_types()[0].id()
        );
        assert_eq!(
            first_checked.object_types()[0].fields()[0].id(),
            second_checked.object_types()[0].fields()[0].id()
        );
        assert_eq!(
            first_checked.field_renames(),
            second_checked.field_renames()
        );
        let replay_base = rename_base(vec![field(
            3,
            "primary_email",
            0,
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            None,
        )]);
        let replay = check(&bundle([("rename.orna", create_then_alter)]), &replay_base);
        assert!(replay.diagnostics().is_empty());
        assert_eq!(
            replay.checked_bundle().unwrap().object_types()[0].fields()[0]
                .id()
                .existing(),
            Some(field_id)
        );
    }

    #[test]
    fn replacing_a_same_shape_field_without_a_rename_is_provisional() {
        let base = rename_base(vec![field(
            3,
            "email",
            0,
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            None,
        )]);
        let report = check(
            &bundle([(
                "rename.orna",
                "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (primary_email TEXT);",
            )]),
            &base,
        );
        assert!(report.diagnostics().is_empty());
        assert!(
            report.checked_bundle().unwrap().object_types()[0].fields()[0]
                .id()
                .is_provisional()
        );
    }

    #[test]
    fn field_rename_rejects_a_base_without_either_name() {
        let base = rename_base(vec![field(
            3,
            "other",
            0,
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            None,
        )]);
        let source = "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (primary_email TEXT); ALTER TYPE people.person RENAME FIELD email TO primary_email;";
        let report = check(&bundle([("rename.orna", source)]), &base);
        assert_eq!(report.diagnostics().len(), 1);
        let diagnostic = &report.diagnostics()[0];
        assert_eq!(diagnostic.code(), DiagnosticCode::UnknownQualifiedName);
        assert_eq!(
            diagnostic.message(),
            "object type people.person has no field named email"
        );
        let old = source.find("RENAME FIELD email").unwrap() + "RENAME FIELD ".len();
        assert_eq!(diagnostic.location().span().start(), old);
        assert_eq!(diagnostic.location().span().end(), old + "email".len());
        assert_no_checked_bundle(&report);
    }

    #[test]
    fn invalid_rename_owners_take_precedence_over_chain_detection() {
        let source = "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (last TEXT); ALTER TYPE people.missing RENAME FIELD email TO first; ALTER TYPE people.missing RENAME FIELD first TO last;";
        let report = check(&bundle([("rename.orna", source)]), &empty_catalogue());
        assert_eq!(report.diagnostics().len(), 2);
        for diagnostic in report.diagnostics() {
            assert_eq!(diagnostic.code(), DiagnosticCode::UnknownQualifiedName);
            assert_eq!(
                diagnostic.message(),
                "object type people.missing must be declared in this source"
            );
        }
        assert_no_checked_bundle(&report);
    }

    #[test]
    fn field_rename_negative_contracts_use_exact_diagnostics() {
        struct Case {
            source: &'static str,
            base: CatalogueSnapshot,
            name: &'static str,
            code: DiagnosticCode,
            message: &'static str,
        }
        let old = || {
            field(
                3,
                "email",
                0,
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                None,
            )
        };
        let new = || {
            field(
                4,
                "primary_email",
                1,
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                None,
            )
        };
        let cases = vec![
            Case {
                source: "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (email TEXT); ALTER TYPE people.person RENAME FIELD email TO email;",
                base: rename_base(vec![old()]),
                name: "email",
                code: DiagnosticCode::DuplicateDefinition,
                message: "field email cannot be renamed to the same name",
            },
            Case {
                source: "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (primary_email TEXT); ALTER TYPE people.person RENAME FIELD email TO primary_email;",
                base: catalogue(vec![schema(1, &["people"])], Vec::new(), Vec::new()),
                name: "people.person",
                code: DiagnosticCode::UnknownQualifiedName,
                message: "field rename requires existing object type people.person",
            },
            Case {
                source: "CREATE SCHEMA people; ALTER TYPE people.person RENAME FIELD email TO primary_email;",
                base: rename_base(vec![old()]),
                name: "people.person",
                code: DiagnosticCode::UnknownQualifiedName,
                message: "object type people.person must be declared in this source",
            },
            Case {
                source: "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (other TEXT); ALTER TYPE people.person RENAME FIELD email TO primary_email;",
                base: rename_base(vec![old()]),
                name: "primary_email",
                code: DiagnosticCode::UnknownQualifiedName,
                message: "object type people.person must declare renamed field primary_email",
            },
            Case {
                source: "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (email TEXT); ALTER TYPE people.person RENAME FIELD email TO primary_email;",
                base: rename_base(vec![old()]),
                name: "email",
                code: DiagnosticCode::DuplicateDefinition,
                message: "object type people.person still declares old field email",
            },
            Case {
                source: "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (primary_email TEXT); ALTER TYPE people.person RENAME FIELD email TO primary_email;",
                base: rename_base(vec![old(), new()]),
                name: "primary_email",
                code: DiagnosticCode::DuplicateDefinition,
                message: "object type people.person already has a different field named primary_email",
            },
            Case {
                source: "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (first TEXT, primary_email TEXT); ALTER TYPE people.person RENAME FIELD email TO primary_email; ALTER TYPE people.person RENAME FIELD email TO first;",
                base: rename_base(vec![old()]),
                name: "email",
                code: DiagnosticCode::DuplicateDefinition,
                message: "field email is renamed more than once",
            },
            Case {
                source: "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (first TEXT, primary_email TEXT); ALTER TYPE people.person RENAME FIELD email TO primary_email; ALTER TYPE people.person RENAME FIELD first TO primary_email;",
                base: rename_base(vec![
                    old(),
                    field(
                        5,
                        "first",
                        1,
                        ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                        None,
                    ),
                ]),
                name: "primary_email",
                code: DiagnosticCode::DuplicateDefinition,
                message: "more than one field is renamed to primary_email",
            },
            Case {
                source: "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (last TEXT); ALTER TYPE people.person RENAME FIELD email TO first; ALTER TYPE people.person RENAME FIELD first TO last;",
                base: rename_base(vec![
                    old(),
                    field(
                        5,
                        "first",
                        1,
                        ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                        None,
                    ),
                ]),
                name: "first",
                code: DiagnosticCode::DuplicateDefinition,
                message: "field rename chain or swap is not supported: email to first",
            },
            Case {
                source: "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (email TEXT, first TEXT); ALTER TYPE people.person RENAME FIELD email TO first; ALTER TYPE people.person RENAME FIELD first TO email;",
                base: rename_base(vec![
                    old(),
                    field(
                        5,
                        "first",
                        1,
                        ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                        None,
                    ),
                ]),
                name: "first",
                code: DiagnosticCode::DuplicateDefinition,
                message: "field rename chain or swap is not supported: email to first",
            },
        ];
        for case in cases {
            let report = check(&bundle([("rename.orna", case.source)]), &case.base);
            assert_eq!(report.diagnostics().len(), 1, "{}", case.message);
            let diagnostic = &report.diagnostics()[0];
            assert_eq!(diagnostic.code(), case.code, "{}", case.source);
            assert_eq!(diagnostic.message(), case.message);
            let start = if case.message == "field email cannot be renamed to the same name"
                || case.message == "field email is renamed more than once"
                || case.message == "object type people.person still declares old field email"
            {
                case.source.rfind("RENAME FIELD email").unwrap() + "RENAME FIELD ".len()
            } else if case.message == "more than one field is renamed to primary_email" {
                case.source
                    .rfind("RENAME FIELD first TO primary_email")
                    .unwrap()
                    + "RENAME FIELD first TO ".len()
            } else if case.message.starts_with("field rename chain or swap") {
                case.source.find("RENAME FIELD email TO").unwrap() + "RENAME FIELD email TO ".len()
            } else {
                case.source.rfind(case.name).unwrap()
            };
            assert_eq!(
                diagnostic.location().span().start(),
                start,
                "{}",
                case.source
            );
            assert_eq!(diagnostic.location().span().end(), start + case.name.len());
            assert_no_checked_bundle(&report);
        }
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
            CREATE SERVER FUNCTION alpha.first(p_one REF alpha.one) \
            RETURNS ROWS (number INT) TRANSACTION READ ONLY VOLATILITY STABLE \
            AS SELECT o.number FROM alpha.one o WHERE REF(o) = p_one; \
            CREATE SERVER FUNCTION beta.second(p_two REF beta.two) \
            RETURNS ROWS (number INT) TRANSACTION READ ONLY VOLATILITY STABLE \
            AS SELECT t.number FROM beta.two t WHERE REF(t) = p_two;";
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
            checked.server_functions()[1].parameters()[0]
                .id()
                .to_string(),
            "provisional:parameter:1"
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
                vec![parameter(6, "p_task", 0, ResolvedType::reference(type_id))],
                vec![rows_column(
                    "title",
                    0,
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                )],
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::ReadOnly),
                FunctionVolatility::Stable,
            )],
        );
        let report = check(
            &bundle([(
                "tasks.orna",
                "CREATE SCHEMA TASKS; CREATE TYPE tasks.task AS OBJECT (title TEXT DEFAULT 'old'); \
                 CREATE SERVER FUNCTION TASKS.OPEN(P_TASK REF tasks.task) RETURNS ROWS (title TEXT) \
                 SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE \
                 AS SELECT t.title FROM tasks.task t WHERE REF(t) = P_TASK;",
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
        let plan = checked.server_functions()[0]
            .query_plan()
            .expect("fixture has a SELECT body");
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
    fn records_signature_and_identity_selected_query_references_in_order_with_exact_spans() {
        let source = "CREATE SCHEMA people; CREATE SCHEMA tasks; \
            CREATE TYPE people.person AS OBJECT (name TEXT); \
            CREATE TYPE tasks.task AS OBJECT (assignee REF people.person, completed BOOL NOT NULL); \
            CREATE SERVER FUNCTION tasks.find(p_task REF tasks.task) \
            RETURNS ROWS (task REF tasks.task, name TEXT) \
            SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE \
            AS SELECT REF(t), t.assignee.name FROM tasks.task t \
            WHERE REF(t) = p_task;";
        let report = check(&bundle([("references.orna", source)]), &empty_catalogue());

        assert!(report.diagnostics().is_empty());
        let checked = report.checked_bundle().unwrap();
        let person = &checked.object_types()[0];
        let task = &checked.object_types()[1];
        let assignee = &task.fields()[0];
        let name = &person.fields()[0];
        let function = &checked.server_functions()[0];
        let plan = function
            .identity_selected_query_plan()
            .expect("fixture has an identity-selected SELECT body");
        assert!(function.query_plan().is_none());
        assert!(function.distinct_query_plan().is_none());
        assert_eq!(plan.scan().object_type(), task.id());
        assert_eq!(plan.selector().owner(), function.id());
        assert_eq!(plan.selector().parameter(), function.parameters()[0].id());
        assert_eq!(plan.projections().len(), 2);
        let query_start = source.find("SELECT REF(t)").unwrap();
        let assignee_start = source.find("t.assignee.name").unwrap();
        let parameter_target_start =
            source.find("p_task REF tasks.task").unwrap() + "p_task REF ".len();
        let return_target_start = source.find("RETURNS ROWS (task REF tasks.task").unwrap()
            + "RETURNS ROWS (task REF ".len();
        let query_object_start = query_start + source[query_start..].find("tasks.task").unwrap();
        let projection_reference_start =
            query_start + source[query_start..].find("REF(t)").unwrap() + 4;
        let selector_reference_start = source.rfind("REF(t)").unwrap() + 4;
        let parameter_read_start = source.rfind("p_task").unwrap();
        let expected = [
            (
                DefinitionReferenceKind::ObjectReference,
                CheckedDefinitionReferenceTarget::ObjectType(task.id()),
                parameter_target_start,
                "tasks.task".len(),
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
                projection_reference_start,
                1,
            ),
            (
                DefinitionReferenceKind::QueryField,
                CheckedDefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: assignee.id(),
                },
                assignee_start + 2,
                "assignee".len(),
            ),
            (
                DefinitionReferenceKind::QueryField,
                CheckedDefinitionReferenceTarget::Field {
                    owner: person.id(),
                    field: name.id(),
                },
                assignee_start + 11,
                "name".len(),
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                CheckedDefinitionReferenceTarget::ObjectType(task.id()),
                selector_reference_start,
                1,
            ),
            (
                DefinitionReferenceKind::ParameterRead,
                CheckedDefinitionReferenceTarget::Parameter {
                    owner: function.id(),
                    parameter: function.parameters()[0].id(),
                },
                parameter_read_start,
                "p_task".len(),
            ),
        ];

        assert_eq!(
            function.parameters()[0].location().span().start(),
            source.find("p_task REF").unwrap()
        );
        assert_eq!(
            function.return_columns()[0].location().span().start(),
            source.find("RETURNS ROWS (task REF").unwrap() + "RETURNS ROWS (".len()
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
    fn preserves_v1_signature_and_query_references_in_order_with_exact_spans() {
        let source = "CREATE SCHEMA people; CREATE SCHEMA tasks; \
            CREATE TYPE people.person AS OBJECT (name TEXT); \
            CREATE TYPE tasks.task AS OBJECT (assignee REF people.person, completed BOOL NOT NULL); \
            CREATE SERVER FUNCTION tasks.find() \
            RETURNS ROWS (task REF tasks.task, name TEXT) \
            AS SELECT REF(t), t.assignee.name FROM tasks.task t \
            WHERE t.completed = t.completed ORDER BY t.assignee.name DESC;";
        let report = check(
            &bundle([("v1_references.orna", source)]),
            &empty_catalogue(),
        );

        assert!(report.diagnostics().is_empty());
        let checked = report.checked_bundle().unwrap();
        let person = &checked.object_types()[0];
        let task = &checked.object_types()[1];
        let assignee = &task.fields()[0];
        let completed = &task.fields()[1];
        let name = &person.fields()[0];
        let function = &checked.server_functions()[0];
        assert!(function.query_plan().is_some());
        assert!(function.identity_selected_query_plan().is_none());
        assert!(function.distinct_query_plan().is_none());
        let query_start = source.find("SELECT REF(t)").unwrap();
        let assignee_starts = source
            .match_indices("t.assignee.name")
            .map(|(start, _)| start)
            .collect::<Vec<_>>();
        let completed_starts = source
            .match_indices("t.completed")
            .map(|(start, _)| start)
            .collect::<Vec<_>>();
        let return_target_start = source.find("task REF tasks.task").unwrap() + "task REF ".len();
        let query_object_start = query_start + source[query_start..].find("tasks.task").unwrap();
        let object_reference_start =
            query_start + source[query_start..].find("REF(t)").unwrap() + 4;
        let expected = [
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
            function.return_columns()[0].location().span().start(),
            source.find("task REF").unwrap()
        );
        assert_eq!(function.references().len(), expected.len());
        for (reference, (kind, target, start, length)) in function.references().iter().zip(expected)
        {
            assert_eq!(reference.kind(), kind);
            assert_eq!(reference.target(), target);
            assert_eq!(reference.location().logical_path(), "v1_references.orna");
            assert_eq!(reference.location().span().start(), start);
            assert_eq!(reference.location().span().end(), start + length);
        }
    }

    #[test]
    fn records_direct_boolean_predicate_paths_after_projections_with_exact_spans() {
        let source = "CREATE SCHEMA people; CREATE SCHEMA tasks; \
            CREATE TYPE people.person AS OBJECT (active BOOL NOT NULL); \
            CREATE TYPE tasks.task AS OBJECT (owner REF people.person, enabled BOOL NOT NULL); \
            CREATE SERVER FUNCTION tasks.enabled() RETURNS ROWS (enabled BOOL) \
            AS SELECT t.enabled FROM tasks.task t WHERE t.enabled; \
            CREATE SERVER FUNCTION tasks.active() RETURNS ROWS (active BOOL) \
            AS SELECT t.owner.active FROM tasks.task t WHERE t.owner.active;";
        let report = check(
            &bundle([("direct_predicates.orna", source)]),
            &empty_catalogue(),
        );

        assert!(
            report.diagnostics().is_empty(),
            "{:?}",
            report.diagnostics()
        );
        let checked = report
            .checked_bundle()
            .expect("direct predicates must check");
        let person = &checked.object_types()[0];
        let task = &checked.object_types()[1];
        let owner = &task.fields()[0];
        let enabled = &task.fields()[1];
        let active = &person.fields()[0];
        let enabled_function = &checked.server_functions()[0];
        let active_function = &checked.server_functions()[1];

        let enabled_starts = source
            .match_indices("t.enabled")
            .map(|(start, _)| start)
            .collect::<Vec<_>>();
        assert_eq!(enabled_starts.len(), 2);
        assert_eq!(
            enabled_function
                .references()
                .iter()
                .map(|reference| (reference.kind(), reference.target()))
                .collect::<Vec<_>>(),
            vec![
                (
                    DefinitionReferenceKind::QueryObject,
                    CheckedDefinitionReferenceTarget::ObjectType(task.id()),
                ),
                (
                    DefinitionReferenceKind::QueryField,
                    CheckedDefinitionReferenceTarget::Field {
                        owner: task.id(),
                        field: enabled.id(),
                    },
                ),
                (
                    DefinitionReferenceKind::QueryField,
                    CheckedDefinitionReferenceTarget::Field {
                        owner: task.id(),
                        field: enabled.id(),
                    },
                ),
            ]
        );
        for (reference, start) in enabled_function
            .references()
            .iter()
            .skip(1)
            .zip(enabled_starts)
        {
            assert_eq!(
                reference.location().logical_path(),
                "direct_predicates.orna"
            );
            assert_eq!(reference.location().span().start(), start + "t.".len());
            assert_eq!(reference.location().span().end(), start + "t.enabled".len());
        }

        let active_starts = source
            .match_indices("t.owner.active")
            .map(|(start, _)| start)
            .collect::<Vec<_>>();
        assert_eq!(active_starts.len(), 2);
        assert_eq!(
            active_function
                .references()
                .iter()
                .map(|reference| (reference.kind(), reference.target()))
                .collect::<Vec<_>>(),
            vec![
                (
                    DefinitionReferenceKind::QueryObject,
                    CheckedDefinitionReferenceTarget::ObjectType(task.id()),
                ),
                (
                    DefinitionReferenceKind::QueryField,
                    CheckedDefinitionReferenceTarget::Field {
                        owner: task.id(),
                        field: owner.id(),
                    },
                ),
                (
                    DefinitionReferenceKind::QueryField,
                    CheckedDefinitionReferenceTarget::Field {
                        owner: person.id(),
                        field: active.id(),
                    },
                ),
                (
                    DefinitionReferenceKind::QueryField,
                    CheckedDefinitionReferenceTarget::Field {
                        owner: task.id(),
                        field: owner.id(),
                    },
                ),
                (
                    DefinitionReferenceKind::QueryField,
                    CheckedDefinitionReferenceTarget::Field {
                        owner: person.id(),
                        field: active.id(),
                    },
                ),
            ]
        );
        let active_plan = active_function
            .query_plan()
            .expect("direct Boolean function must use the v1 query plan");
        assert!(active_plan.selection().is_some());
        assert!(active_plan.selection().unwrap().value_type().nullable());
        let expected_spans = [
            (active_starts[0] + 2, "owner".len()),
            (active_starts[0] + 8, "active".len()),
            (active_starts[1] + 2, "owner".len()),
            (active_starts[1] + 8, "active".len()),
        ];
        for (reference, (start, length)) in active_function
            .references()
            .iter()
            .skip(1)
            .zip(expected_spans)
        {
            assert_eq!(reference.location().span().start(), start);
            assert_eq!(reference.location().span().end(), start + length);
        }
    }

    #[test]
    fn direct_boolean_literals_add_no_predicate_references() {
        let source = "CREATE SCHEMA tasks; \
            CREATE TYPE tasks.task AS OBJECT (enabled BOOL NOT NULL); \
            CREATE SERVER FUNCTION tasks.all_tasks() RETURNS ROWS (enabled BOOL) \
            AS SELECT t.enabled FROM tasks.task t WHERE TRUE; \
            CREATE SERVER FUNCTION tasks.no_tasks() RETURNS ROWS (enabled BOOL) \
            AS SELECT t.enabled FROM tasks.task t WHERE FALSE;";
        let report = check(&bundle([("literals.orna", source)]), &empty_catalogue());

        assert!(
            report.diagnostics().is_empty(),
            "{:?}",
            report.diagnostics()
        );
        let checked = report
            .checked_bundle()
            .expect("literal predicates must check");
        let task = &checked.object_types()[0];
        let enabled = &task.fields()[0];
        for function in checked.server_functions() {
            assert_eq!(
                function
                    .references()
                    .iter()
                    .map(|reference| (reference.kind(), reference.target()))
                    .collect::<Vec<_>>(),
                vec![
                    (
                        DefinitionReferenceKind::QueryObject,
                        CheckedDefinitionReferenceTarget::ObjectType(task.id()),
                    ),
                    (
                        DefinitionReferenceKind::QueryField,
                        CheckedDefinitionReferenceTarget::Field {
                            owner: task.id(),
                            field: enabled.id(),
                        },
                    ),
                ]
            );
        }
    }

    #[test]
    fn rejects_non_boolean_direct_predicates_at_the_complete_predicate() {
        let source = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); \
            CREATE SERVER FUNCTION tasks.bad() RETURNS ROWS (title TEXT) \
            AS SELECT t.title FROM tasks.task t WHERE t.title;";
        let report = check(&bundle([("direct_type.orna", source)]), &empty_catalogue());

        assert_no_checked_bundle(&report);
        assert_eq!(report.diagnostics().len(), 1);
        let diagnostic = &report.diagnostics()[0];
        assert_eq!(diagnostic.code(), DiagnosticCode::TypeMismatch);
        assert_eq!(diagnostic.message(), "WHERE requires a BOOLEAN expression");
        let predicate_start = source.rfind("t.title").expect("predicate exists");
        assert_eq!(diagnostic.location().logical_path(), "direct_type.orna");
        assert_eq!(diagnostic.location().span().start(), predicate_start);
        assert_eq!(
            diagnostic.location().span().end(),
            predicate_start + "t.title".len()
        );
    }

    #[test]
    fn rejects_parameterised_direct_predicates_through_the_identity_selector_boundary() {
        let source = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (enabled BOOL NOT NULL); \
            CREATE SERVER FUNCTION tasks.bad(p_task REF tasks.task) RETURNS ROWS (enabled BOOL) \
            SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE \
            AS SELECT t.enabled FROM tasks.task t WHERE t.enabled;";
        let report = check(
            &bundle([("parameter_direct.orna", source)]),
            &empty_catalogue(),
        );

        assert_no_checked_bundle(&report);
        assert_eq!(report.diagnostics().len(), 1);
        let diagnostic = &report.diagnostics()[0];
        assert_eq!(diagnostic.code(), DiagnosticCode::DomainIncompatible);
        assert_eq!(
            diagnostic.message(),
            "parameterised SELECT SERVER functions require WHERE REF(source_alias) = selector_parameter"
        );
        let predicate_start = source.rfind("t.enabled").expect("predicate exists");
        assert_eq!(
            diagnostic.location().logical_path(),
            "parameter_direct.orna"
        );
        assert_eq!(diagnostic.location().span().start(), predicate_start);
        assert_eq!(
            diagnostic.location().span().end(),
            predicate_start + "t.enabled".len()
        );
    }

    #[test]
    fn checks_distinct_query_identities_and_orders_signature_then_body_evidence() {
        let source = "CREATE SCHEMA people; CREATE SCHEMA tasks; \
            CREATE TYPE people.person AS OBJECT (active BOOL NOT NULL); \
            CREATE TYPE tasks.task AS OBJECT (assignee REF people.person, completed BOOL NOT NULL); \
            CREATE SERVER FUNCTION tasks.values() \
            RETURNS ROWS (task REF tasks.task, active BOOL, completed BOOL) \
            SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE \
            AS SELECT DISTINCT REF(t), t.assignee.active, t.completed FROM tasks.task t \
            WHERE t.completed = t.completed;";
        let report = check(
            &bundle([("distinct_references.orna", source)]),
            &empty_catalogue(),
        );

        assert!(report.diagnostics().is_empty());
        let checked = report.checked_bundle().unwrap();
        let person = &checked.object_types()[0];
        let task = &checked.object_types()[1];
        let active = &person.fields()[0];
        let assignee = &task.fields()[0];
        let completed = &task.fields()[1];
        let function = &checked.server_functions()[0];
        let plan = function
            .distinct_query_plan()
            .expect("fixture has a DISTINCT SELECT body");
        assert!(function.query_plan().is_none());
        assert!(function.identity_selected_query_plan().is_none());
        assert_eq!(plan.scan().object_type(), task.id());
        assert_eq!(plan.projections().len(), 3);
        assert!(!plan.projections()[0].value_type().nullable());
        assert!(plan.projections()[1].value_type().nullable());
        assert!(!plan.projections()[2].value_type().nullable());
        assert!(plan.selection().is_some());
        let ExpressionKind::FieldPath { steps, .. } = plan.projections()[1].kind() else {
            panic!("second DISTINCT projection must be a field path");
        };
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].owner(), task.id());
        assert_eq!(steps[0].field(), assignee.id());
        assert_eq!(steps[1].owner(), person.id());
        assert_eq!(steps[1].field(), active.id());

        let query_start = source.find("SELECT DISTINCT").unwrap();
        let query_object_start = query_start + source[query_start..].find("tasks.task").unwrap();
        let projection_reference_start =
            query_start + source[query_start..].find("REF(t)").unwrap() + "REF(".len();
        let assignee_start = source.find("t.assignee.active").unwrap();
        let completed_starts = source
            .match_indices("t.completed")
            .map(|(start, _)| start)
            .collect::<Vec<_>>();
        let return_target_start = source.find("task REF tasks.task").unwrap() + "task REF ".len();
        let expected = [
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
                projection_reference_start,
                1,
            ),
            (
                DefinitionReferenceKind::QueryField,
                CheckedDefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: assignee.id(),
                },
                assignee_start + "t.".len(),
                "assignee".len(),
            ),
            (
                DefinitionReferenceKind::QueryField,
                CheckedDefinitionReferenceTarget::Field {
                    owner: person.id(),
                    field: active.id(),
                },
                assignee_start + "t.assignee.".len(),
                "active".len(),
            ),
            (
                DefinitionReferenceKind::QueryField,
                CheckedDefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: completed.id(),
                },
                completed_starts[0] + "t.".len(),
                "completed".len(),
            ),
            (
                DefinitionReferenceKind::QueryField,
                CheckedDefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: completed.id(),
                },
                completed_starts[1] + "t.".len(),
                "completed".len(),
            ),
            (
                DefinitionReferenceKind::QueryField,
                CheckedDefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: completed.id(),
                },
                completed_starts[2] + "t.".len(),
                "completed".len(),
            ),
        ];

        assert_eq!(function.references().len(), expected.len());
        for (reference, (kind, target, start, length)) in function.references().iter().zip(expected)
        {
            assert_eq!(reference.kind(), kind);
            assert_eq!(reference.target(), target);
            assert_eq!(
                reference.location().logical_path(),
                "distinct_references.orna"
            );
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
        assert_eq!(
            diagnostics[1].message(),
            "SERVER functions do not yet support TRANSACTION MANUAL"
        );
        assert!(diagnostics.iter().all(|diagnostic| {
            diagnostic.message() != "SERVER functions do not yet support this body form"
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
            CREATE SERVER FUNCTION tasks.open() RETURNS ROWS (title TEXT, completed BOOL) \
            SECURITY DEFINER TRANSACTION READ ONLY VOLATILITY STABLE \
            AS SELECT t.title, t.completed FROM tasks.task t WHERE t.completed = FALSE; \
            CREATE TYPE tasks.task AS OBJECT (title TEXT, completed BOOL NOT NULL);";
        let report = check(&bundle([("functions.orna", source)]), &empty_catalogue());

        assert!(report.diagnostics().is_empty());
        let checked = &report.checked_bundle().unwrap().server_functions()[0];
        assert_eq!(checked.security(), FunctionSecurity::Definer);
        assert_eq!(checked.transaction(), Some(FunctionTransaction::ReadOnly));
        assert_eq!(checked.volatility(), FunctionVolatility::Stable);
        assert!(checked.parameters().is_empty());
        assert_eq!(checked.return_columns().len(), 2);
        let plan = checked.query_plan().expect("fixture has a SELECT body");
        assert_eq!(plan.projections().len(), 2);
        assert!(plan.selection().is_some());
    }

    #[test]
    fn checks_server_insert_with_exact_body_identities_and_evidence() {
        let source = "CREATE SCHEMA tasks; \
            CREATE TYPE tasks.person AS OBJECT (name TEXT NOT NULL); \
            CREATE TYPE tasks.task AS OBJECT (title TEXT NOT NULL, done BOOL NOT NULL, note TEXT, owner REF tasks.person); \
            CREATE SERVER FUNCTION tasks.create(p_title TEXT, p_unused INT, p_owner REF tasks.person) \
            RETURNS ROWS (result REF tasks.task) TRANSACTION ATOMIC \
            AS INSERT INTO tasks.task AS created (title, done, note, owner) \
            VALUES (p_title, FALSE, NULL, p_owner) RETURNING REF(created);";
        let report = check(&bundle([("functions.orna", source)]), &empty_catalogue());

        assert!(report.diagnostics().is_empty());
        let checked = &report.checked_bundle().unwrap().server_functions()[0];
        assert!(checked.query_plan().is_none());
        let task = &report.checked_bundle().unwrap().object_types()[1];
        let person = &report.checked_bundle().unwrap().object_types()[0];
        let plan = checked.mutation_plan().expect("expected an INSERT body");
        assert_eq!(plan.target_object(), task.id());
        assert_eq!(plan.returned_object(), task.id());
        assert_eq!(plan.assignments().len(), 4);
        assert_eq!(plan.assignments()[0].field(), task.fields()[0].id());
        assert_eq!(plan.assignments()[1].field(), task.fields()[1].id());
        assert_eq!(plan.assignments()[2].field(), task.fields()[2].id());
        assert_eq!(plan.assignments()[3].field(), task.fields()[3].id());
        assert_eq!(checked.return_columns()[0].name(), "result");
        assert_eq!(checked.security(), FunctionSecurity::Invoker);
        assert_eq!(checked.transaction(), Some(FunctionTransaction::Atomic));
        assert_eq!(checked.volatility(), FunctionVolatility::Volatile);

        let parameter_ids = checked
            .parameters()
            .iter()
            .map(|parameter| parameter.id())
            .collect::<Vec<_>>();
        assert_eq!(
            checked
                .references()
                .iter()
                .map(|reference| (reference.kind(), reference.target()))
                .collect::<Vec<_>>(),
            vec![
                (
                    DefinitionReferenceKind::ObjectReference,
                    CheckedDefinitionReferenceTarget::ObjectType(person.id()),
                ),
                (
                    DefinitionReferenceKind::ObjectReference,
                    CheckedDefinitionReferenceTarget::ObjectType(task.id()),
                ),
                (
                    DefinitionReferenceKind::WriteObject,
                    CheckedDefinitionReferenceTarget::ObjectType(task.id()),
                ),
                (
                    DefinitionReferenceKind::WriteField,
                    CheckedDefinitionReferenceTarget::Field {
                        owner: task.id(),
                        field: task.fields()[0].id()
                    },
                ),
                (
                    DefinitionReferenceKind::ParameterRead,
                    CheckedDefinitionReferenceTarget::Parameter {
                        owner: checked.id(),
                        parameter: parameter_ids[0]
                    },
                ),
                (
                    DefinitionReferenceKind::WriteField,
                    CheckedDefinitionReferenceTarget::Field {
                        owner: task.id(),
                        field: task.fields()[1].id()
                    },
                ),
                (
                    DefinitionReferenceKind::WriteField,
                    CheckedDefinitionReferenceTarget::Field {
                        owner: task.id(),
                        field: task.fields()[2].id()
                    },
                ),
                (
                    DefinitionReferenceKind::WriteField,
                    CheckedDefinitionReferenceTarget::Field {
                        owner: task.id(),
                        field: task.fields()[3].id()
                    },
                ),
                (
                    DefinitionReferenceKind::ParameterRead,
                    CheckedDefinitionReferenceTarget::Parameter {
                        owner: checked.id(),
                        parameter: parameter_ids[2]
                    },
                ),
                (
                    DefinitionReferenceKind::ObjectReference,
                    CheckedDefinitionReferenceTarget::ObjectType(task.id()),
                ),
            ]
        );
        assert!(
            checked
                .references()
                .iter()
                .all(|reference| reference.location().logical_path() == "functions.orna")
        );
        assert_eq!(
            checked
                .references()
                .iter()
                .map(|reference| {
                    (
                        reference.location().span().start(),
                        reference.location().span().end(),
                    )
                })
                .collect::<Vec<_>>(),
            vec![
                {
                    let start =
                        source.find("p_owner REF tasks.person").unwrap() + "p_owner REF ".len();
                    (start, start + "tasks.person".len())
                },
                {
                    let start = source.find("result REF tasks.task").unwrap() + "result REF ".len();
                    (start, start + "tasks.task".len())
                },
                {
                    let start = source.rfind("tasks.task AS created").unwrap();
                    (start, start + "tasks.task".len())
                },
                {
                    let start = source.rfind("(title, done").unwrap() + 1;
                    (start, start + "title".len())
                },
                {
                    let start = source.rfind("p_title").unwrap();
                    (start, start + "p_title".len())
                },
                {
                    let start = source.rfind("done, note").unwrap();
                    (start, start + "done".len())
                },
                {
                    let start = source.rfind("note, owner").unwrap();
                    (start, start + "note".len())
                },
                {
                    let start = source.rfind("note, owner)").unwrap() + "note, ".len();
                    (start, start + "owner".len())
                },
                {
                    let start = source.rfind("p_owner").unwrap();
                    (start, start + "p_owner".len())
                },
                {
                    let start = source.rfind("created)").unwrap();
                    (start, start + "created".len())
                },
            ]
        );
    }

    #[test]
    fn checks_server_update_with_selector_and_exact_evidence_order() {
        let source = "CREATE SCHEMA tasks; \
            CREATE TYPE tasks.person AS OBJECT (name TEXT NOT NULL); \
            CREATE TYPE tasks.task AS OBJECT (title TEXT NOT NULL, done BOOL NOT NULL, owner REF tasks.person); \
            CREATE SERVER FUNCTION tasks.update(p_task REF tasks.task, p_title TEXT, p_owner REF tasks.person) \
            RETURNS ROWS (updated REF tasks.task) TRANSACTION ATOMIC \
            AS UPDATE tasks.task AS changed SET title = p_title, owner = p_owner \
            WHERE REF(changed) = p_task RETURNING REF(changed);";
        let report = check(&bundle([("functions.orna", source)]), &empty_catalogue());

        assert!(
            report.diagnostics().is_empty(),
            "{:?}",
            report.diagnostics()
        );
        let bundle = report.checked_bundle().unwrap();
        let checked = &bundle.server_functions()[0];
        let person = &bundle.object_types()[0];
        let task = &bundle.object_types()[1];
        let plan = checked.mutation_plan().expect("expected an UPDATE body");
        let parameters = checked.parameters();
        assert_eq!(
            plan.operation(),
            &crate::mutation::MutationOperation::Update {
                selector_owner: checked.id(),
                selector_parameter: parameters[0].id(),
            }
        );
        assert_eq!(plan.target_object(), task.id());
        assert_eq!(plan.returned_object(), task.id());
        assert_eq!(plan.assignments().len(), 2);
        assert_eq!(plan.assignments()[0].field(), task.fields()[0].id());
        assert_eq!(plan.assignments()[1].field(), task.fields()[2].id());
        assert_eq!(
            checked
                .references()
                .iter()
                .map(|reference| (reference.kind(), reference.target()))
                .collect::<Vec<_>>(),
            vec![
                (
                    DefinitionReferenceKind::ObjectReference,
                    CheckedDefinitionReferenceTarget::ObjectType(task.id()),
                ),
                (
                    DefinitionReferenceKind::ObjectReference,
                    CheckedDefinitionReferenceTarget::ObjectType(person.id()),
                ),
                (
                    DefinitionReferenceKind::ObjectReference,
                    CheckedDefinitionReferenceTarget::ObjectType(task.id()),
                ),
                (
                    DefinitionReferenceKind::WriteObject,
                    CheckedDefinitionReferenceTarget::ObjectType(task.id()),
                ),
                (
                    DefinitionReferenceKind::WriteField,
                    CheckedDefinitionReferenceTarget::Field {
                        owner: task.id(),
                        field: task.fields()[0].id(),
                    },
                ),
                (
                    DefinitionReferenceKind::ParameterRead,
                    CheckedDefinitionReferenceTarget::Parameter {
                        owner: checked.id(),
                        parameter: parameters[1].id(),
                    },
                ),
                (
                    DefinitionReferenceKind::WriteField,
                    CheckedDefinitionReferenceTarget::Field {
                        owner: task.id(),
                        field: task.fields()[2].id(),
                    },
                ),
                (
                    DefinitionReferenceKind::ParameterRead,
                    CheckedDefinitionReferenceTarget::Parameter {
                        owner: checked.id(),
                        parameter: parameters[2].id(),
                    },
                ),
                (
                    DefinitionReferenceKind::ObjectReference,
                    CheckedDefinitionReferenceTarget::ObjectType(task.id()),
                ),
                (
                    DefinitionReferenceKind::ParameterRead,
                    CheckedDefinitionReferenceTarget::Parameter {
                        owner: checked.id(),
                        parameter: parameters[0].id(),
                    },
                ),
                (
                    DefinitionReferenceKind::ObjectReference,
                    CheckedDefinitionReferenceTarget::ObjectType(task.id()),
                ),
            ]
        );
        let token_span = |context: &str, prefix: &str, token: &str| {
            let context_start = source.find(context).unwrap();
            let start = context_start + prefix.len();
            (start, start + token.len())
        };
        assert_eq!(
            checked
                .references()
                .iter()
                .map(|reference| {
                    (
                        reference.location().span().start(),
                        reference.location().span().end(),
                    )
                })
                .collect::<Vec<_>>(),
            vec![
                token_span("p_task REF tasks.task", "p_task REF ", "tasks.task"),
                token_span("p_owner REF tasks.person", "p_owner REF ", "tasks.person"),
                token_span("updated REF tasks.task", "updated REF ", "tasks.task"),
                token_span("UPDATE tasks.task", "UPDATE ", "tasks.task"),
                token_span("SET title", "SET ", "title"),
                token_span("= p_title", "= ", "p_title"),
                {
                    let start = source.rfind(", owner").unwrap() + ", ".len();
                    (start, start + "owner".len())
                },
                token_span("= p_owner", "= ", "p_owner"),
                token_span("WHERE REF(changed)", "WHERE REF(", "changed"),
                token_span("= p_task RETURNING", "= ", "p_task"),
                token_span("RETURNING REF(changed)", "RETURNING REF(", "changed"),
            ]
        );
    }

    #[test]
    fn checks_server_delete_with_boolean_result_and_exact_evidence_order() {
        let source = "CREATE SCHEMA tasks; \
            CREATE TYPE tasks.task AS OBJECT (title TEXT NOT NULL); \
            CREATE SERVER FUNCTION tasks.remove(p_task REF tasks.task) \
            RETURNS ROWS (deleted BOOL) TRANSACTION ATOMIC \
            AS DELETE FROM tasks.task AS deleted_task \
            WHERE REF(deleted_task) = p_task RETURNING TRUE;";
        let report = check(&bundle([("functions.orna", source)]), &empty_catalogue());

        assert!(
            report.diagnostics().is_empty(),
            "{:?}",
            report.diagnostics()
        );
        let bundle = report.checked_bundle().expect("DELETE source is valid");
        let checked = &bundle.server_functions()[0];
        let task = &bundle.object_types()[0];
        let parameter = &checked.parameters()[0];
        let plan = checked.delete_plan().expect("expected a DELETE body");

        assert_eq!(plan.target_object(), task.id());
        assert_eq!(plan.selector_owner(), checked.id());
        assert_eq!(plan.selector_parameter(), parameter.id());
        assert_eq!(checked.return_columns()[0].name(), "deleted");
        assert_eq!(
            checked.return_columns()[0].semantic_type(),
            SemanticType::Scalar(StandardScalar::Boolean)
        );
        assert_eq!(
            checked
                .references()
                .iter()
                .map(|reference| (reference.kind(), reference.target()))
                .collect::<Vec<_>>(),
            vec![
                (
                    DefinitionReferenceKind::ObjectReference,
                    CheckedDefinitionReferenceTarget::ObjectType(task.id()),
                ),
                (
                    DefinitionReferenceKind::WriteObject,
                    CheckedDefinitionReferenceTarget::ObjectType(task.id()),
                ),
                (
                    DefinitionReferenceKind::ObjectReference,
                    CheckedDefinitionReferenceTarget::ObjectType(task.id()),
                ),
                (
                    DefinitionReferenceKind::ParameterRead,
                    CheckedDefinitionReferenceTarget::Parameter {
                        owner: checked.id(),
                        parameter: parameter.id(),
                    },
                ),
            ]
        );
        let span = |context: &str, prefix: &str, token: &str| {
            let start = source.find(context).unwrap() + prefix.len();
            (start, start + token.len())
        };
        assert_eq!(
            checked
                .references()
                .iter()
                .map(|reference| {
                    (
                        reference.location().span().start(),
                        reference.location().span().end(),
                    )
                })
                .collect::<Vec<_>>(),
            vec![
                span("p_task REF tasks.task", "p_task REF ", "tasks.task"),
                span("DELETE FROM tasks.task", "DELETE FROM ", "tasks.task"),
                span("WHERE REF(deleted_task)", "WHERE REF(", "deleted_task",),
                span("= p_task RETURNING", "= ", "p_task"),
            ]
        );
    }

    #[test]
    fn rejects_delete_return_shape_and_execution_modes_exactly() {
        let prefix =
            "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT NOT NULL); ";
        let body =
            "AS DELETE FROM tasks.task AS removed WHERE REF(removed) = p_task RETURNING TRUE;";
        let cases = [
            (
                format!(
                    "{prefix}CREATE SERVER FUNCTION tasks.remove(p_task REF tasks.task) RETURNS ROWS (a BOOL, b BOOL) TRANSACTION ATOMIC {body}"
                ),
                DiagnosticCode::TypeMismatch,
                "A DELETE SERVER function must declare exactly one column in RETURNS ROWS (...)",
                "ROWS (a BOOL, b BOOL)",
            ),
            (
                format!(
                    "{prefix}CREATE SERVER FUNCTION tasks.remove(p_task REF tasks.task) RETURNS ROWS (deleted REF tasks.task) TRANSACTION ATOMIC {body}"
                ),
                DiagnosticCode::TypeMismatch,
                "The RETURNS ROWS (...) column for a DELETE SERVER function must use BOOLEAN",
                "deleted REF tasks.task",
            ),
            (
                format!(
                    "{prefix}CREATE SERVER FUNCTION tasks.remove(p_task REF tasks.task) RETURNS BOOL TRANSACTION ATOMIC {body}"
                ),
                DiagnosticCode::TypeMismatch,
                "DELETE SERVER functions require RETURNS ROWS (...)",
                "BOOL",
            ),
        ];

        for (source, code, message, marker) in cases {
            let source_bundle =
                SourceBundle::new([SourceUnit::new("functions.orna", &source)]).unwrap();
            let report = check(&source_bundle, &empty_catalogue());
            assert_no_checked_bundle(&report);
            assert_eq!(report.diagnostics().len(), 1);
            let diagnostic = &report.diagnostics()[0];
            assert_eq!(diagnostic.code(), code);
            assert_eq!(diagnostic.message(), message);
            let start = source.rfind(marker).unwrap();
            assert_eq!(diagnostic.location().span().start(), start);
            assert_eq!(diagnostic.location().span().end(), start + marker.len());
        }

        let source = format!(
            "{prefix}CREATE SERVER FUNCTION tasks.remove(p_task REF tasks.task) \
             RETURNS ROWS (deleted BOOL) SECURITY DEFINER TRANSACTION READ ONLY VOLATILITY STABLE {body}"
        );
        let source_bundle =
            SourceBundle::new([SourceUnit::new("functions.orna", &source)]).unwrap();
        let report = check(&source_bundle, &empty_catalogue());
        assert_no_checked_bundle(&report);
        assert_eq!(report.diagnostics().len(), 3);
        assert_eq!(
            report
                .diagnostics()
                .iter()
                .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
                .collect::<Vec<_>>(),
            vec![
                (
                    DiagnosticCode::DomainIncompatible,
                    "DELETE SERVER functions require SECURITY INVOKER",
                ),
                (
                    DiagnosticCode::DomainIncompatible,
                    "DELETE SERVER functions require TRANSACTION ATOMIC",
                ),
                (
                    DiagnosticCode::DomainIncompatible,
                    "DELETE SERVER functions require VOLATILITY VOLATILE",
                ),
            ]
        );
        let declaration_start = source.find("CREATE SERVER FUNCTION").unwrap();
        for diagnostic in report.diagnostics() {
            assert_eq!(diagnostic.location().span().start(), declaration_start);
            assert_eq!(diagnostic.location().span().end(), source.len());
        }
    }

    #[test]
    fn rejects_an_unused_delete_parameter_outside_the_runtime_types() {
        let source = "CREATE SCHEMA tasks; \
            CREATE TYPE tasks.task AS OBJECT (title TEXT NOT NULL); \
            CREATE SERVER FUNCTION tasks.remove(p_task REF tasks.task, unused DECIMAL) \
            RETURNS ROWS (deleted BOOL) TRANSACTION ATOMIC \
            AS DELETE FROM tasks.task AS removed \
            WHERE REF(removed) = p_task RETURNING TRUE;";
        let report = check(&bundle([("functions.orna", source)]), &empty_catalogue());

        assert_no_checked_bundle(&report);
        assert_eq!(report.diagnostics().len(), 1);
        let diagnostic = &report.diagnostics()[0];
        assert_eq!(diagnostic.code(), DiagnosticCode::DomainIncompatible);
        assert_eq!(
            diagnostic.message(),
            "DELETE does not yet support the type of parameter unused; supported types are BOOLEAN, INTEGER, BIGINT, FLOAT, CHARACTER LARGE OBJECT, BINARY LARGE OBJECT, and REF"
        );
        let start = source.find("unused DECIMAL").unwrap();
        assert_eq!(diagnostic.location().span().start(), start);
        assert_eq!(diagnostic.location().span().end(), start + "unused".len());
    }

    #[test]
    fn rejects_insert_return_and_execution_modes() {
        let prefix =
            "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT NOT NULL); ";
        let cases = [
            (
                format!(
                    "{prefix}CREATE SERVER FUNCTION tasks.create(p TEXT) RETURNS ROWS (a REF tasks.task, b REF tasks.task) TRANSACTION ATOMIC AS INSERT INTO tasks.task AS made (title) VALUES (p) RETURNING REF(made);"
                ),
                vec![(
                    DiagnosticCode::TypeMismatch,
                    "An INSERT SERVER function must declare exactly one column in RETURNS ROWS (...)",
                    "ROWS (a",
                )],
            ),
            (
                format!(
                    "{prefix}CREATE SERVER FUNCTION tasks.create(p TEXT) RETURNS ROWS (a TEXT) TRANSACTION ATOMIC AS INSERT INTO tasks.task AS made (title) VALUES (p) RETURNING REF(made);"
                ),
                vec![(
                    DiagnosticCode::TypeMismatch,
                    "The RETURNS ROWS (...) column for an INSERT SERVER function must use REF",
                    "a TEXT",
                )],
            ),
            (
                format!(
                    "{prefix}CREATE TYPE tasks.other AS OBJECT (title TEXT NOT NULL); CREATE SERVER FUNCTION tasks.create(p TEXT) RETURNS ROWS (a REF tasks.other) TRANSACTION ATOMIC AS INSERT INTO tasks.task AS made (title) VALUES (p) RETURNING REF(made);"
                ),
                vec![(
                    DiagnosticCode::TypeMismatch,
                    "The returned REF must point to the object type being inserted",
                    "tasks.other",
                )],
            ),
            (
                format!(
                    "{prefix}CREATE SERVER FUNCTION tasks.create(p TEXT) RETURNS ROWS (a REF tasks.task) SECURITY DEFINER TRANSACTION ATOMIC AS INSERT INTO tasks.task AS made (title) VALUES (p) RETURNING REF(made);"
                ),
                vec![(
                    DiagnosticCode::DomainIncompatible,
                    "INSERT SERVER functions require SECURITY INVOKER",
                    "CREATE SERVER FUNCTION",
                )],
            ),
            (
                format!(
                    "{prefix}CREATE SERVER FUNCTION tasks.create(p TEXT) RETURNS ROWS (a REF tasks.task) AS INSERT INTO tasks.task AS made (title) VALUES (p) RETURNING REF(made);"
                ),
                vec![(
                    DiagnosticCode::DomainIncompatible,
                    "INSERT SERVER functions require TRANSACTION ATOMIC",
                    "CREATE SERVER FUNCTION",
                )],
            ),
            (
                format!(
                    "{prefix}CREATE SERVER FUNCTION tasks.create(p TEXT) RETURNS ROWS (a REF tasks.task) TRANSACTION READ ONLY AS INSERT INTO tasks.task AS made (title) VALUES (p) RETURNING REF(made);"
                ),
                vec![(
                    DiagnosticCode::DomainIncompatible,
                    "INSERT SERVER functions require TRANSACTION ATOMIC",
                    "CREATE SERVER FUNCTION",
                )],
            ),
            (
                format!(
                    "{prefix}CREATE SERVER FUNCTION tasks.create(p TEXT) RETURNS ROWS (a REF tasks.task) TRANSACTION ATOMIC VOLATILITY STABLE AS INSERT INTO tasks.task AS made (title) VALUES (p) RETURNING REF(made);"
                ),
                vec![(
                    DiagnosticCode::DomainIncompatible,
                    "INSERT SERVER functions require VOLATILITY VOLATILE",
                    "CREATE SERVER FUNCTION",
                )],
            ),
        ];
        for (source, expected) in cases {
            let bundle = SourceBundle::new([SourceUnit::new("functions.orna", &source)]).unwrap();
            let report = check(&bundle, &empty_catalogue());
            assert_no_checked_bundle(&report);
            assert_eq!(report.diagnostics().len(), expected.len());
            for (diagnostic, (code, message, marker)) in report.diagnostics().iter().zip(expected) {
                assert_eq!(diagnostic.code(), code);
                assert_eq!(diagnostic.message(), message);
                assert_eq!(diagnostic.location().logical_path(), "functions.orna");
                let expected_start = source.rfind(marker).unwrap();
                assert_eq!(diagnostic.location().span().start(), expected_start);
                let expected_end = match message {
                    "An INSERT SERVER function must declare exactly one column in RETURNS ROWS (...)" => {
                        source.find(") TRANSACTION").unwrap() + 1
                    }
                    "The RETURNS ROWS (...) column for an INSERT SERVER function must use REF" => {
                        expected_start + "a TEXT".len()
                    }
                    "The returned REF must point to the object type being inserted" => {
                        expected_start + "tasks.other".len()
                    }
                    _ => source.len(),
                };
                assert_eq!(diagnostic.location().span().end(), expected_end);
            }
        }
    }

    #[test]
    fn rejects_update_return_target_and_execution_modes_exactly() {
        let prefix = "CREATE SCHEMA tasks; \
            CREATE TYPE tasks.task AS OBJECT (title TEXT NOT NULL); \
            CREATE TYPE tasks.other AS OBJECT (title TEXT NOT NULL); ";
        let wrong_modes = format!(
            "{prefix}CREATE SERVER FUNCTION tasks.update(p_task REF tasks.task, p_title TEXT) \
             RETURNS ROWS (updated REF tasks.task) SECURITY DEFINER TRANSACTION READ ONLY VOLATILITY STABLE \
             AS UPDATE tasks.task AS changed SET title = p_title WHERE REF(changed) = p_task RETURNING REF(changed);"
        );
        let source_bundle =
            SourceBundle::new([SourceUnit::new("functions.orna", &wrong_modes)]).unwrap();
        let report = check(&source_bundle, &empty_catalogue());
        assert_no_checked_bundle(&report);
        assert_eq!(report.diagnostics().len(), 3);
        assert_eq!(
            report
                .diagnostics()
                .iter()
                .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
                .collect::<Vec<_>>(),
            vec![
                (
                    DiagnosticCode::DomainIncompatible,
                    "UPDATE SERVER functions require SECURITY INVOKER",
                ),
                (
                    DiagnosticCode::DomainIncompatible,
                    "UPDATE SERVER functions require TRANSACTION ATOMIC",
                ),
                (
                    DiagnosticCode::DomainIncompatible,
                    "UPDATE SERVER functions require VOLATILITY VOLATILE",
                ),
            ]
        );
        assert!(report.diagnostics().iter().all(|diagnostic| {
            diagnostic.location().span().start()
                == wrong_modes.rfind("CREATE SERVER FUNCTION").unwrap()
                && diagnostic.location().span().end() == wrong_modes.len()
        }));

        let wrong_return = format!(
            "{prefix}CREATE SERVER FUNCTION tasks.update(p_task REF tasks.task, p_title TEXT) \
             RETURNS ROWS (updated REF tasks.other) TRANSACTION ATOMIC \
             AS UPDATE tasks.task AS changed SET title = p_title WHERE REF(changed) = p_task RETURNING REF(changed);"
        );
        let source_bundle =
            SourceBundle::new([SourceUnit::new("functions.orna", &wrong_return)]).unwrap();
        let report = check(&source_bundle, &empty_catalogue());
        assert_no_checked_bundle(&report);
        assert_eq!(report.diagnostics().len(), 1);
        assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);
        assert_eq!(
            report.diagnostics()[0].message(),
            "The returned REF must point to the object type being updated"
        );
        let start = wrong_return.rfind("tasks.other").unwrap();
        assert_eq!(report.diagnostics()[0].location().span().start(), start);
        assert_eq!(
            report.diagnostics()[0].location().span().end(),
            start + "tasks.other".len()
        );
    }

    #[test]
    fn rejects_distinct_function_shape_with_four_ordered_declaration_diagnostics() {
        let source = "CREATE SCHEMA tasks; \
            CREATE TYPE tasks.task AS OBJECT (completed BOOL NOT NULL); \
            CREATE SERVER FUNCTION tasks.values(p_flag BOOL) RETURNS ROWS (completed BOOL) \
            SECURITY DEFINER TRANSACTION ATOMIC VOLATILITY IMMUTABLE \
            AS SELECT DISTINCT t.completed FROM tasks.task t;";
        let report = check(
            &bundle([("distinct_shape.orna", source)]),
            &empty_catalogue(),
        );

        assert_no_checked_bundle(&report);
        assert_eq!(
            report
                .diagnostics()
                .iter()
                .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
                .collect::<Vec<_>>(),
            vec![
                (
                    DiagnosticCode::DomainIncompatible,
                    "SELECT DISTINCT SERVER functions require zero declared parameters",
                ),
                (
                    DiagnosticCode::DomainIncompatible,
                    "SELECT DISTINCT SERVER functions require SECURITY INVOKER",
                ),
                (
                    DiagnosticCode::DomainIncompatible,
                    "SELECT DISTINCT SERVER functions require TRANSACTION READ ONLY",
                ),
                (
                    DiagnosticCode::DomainIncompatible,
                    "SELECT DISTINCT SERVER functions require VOLATILITY STABLE",
                ),
            ]
        );
        for diagnostic in report.diagnostics() {
            assert_eq!(diagnostic.location().logical_path(), "distinct_shape.orna");
            assert_eq!(
                diagnostic.location().span().start(),
                source.find("CREATE SERVER FUNCTION").unwrap()
            );
            assert_eq!(diagnostic.location().span().end(), source.len());
        }
    }

    #[test]
    fn distinct_semantic_and_return_errors_precede_function_shape_diagnostics() {
        let semantic_source = "CREATE SCHEMA tasks; \
            CREATE TYPE tasks.task AS OBJECT (completed BOOL NOT NULL); \
            CREATE SERVER FUNCTION tasks.values(p_flag BOOL) RETURNS ROWS (completed BOOL) \
            SECURITY DEFINER TRANSACTION ATOMIC VOLATILITY IMMUTABLE \
            AS SELECT DISTINCT t.unknown FROM tasks.task t;";
        let report = check(
            &bundle([("distinct_semantic.orna", semantic_source)]),
            &empty_catalogue(),
        );
        assert_no_checked_bundle(&report);
        assert_eq!(report.diagnostics().len(), 1);
        let diagnostic = &report.diagnostics()[0];
        assert_eq!(diagnostic.code(), DiagnosticCode::UnknownQualifiedName);
        assert_eq!(diagnostic.message(), "unknown field unknown on tasks.task");
        assert_eq!(
            diagnostic.location().logical_path(),
            "distinct_semantic.orna"
        );
        let unknown_start = semantic_source.rfind("unknown").unwrap();
        assert_eq!(diagnostic.location().span().start(), unknown_start);
        assert_eq!(
            diagnostic.location().span().end(),
            unknown_start + "unknown".len()
        );

        let return_source = "CREATE SCHEMA tasks; \
            CREATE TYPE tasks.task AS OBJECT (completed BOOL NOT NULL); \
            CREATE SERVER FUNCTION tasks.values(p_flag BOOL) RETURNS ROWS (completed TEXT) \
            SECURITY DEFINER TRANSACTION ATOMIC VOLATILITY IMMUTABLE \
            AS SELECT DISTINCT t.completed FROM tasks.task t;";
        let report = check(
            &bundle([("distinct_return.orna", return_source)]),
            &empty_catalogue(),
        );
        assert_no_checked_bundle(&report);
        assert_eq!(report.diagnostics().len(), 1);
        let diagnostic = &report.diagnostics()[0];
        assert_eq!(diagnostic.code(), DiagnosticCode::TypeMismatch);
        assert_eq!(
            diagnostic.message(),
            "SELECT column 1 does not have the same type as RETURNS ROWS column completed"
        );
        assert_eq!(diagnostic.location().logical_path(), "distinct_return.orna");
        let return_start = return_source.find("completed TEXT").unwrap();
        assert_eq!(diagnostic.location().span().start(), return_start);
        assert_eq!(
            diagnostic.location().span().end(),
            return_start + "completed TEXT".len()
        );
    }

    #[test]
    fn rejects_unsupported_distinct_projections_with_the_relational_diagnostic() {
        let source = "CREATE SCHEMA tasks; \
            CREATE TYPE tasks.task AS OBJECT (title TEXT); \
            CREATE SERVER FUNCTION tasks.values() RETURNS ROWS (title TEXT) \
            SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE \
            AS SELECT DISTINCT t.title FROM tasks.task t;";
        let report = check(
            &bundle([("distinct_domain.orna", source)]),
            &empty_catalogue(),
        );

        assert_no_checked_bundle(&report);
        assert_eq!(report.diagnostics().len(), 1);
        let diagnostic = &report.diagnostics()[0];
        assert_eq!(diagnostic.code(), DiagnosticCode::DomainIncompatible);
        assert_eq!(
            diagnostic.message(),
            "SELECT DISTINCT projections support only BOOLEAN, INTEGER, BIGINT, BYTES, and REF values"
        );
        assert_eq!(diagnostic.location().logical_path(), "distinct_domain.orna");
        let projection_start = source.rfind("t.title").unwrap();
        assert_eq!(diagnostic.location().span().start(), projection_start);
        assert_eq!(
            diagnostic.location().span().end(),
            projection_start + "t.title".len()
        );
    }

    #[test]
    fn rejects_select_projection_count_and_type_at_rows_declarations() {
        let source = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); \
            CREATE SERVER FUNCTION tasks.count() RETURNS ROWS (first TEXT, second TEXT) \
            AS SELECT t.title FROM tasks.task t; \
            CREATE SERVER FUNCTION tasks.kind() RETURNS ROWS (title BOOL) \
            AS SELECT t.title FROM tasks.task t; \
            CREATE SERVER FUNCTION tasks.wide() RETURNS ROWS (only TEXT) \
            AS SELECT t.title, t.title FROM tasks.task t;";
        let report = check(&bundle([("functions.orna", source)]), &empty_catalogue());

        assert_eq!(report.diagnostics().len(), 3);
        assert_eq!(
            report.diagnostics()[0].message(),
            "SELECT returns 1 column, but RETURNS ROWS (...) declares 2 columns"
        );
        assert_eq!(
            report.diagnostics()[0].location().span().start(),
            source.find("ROWS (first").unwrap()
        );
        assert_eq!(
            report.diagnostics()[1].message(),
            "SELECT column 1 does not have the same type as RETURNS ROWS column title"
        );
        assert_eq!(
            report.diagnostics()[1].location().span().start(),
            source.find("title BOOL").unwrap()
        );
        assert_eq!(
            report.diagnostics()[2].message(),
            "SELECT returns 2 columns, but RETURNS ROWS (...) declares 1 column"
        );
        assert_eq!(
            report.diagnostics()[2].location().span().start(),
            source.rfind("ROWS (only").unwrap()
        );
        assert_no_checked_bundle(&report);
    }

    #[test]
    fn rejects_parameterised_select_with_more_than_one_declared_parameter() {
        let _function_id = FunctionId::from_bytes([4; 16]);
        let _parameter_id = ParameterId::from_bytes([5; 16]);
        let _offset_parameter_id = ParameterId::from_bytes([6; 16]);
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
                 SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE \
                 AS SELECT t.title FROM tasks.task t;",
            )]),
            &base,
        );

        assert_eq!(report.diagnostics().len(), 1);
        assert_eq!(
            report.diagnostics()[0].code(),
            DiagnosticCode::DomainIncompatible
        );
        assert_eq!(
            report.diagnostics()[0].message(),
            "parameterised SELECT SERVER functions require exactly one declared parameter"
        );
        assert_eq!(
            report.diagnostics()[0].location().logical_path(),
            "functions.orna"
        );
        assert_eq!(
            report.diagnostics()[0].location().span().start(),
            "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); \
                 CREATE SERVER FUNCTION tasks.open(p_offset INT, p_limit INT) RETURNS ROWS (title TEXT) \
                 SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE \
                 AS SELECT t.title FROM tasks.task t;"
                .find("SELECT t.title")
                .unwrap()
        );
        assert_no_checked_bundle(&report);
    }

    #[test]
    fn rejects_identity_selected_query_candidates_with_exact_diagnostics() {
        let prefix = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); ";
        let suffix = " SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE";
        let cases = [
            (
                "no_predicate",
                "CREATE SERVER FUNCTION tasks.get(p_task REF tasks.task) RETURNS ROWS (title TEXT)",
                " AS SELECT t.title FROM tasks.task t;",
                DiagnosticCode::DomainIncompatible,
                "parameterised SELECT SERVER functions require WHERE REF(source_alias) = selector_parameter",
                "SELECT t.title",
            ),
            (
                "wrong_name",
                "CREATE SERVER FUNCTION tasks.get(p_task REF tasks.task) RETURNS ROWS (title TEXT)",
                " AS SELECT t.title FROM tasks.task t WHERE REF(t) = other;",
                DiagnosticCode::UnknownQualifiedName,
                "this function has no parameter named other",
                "other",
            ),
            (
                "wrong_type",
                "CREATE SERVER FUNCTION tasks.get(p_task INT) RETURNS ROWS (title TEXT)",
                " AS SELECT t.title FROM tasks.task t WHERE REF(t) = p_task;",
                DiagnosticCode::TypeMismatch,
                "selector parameter p_task must use REF tasks.task",
                "p_task;",
            ),
            (
                "wrong_alias",
                "CREATE SERVER FUNCTION tasks.get(p_task REF tasks.task) RETURNS ROWS (title TEXT)",
                " AS SELECT t.title FROM tasks.task t WHERE REF(other) = p_task;",
                DiagnosticCode::UnknownQualifiedName,
                "unknown query alias other",
                "other",
            ),
            (
                "return_type",
                "CREATE SERVER FUNCTION tasks.get(p_task REF tasks.task) RETURNS ROWS (title BOOL)",
                " AS SELECT t.title FROM tasks.task t WHERE REF(t) = p_task;",
                DiagnosticCode::TypeMismatch,
                "SELECT column 1 does not have the same type as RETURNS ROWS column title",
                "title BOOL",
            ),
        ];

        for (path, header, body, code, message, marker) in cases {
            let source = format!("{prefix}{header}{suffix}{body}");
            let bundle = SourceBundle::new([SourceUnit::new(path, source.as_str())]).unwrap();
            let report = check(&bundle, &empty_catalogue());
            assert_eq!(report.diagnostics().len(), 1, "{path}");
            let diagnostic = &report.diagnostics()[0];
            assert_eq!(diagnostic.code(), code, "{path}");
            assert_eq!(diagnostic.message(), message, "{path}");
            assert_eq!(diagnostic.location().logical_path(), path, "{path}");
            let expected_start = source.rfind(marker).unwrap();
            assert_eq!(
                diagnostic.location().span().start(),
                expected_start,
                "{path}"
            );
            assert_eq!(
                diagnostic.location().span().end(),
                if path == "no_predicate" {
                    source.len() - 1
                } else {
                    expected_start + marker.len().saturating_sub((path == "wrong_type") as usize)
                },
                "{path}"
            );
            assert_no_checked_bundle(&report);
        }
    }

    #[test]
    fn reports_identity_selected_query_mode_failures_before_body_checking() {
        let source = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); \
            CREATE SERVER FUNCTION tasks.get(p_task REF tasks.task) RETURNS ROWS (title TEXT) \
            SECURITY DEFINER TRANSACTION ATOMIC VOLATILITY VOLATILE \
            AS SELECT t.title FROM tasks.task t;";
        let report = check(&bundle([("modes.orna", source)]), &empty_catalogue());
        let messages = report
            .diagnostics()
            .iter()
            .map(|diagnostic| diagnostic.message())
            .collect::<Vec<_>>();
        assert_eq!(
            messages,
            vec![
                "parameterised SELECT SERVER functions require SECURITY INVOKER",
                "parameterised SELECT SERVER functions require TRANSACTION READ ONLY",
                "parameterised SELECT SERVER functions require VOLATILITY STABLE",
            ]
        );
        for diagnostic in report.diagnostics() {
            assert_eq!(diagnostic.code(), DiagnosticCode::DomainIncompatible);
            assert_eq!(diagnostic.location().logical_path(), "modes.orna");
            assert_eq!(
                diagnostic.location().span().start(),
                source.find("CREATE SERVER FUNCTION").unwrap()
            );
            assert_eq!(diagnostic.location().span().end(), source.len());
        }
        assert_no_checked_bundle(&report);
    }

    #[test]
    fn syntax_errors_take_precedence_over_identity_selected_query_modes() {
        let source = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); \
            CREATE SERVER FUNCTION tasks.get(p_task REF tasks.task) RETURNS ROWS (title TEXT) \
            SECURITY DEFINER TRANSACTION ATOMIC VOLATILITY VOLATILE \
            AS SELECT t.title FROM tasks.task t WHERE p_task = REF(t);";
        let report = check(&bundle([("syntax.orna", source)]), &empty_catalogue());
        assert_eq!(report.diagnostics().len(), 1);
        assert_eq!(
            report.diagnostics()[0].code(),
            DiagnosticCode::UnexpectedToken
        );
        assert_eq!(
            report.diagnostics()[0].message(),
            "the current Orna SELECT parser does not yet implement selector parameters on the left side of WHERE equality; expected WHERE REF(alias) = selector_parameter"
        );
        assert_eq!(
            report.diagnostics()[0].location().logical_path(),
            "syntax.orna"
        );
        assert_eq!(
            report.diagnostics()[0].location().span().start(),
            source.rfind("p_task").unwrap()
        );
        assert_no_checked_bundle(&report);
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
            "SERVER functions do not yet support this body form"
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
            diagnostic.message() != "SERVER functions do not yet support this body form"
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
            "SERVER function parameters do not yet support default values"
        );
        assert_eq!(
            report.diagnostics()[0].location().span().start(),
            source.find("'open'").unwrap()
        );
        assert_eq!(
            report.diagnostics()[1].message(),
            "SERVER functions do not yet support REQUIRES CAPABILITY"
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
