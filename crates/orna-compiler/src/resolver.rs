//! Semantic resolution for parsed source bundles.
//!
//! The resolver consumes the `Parse` values retained by [`super::parse_bundle`].
//! It does not parse source text or expose syntax implementation values.

mod identity;

use std::collections::{HashMap, HashSet};

use orna_core::{
    CatalogueRevisionId, ExpressionId, FieldId, FunctionId, FunctionRevisionId, ParameterId,
    SchemaId, TypeId,
    catalogue::{
        CatalogueSnapshot, FieldDefinition, FunctionDefinition, FunctionDomain, FunctionReturn,
        FunctionReturnColumnDefinition, FunctionSecurity as CatalogueFunctionSecurity,
        FunctionTransaction as CatalogueFunctionTransaction,
        FunctionVolatility as CatalogueFunctionVolatility, ObjectTypeDefinition, OnDeleteAction,
        ParameterDefinition, QualifiedSemanticName, SchemaDefinition,
    },
    source::SourceBundle,
    types::{ResolvedType, StandardScalar},
};
use orna_syntax::{
    FunctionReturnType, FunctionSecurity as SyntaxFunctionSecurity,
    FunctionTransaction as SyntaxFunctionTransaction,
    FunctionVolatility as SyntaxFunctionVolatility, ObjectTypeDeclaration, OnDeletePolicy,
    QualifiedName, ServerFunctionBody, ServerFunctionDeclaration, SourceSlice, SourceSpan,
    StandardLargeObjectKind, TypeSpecification,
};

use crate::relational::{RelationalQueryIr, check_query};
use crate::{
    ByteSpan, CompilerDiagnostic, DiagnosticCode, ParseReport, SourceLocation,
    normalise_name_part as semantic_part, normalise_qualified_name as semantic_name, parse_bundle,
};

/// Checks one source bundle against an immutable catalogue snapshot.
///
/// This function parses the bundle exactly once. Resolution consumes the owned
/// `Parse` values that [`parse_bundle`] retains in the resulting report.
pub fn check(bundle: &SourceBundle, base: &CatalogueSnapshot) -> CheckReport {
    check_parsed(parse_bundle(bundle), base)
}

/// The value of a default expression accepted in this first compiler slice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConstantValue {
    Null,
    Boolean(bool),
    Integer(i64),
    Text(String),
}

/// A checked constant default expression.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedDefault {
    id: ExpressionId,
    value: ConstantValue,
    location: SourceLocation,
}

impl CheckedDefault {
    /// Returns the stable identity of this checked expression.
    pub const fn id(&self) -> ExpressionId {
        self.id
    }

    /// Returns the checked constant value.
    pub fn value(&self) -> &ConstantValue {
        &self.value
    }

    /// Returns the location of the source expression.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }
}

/// A checked field definition without parser implementation values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedField {
    id: FieldId,
    name: String,
    ordinal: u32,
    resolved_type: ResolvedType,
    nullable: bool,
    unique: bool,
    default: Option<CheckedDefault>,
    on_delete: Option<OnDeleteAction>,
    location: SourceLocation,
}

impl CheckedField {
    /// Returns the stable identity of the field.
    pub const fn id(&self) -> FieldId {
        self.id
    }
    /// Returns the resolved field name.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Returns the declaration ordinal.
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }
    /// Returns the resolved type.
    pub const fn resolved_type(&self) -> ResolvedType {
        self.resolved_type
    }
    /// Reports whether the field permits null.
    pub const fn nullable(&self) -> bool {
        self.nullable
    }
    /// Reports whether the field is unique.
    pub const fn unique(&self) -> bool {
        self.unique
    }
    /// Returns the checked default expression, when declared.
    pub fn default(&self) -> Option<&CheckedDefault> {
        self.default.as_ref()
    }
    /// Returns the resolved delete action, when declared.
    pub const fn on_delete(&self) -> Option<OnDeleteAction> {
        self.on_delete
    }
    /// Returns the source location of the field declaration.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }
}

/// A checked object type declaration without parser implementation values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedObjectType {
    id: TypeId,
    name: QualifiedSemanticName,
    fields: Vec<CheckedField>,
    location: SourceLocation,
}

impl CheckedObjectType {
    /// Returns the stable identity of the object type.
    pub const fn id(&self) -> TypeId {
        self.id
    }
    /// Returns the resolved qualified type name.
    pub fn name(&self) -> &QualifiedSemanticName {
        &self.name
    }
    /// Returns checked fields in declaration order.
    pub fn fields(&self) -> &[CheckedField] {
        &self.fields
    }
    /// Returns the source location of the declaration.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }
}

/// A checked source bundle ready for a later semantic-diff and apply stage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedBundle {
    schemas: Vec<CheckedSchema>,
    object_types: Vec<CheckedObjectType>,
    server_functions: Vec<CheckedServerFunction>,
}

impl CheckedBundle {
    /// Returns submitted schema declarations in source order.
    pub fn schemas(&self) -> &[CheckedSchema] {
        &self.schemas
    }

    /// Returns submitted object declarations in source order.
    pub fn object_types(&self) -> &[CheckedObjectType] {
        &self.object_types
    }

    /// Returns submitted checked SERVER functions in source order.
    pub fn server_functions(&self) -> &[CheckedServerFunction] {
        &self.server_functions
    }
}

/// A checked SERVER function with an Orna-owned relational execution plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedServerFunction {
    definition: FunctionDefinition,
    location: SourceLocation,
    plan: RelationalQueryIr,
}

impl CheckedServerFunction {
    /// Returns the stable function identity.
    pub const fn id(&self) -> FunctionId {
        self.definition.id()
    }

    /// Returns the resolved function name.
    pub fn name(&self) -> &QualifiedSemanticName {
        self.definition.name()
    }

    /// Returns the source location of the declaration.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn plan(&self) -> &RelationalQueryIr {
        &self.plan
    }
}

/// A checked logical schema declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedSchema {
    id: SchemaId,
    name: QualifiedSemanticName,
    location: SourceLocation,
}

impl CheckedSchema {
    /// Returns the stable identity of the schema.
    pub const fn id(&self) -> SchemaId {
        self.id
    }

    /// Returns the resolved logical schema name.
    pub fn name(&self) -> &QualifiedSemanticName {
        &self.name
    }

    /// Returns the source location of the declaration.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }
}

/// The result of parsing and checking a source bundle.
#[derive(Clone, Debug)]
pub struct CheckReport {
    parse_report: ParseReport,
    diagnostics: Vec<CompilerDiagnostic>,
    checked_bundle: Option<CheckedBundle>,
    candidate: Option<CatalogueSnapshot>,
}

impl CheckReport {
    /// Returns the retained parse report on both success and failure.
    pub fn parse_report(&self) -> &ParseReport {
        &self.parse_report
    }
    /// Returns syntax and semantic diagnostics in source order.
    pub fn diagnostics(&self) -> &[CompilerDiagnostic] {
        &self.diagnostics
    }
    /// Returns checked Orna-owned definitions when checking succeeds.
    pub fn checked_bundle(&self) -> Option<&CheckedBundle> {
        self.checked_bundle.as_ref()
    }
    /// Returns the immutable candidate catalogue when checking succeeds.
    pub fn candidate(&self) -> Option<&CatalogueSnapshot> {
        self.candidate.as_ref()
    }
}

#[derive(Clone, Copy)]
struct Header<'a> {
    declaration: &'a ObjectTypeDeclaration,
    logical_path: &'a str,
    id: TypeId,
}

/// Resolved metadata for a SERVER function before relational planning.
#[derive(Clone, Copy)]
struct ServerFunctionHeader<'a> {
    declaration: &'a ServerFunctionDeclaration,
    logical_path: &'a str,
    id: FunctionId,
    security: CatalogueFunctionSecurity,
    transaction: Option<CatalogueFunctionTransaction>,
    volatility: CatalogueFunctionVolatility,
}

/// One resolved parameter accepted by this SERVER function slice.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ResolvedServerFunctionParameter {
    id: ParameterId,
    name: String,
    ordinal: u32,
    resolved_type: ResolvedType,
}

/// One resolved column in a `ROWS` result.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ResolvedServerFunctionReturnColumn {
    name: String,
    ordinal: u32,
    resolved_type: ResolvedType,
    location: SourceLocation,
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
    id: FunctionId,
    name: QualifiedSemanticName,
    parameters: Vec<ResolvedServerFunctionParameter>,
    return_type: ResolvedServerFunctionReturn,
    security: CatalogueFunctionSecurity,
    transaction: Option<CatalogueFunctionTransaction>,
    volatility: CatalogueFunctionVolatility,
    body: &'a ServerFunctionBody,
    location: SourceLocation,
}

/// A fully checked SERVER function before its revision identity is allocated.
#[derive(Clone, Debug)]
struct PlannedServerFunction {
    id: FunctionId,
    name: QualifiedSemanticName,
    parameters: Vec<ResolvedServerFunctionParameter>,
    columns: Vec<ResolvedServerFunctionReturnColumn>,
    security: CatalogueFunctionSecurity,
    transaction: Option<CatalogueFunctionTransaction>,
    volatility: CatalogueFunctionVolatility,
    location: SourceLocation,
    plan: RelationalQueryIr,
}

fn check_parsed(parse_report: ParseReport, base: &CatalogueSnapshot) -> CheckReport {
    let mut diagnostics = parse_report.diagnostics().to_vec();
    if !diagnostics.is_empty() {
        return failed(parse_report, diagnostics);
    }

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
                id: base
                    .schema_by_name(&name)
                    .map_or_else(SchemaId::new, SchemaDefinition::id),
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
            let id = base
                .object_type_by_name(&name)
                .map_or_else(TypeId::new, ObjectTypeDefinition::id);
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
        let base_type = base.object_type_by_id(header.id);
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

            let resolved_type = resolve_type(
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

            let id = base_type
                .and_then(|object_type| object_type.field_by_name(&name))
                .map_or_else(FieldId::new, FieldDefinition::id);
            let existing_default = base_type
                .and_then(|object_type| object_type.field_by_name(&name))
                .and_then(FieldDefinition::default_expression);
            let default = match (field.default_expression.as_ref(), resolved_type) {
                (Some(source), Some(resolved_type)) => checked_default(
                    source,
                    resolved_type,
                    field.nullable,
                    existing_default,
                    header.logical_path,
                    &mut diagnostics,
                ),
                _ => None,
            };

            if let Some(resolved_type) = resolved_type {
                checked_fields.push(CheckedField {
                    id,
                    name,
                    ordinal: field.order as u32,
                    resolved_type,
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

    let mut object_types: Vec<ObjectTypeDefinition> = Vec::with_capacity(checked_types.len());
    for checked_type in &checked_types {
        let definition = ObjectTypeDefinition::new(
            checked_type.id,
            checked_type.name.clone(),
            checked_type
                .fields
                .iter()
                .map(as_field_definition)
                .collect(),
        );
        if let Some(index) = object_types
            .iter()
            .position(|candidate| candidate.id() == checked_type.id)
        {
            object_types[index] = definition;
        } else {
            object_types.push(definition);
        }
    }
    let mut schemas: Vec<SchemaDefinition> = Vec::with_capacity(checked_schemas.len());
    for checked_schema in &checked_schemas {
        let definition = SchemaDefinition::new(checked_schema.id, checked_schema.name.clone());
        if let Some(index) = schemas
            .iter()
            .position(|candidate| candidate.id() == checked_schema.id)
        {
            schemas[index] = definition;
        } else {
            schemas.push(definition);
        }
    }
    let query_catalogue = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::new(),
        schemas,
        object_types,
        Vec::new(),
    )
    .expect("checked definitions satisfy catalogue invariants");

    let function_headers = if diagnostics.is_empty() {
        resolve_server_function_headers(&parse_report, base, &known_schemas, &mut diagnostics)
    } else {
        Vec::new()
    };
    let function_inputs = if diagnostics.is_empty() {
        resolve_server_function_inputs(&function_headers, &submitted_ids, base, &mut diagnostics)
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

    let mut functions: Vec<FunctionDefinition> = Vec::with_capacity(checked_functions.len());
    for checked_function in &checked_functions {
        let definition = checked_function.definition.clone();
        if let Some(index) = functions
            .iter()
            .position(|candidate| candidate.id() == definition.id())
        {
            functions[index] = definition;
        } else {
            functions.push(definition);
        }
    }
    let candidate = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::new(),
        query_catalogue.schemas().to_vec(),
        query_catalogue.object_types().to_vec(),
        functions,
    )
    .expect("checked definitions satisfy catalogue invariants");
    CheckReport {
        parse_report,
        diagnostics,
        checked_bundle: Some(CheckedBundle {
            schemas: checked_schemas,
            object_types: checked_types,
            server_functions: checked_functions,
        }),
        candidate: Some(candidate),
    }
}

fn resolve_server_function_headers<'a>(
    parse_report: &'a ParseReport,
    base: &CatalogueSnapshot,
    known_schemas: &HashSet<QualifiedSemanticName>,
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
                id: base
                    .function_by_name(&name)
                    .map_or_else(FunctionId::new, |function| function.id()),
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
    submitted_ids: &HashMap<QualifiedSemanticName, TypeId>,
    base: &CatalogueSnapshot,
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

            let Some(resolved_type) = resolve_type(
                &parameter.type_specification,
                submitted_ids,
                header.logical_path,
                diagnostics,
            ) else {
                continue;
            };
            let id = base_function
                .and_then(|function| function.parameter_by_name(&parameter_name))
                .map_or_else(ParameterId::new, |parameter| parameter.id());
            parameters.push(ResolvedServerFunctionParameter {
                id,
                name: parameter_name,
                ordinal: parameter.order as u32,
                resolved_type,
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
    submitted_ids: &HashMap<QualifiedSemanticName, TypeId>,
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
                let Some(resolved_type) = resolve_type(
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
                    resolved_type,
                    location: location(logical_path, &column.span),
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
    catalogue: &CatalogueSnapshot,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) -> Vec<CheckedServerFunction> {
    let diagnostics_before = diagnostics.len();
    let mut plans = Vec::with_capacity(inputs.len());

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
        let plan = match check_query(&body.query, catalogue, input.location.logical_path()) {
            Ok(plan) => plan,
            Err(query_diagnostics) => {
                diagnostics.extend(query_diagnostics);
                continue;
            }
        };

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
            if projection.value_type().resolved_type() != column.resolved_type {
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

        plans.push(PlannedServerFunction {
            id: input.id,
            name: input.name.clone(),
            parameters: input.parameters.clone(),
            columns: columns.clone(),
            security: input.security,
            transaction: input.transaction,
            volatility: input.volatility,
            location: input.location.clone(),
            plan,
        });
    }

    if diagnostics.len() != diagnostics_before {
        return Vec::new();
    }

    plans
        .into_iter()
        .map(|plan| CheckedServerFunction {
            definition: FunctionDefinition::new(
                plan.id,
                plan.name,
                FunctionDomain::Server,
                plan.parameters
                    .into_iter()
                    .map(|parameter| {
                        ParameterDefinition::new(
                            parameter.id,
                            parameter.name,
                            parameter.ordinal,
                            parameter.resolved_type,
                            None,
                        )
                    })
                    .collect(),
                FunctionReturn::Rows(
                    plan.columns
                        .into_iter()
                        .map(|column| {
                            FunctionReturnColumnDefinition::new(
                                column.name,
                                column.ordinal,
                                column.resolved_type,
                            )
                        })
                        .collect(),
                ),
                FunctionRevisionId::new(),
                plan.security,
                plan.transaction,
                plan.volatility,
            ),
            location: plan.location,
            plan: plan.plan,
        })
        .collect()
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
        candidate: None,
    }
}

fn resolve_type(
    specification: &TypeSpecification,
    submitted_ids: &HashMap<QualifiedSemanticName, TypeId>,
    logical_path: &str,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) -> Option<ResolvedType> {
    match specification {
        TypeSpecification::Named(name) => {
            if let Some(scalar) = resolve_closed_scalar(name) {
                return Some(ResolvedType::scalar(scalar));
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
            Some(ResolvedType::scalar(scalar))
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
                Some(ResolvedType::reference(id))
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
    resolved_type: ResolvedType,
    nullable: bool,
    existing_id: Option<ExpressionId>,
    logical_path: &str,
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
    let valid = match (&value, resolved_type) {
        (ConstantValue::Null, _) => nullable,
        (ConstantValue::Boolean(_), ResolvedType::Scalar(StandardScalar::Boolean)) => true,
        (
            ConstantValue::Integer(_),
            ResolvedType::Scalar(StandardScalar::Integer | StandardScalar::BigInt),
        ) => true,
        (ConstantValue::Text(_), ResolvedType::Scalar(StandardScalar::CharacterLargeObject)) => {
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
        id: existing_id.unwrap_or_default(),
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

fn as_field_definition(field: &CheckedField) -> FieldDefinition {
    FieldDefinition::new(
        field.id,
        field.name.clone(),
        field.ordinal,
        field.resolved_type,
        field.nullable,
        field.unique,
        field.default.as_ref().map(CheckedDefault::id),
        field.on_delete,
    )
}

fn namespace_of(name: &QualifiedSemanticName) -> Option<QualifiedSemanticName> {
    let namespace_parts = name.parts().get(..name.parts().len().checked_sub(1)?)?;
    if namespace_parts.is_empty() {
        return None;
    }
    QualifiedSemanticName::new(namespace_parts.iter().cloned()).ok()
}

fn location(logical_path: &str, span: &SourceSpan) -> SourceLocation {
    SourceLocation {
        logical_path: logical_path.to_owned(),
        span: ByteSpan::from_syntax_span(span),
    }
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
    use std::collections::HashSet;

    use orna_core::{
        CatalogueRevisionId, FunctionId, FunctionRevisionId, SchemaId,
        catalogue::{
            CatalogueSnapshot, FunctionDefinition, FunctionDomain, FunctionReturn,
            FunctionSecurity, FunctionTransaction, FunctionVolatility, OnDeleteAction,
            QualifiedSemanticName, SchemaDefinition,
        },
        source::{SourceBundle, SourceUnit},
        types::{ResolvedType, StandardScalar},
    };

    use super::{
        ConstantValue, DiagnosticCode, check, parse_bundle, resolve_server_function_headers,
    };

    fn empty_catalogue() -> CatalogueSnapshot {
        CatalogueSnapshot::new(CatalogueRevisionId::new(), Vec::new(), Vec::new()).unwrap()
    }

    fn schema(id: u8, parts: &[&str]) -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::from_bytes([id; 16]),
            QualifiedSemanticName::new(parts.iter().copied()).unwrap(),
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

    fn successful_candidate(source: &'static str) -> CatalogueSnapshot {
        check(&bundle([("schema.orna", source)]), &empty_catalogue())
            .candidate()
            .cloned()
            .expect("source must check")
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
            task.fields()[0].resolved_type(),
            ResolvedType::reference(person.id())
        );
    }

    #[test]
    fn empty_schema_declaration_persists_with_a_stable_identity() {
        let initial = successful_candidate("CREATE SCHEMA crm;");
        let schema_id = initial.schemas()[0].id();

        assert_eq!(initial.schemas().len(), 1);
        assert_eq!(initial.schemas()[0].name().to_string(), "crm");
        assert!(initial.object_types().is_empty());

        let report = check(&bundle([("schema.orna", "CREATE SCHEMA CRM;")]), &initial);

        assert!(report.diagnostics().is_empty());
        assert_eq!(report.candidate().unwrap().schemas()[0].id(), schema_id);
    }

    #[test]
    fn requires_submitted_schema_declarations_even_when_base_has_them() {
        let base = CatalogueSnapshot::new(
            CatalogueRevisionId::new(),
            vec![schema(1, &["crm"])],
            vec![],
        )
        .unwrap();

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
        assert!(object_report.candidate().is_none());

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
        assert!(function_report.candidate().is_none());
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
            fields[0].resolved_type(),
            ResolvedType::scalar(StandardScalar::Boolean)
        );
        assert!(!fields[0].nullable());
        assert_eq!(
            fields[0].default().unwrap().value(),
            &ConstantValue::Boolean(false)
        );
        assert_eq!(
            fields[1].resolved_type(),
            ResolvedType::scalar(StandardScalar::Integer)
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
            fields[4].resolved_type(),
            ResolvedType::scalar(StandardScalar::CharacterLargeObject)
        );
        assert_eq!(
            fields[5].resolved_type(),
            ResolvedType::scalar(StandardScalar::BinaryLargeObject)
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
            fields[0].resolved_type(),
            ResolvedType::scalar(StandardScalar::CharacterLargeObject)
        );
        assert_eq!(
            fields[1].resolved_type(),
            ResolvedType::scalar(StandardScalar::BinaryLargeObject)
        );
    }

    #[test]
    fn repeated_checks_preserve_matching_ids_even_when_fields_reorder() {
        let initial = successful_candidate(
            "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (name TEXT, age INT DEFAULT 1);",
        );
        let original = initial.object_types()[0].clone();
        let name_id = original.field_by_name("name").unwrap().id();
        let age = original.field_by_name("age").unwrap();

        let report = check(
            &bundle([(
                "renamed-file.orna",
                "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (age INT DEFAULT 1, name TEXT);",
            )]),
            &initial,
        );

        assert!(report.diagnostics().is_empty());
        let revised = &report.candidate().unwrap().object_types()[0];
        assert_eq!(revised.id(), original.id());
        assert_eq!(revised.field_by_name("name").unwrap().id(), name_id);
        assert_eq!(revised.field_by_name("age").unwrap().id(), age.id());
        assert_eq!(
            revised.field_by_name("age").unwrap().default_expression(),
            age.default_expression()
        );
    }

    #[test]
    fn added_field_gets_a_new_identity() {
        let initial = successful_candidate(
            "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (name TEXT);",
        );
        let report = check(
            &bundle([(
                "schema.orna",
                "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (name TEXT, email TEXT);",
            )]),
            &initial,
        );

        let initial_name = initial.object_types()[0]
            .field_by_name("name")
            .unwrap()
            .id();
        let revised = &report.candidate().unwrap().object_types()[0];
        assert_eq!(revised.field_by_name("name").unwrap().id(), initial_name);
        assert_ne!(revised.field_by_name("email").unwrap().id(), initial_name);
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
        assert!(report.candidate().is_none());
    }

    #[test]
    fn candidate_contains_only_submitted_schemas_and_object_types() {
        let initial = successful_candidate(
            "CREATE SCHEMA people; CREATE SCHEMA tasks;\
             CREATE TYPE people.person AS OBJECT (name TEXT);\
             CREATE TYPE tasks.task AS OBJECT (title TEXT);",
        );
        let person_id = initial.object_types()[0].id();
        let report = check(
            &bundle([(
                "schema.orna",
                "CREATE SCHEMA people; CREATE TYPE people.customer AS OBJECT (name TEXT);",
            )]),
            &initial,
        );

        let candidate = report.candidate().unwrap();
        assert_eq!(candidate.schemas().len(), 1);
        assert_eq!(candidate.schemas()[0].name().to_string(), "people");
        assert_eq!(candidate.object_types().len(), 1);
        assert_ne!(candidate.object_types()[0].id(), person_id);
        assert!(
            candidate
                .object_type_by_name(&QualifiedSemanticName::new(["people", "person"]).unwrap())
                .is_none()
        );
        assert!(
            candidate
                .object_type_by_name(&QualifiedSemanticName::new(["tasks", "task"]).unwrap())
                .is_none()
        );
    }

    #[test]
    fn rejects_references_to_base_object_types_omitted_from_the_bundle() {
        let initial = successful_candidate(
            "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (name TEXT);",
        );
        let source = "CREATE SCHEMA tasks; \
            CREATE TYPE tasks.task AS OBJECT (owner REF people.person);";

        let report = check(&bundle([("tasks.orna", source)]), &initial);

        assert_eq!(report.diagnostics().len(), 1);
        assert_eq!(
            report.diagnostics()[0].code(),
            DiagnosticCode::UnknownQualifiedName
        );
        assert_eq!(
            report.diagnostics()[0].location().span().start(),
            source.find("people.person").unwrap()
        );
        assert!(report.checked_bundle().is_none());
        assert!(report.candidate().is_none());
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
        assert!(report.candidate().is_none());
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
        assert!(report.checked_bundle().is_none());
        assert!(report.candidate().is_none());
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
        assert!(report.checked_bundle().is_none());
        assert!(report.candidate().is_none());
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
        let candidate = report.candidate().unwrap();
        let definition = candidate.function_by_id(checked.id()).unwrap();
        assert_eq!(definition.security(), FunctionSecurity::Definer);
        assert_eq!(
            definition.transaction(),
            Some(FunctionTransaction::ReadOnly)
        );
        assert_eq!(definition.volatility(), FunctionVolatility::Stable);
        assert_eq!(definition.parameters().len(), 1);
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
        assert!(report.checked_bundle().is_none());
        assert!(report.candidate().is_none());
    }

    #[test]
    fn preserves_existing_function_and_reordered_parameter_ids_by_semantic_name() {
        let initial = successful_candidate(
            "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); \
             CREATE SERVER FUNCTION tasks.open(p_limit INT, p_offset INT) RETURNS ROWS (title TEXT) \
             AS SELECT t.title FROM tasks.task t;",
        );
        let original = initial
            .function_by_name(&QualifiedSemanticName::new(["tasks", "open"]).unwrap())
            .unwrap();
        let function_id = original.id();
        let parameter_id = original.parameter_by_name("p_limit").unwrap().id();
        let offset_parameter_id = original.parameter_by_name("p_offset").unwrap().id();

        let report = check(
            &bundle([(
                "functions.orna",
                "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); \
                 CREATE SERVER FUNCTION tasks.open(p_offset INT, p_limit INT) RETURNS ROWS (title TEXT) \
                 AS SELECT t.title FROM tasks.task t;",
            )]),
            &initial,
        );

        assert!(report.diagnostics().is_empty());
        let revised = report
            .candidate()
            .unwrap()
            .function_by_name(&QualifiedSemanticName::new(["tasks", "open"]).unwrap())
            .unwrap();
        assert_eq!(revised.id(), function_id);
        assert_eq!(
            revised.parameter_by_name("p_limit").unwrap().id(),
            parameter_id
        );
        assert_eq!(
            revised.parameter_by_name("p_offset").unwrap().id(),
            offset_parameter_id
        );
        assert_eq!(revised.parameters()[0].name(), "p_offset");
        assert_eq!(revised.parameters()[1].name(), "p_limit");
    }

    #[test]
    fn any_server_function_error_rejects_all_checked_definitions_and_candidates() {
        let source = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); \
            CREATE SERVER FUNCTION tasks.valid() RETURNS ROWS (title TEXT) \
            AS SELECT t.title FROM tasks.task t; \
            CREATE SERVER FUNCTION tasks.invalid() RETURNS ROWS (title BOOL) \
            AS SELECT t.title FROM tasks.task t;";
        let report = check(&bundle([("functions.orna", source)]), &empty_catalogue());

        assert_eq!(report.diagnostics().len(), 1);
        assert!(report.checked_bundle().is_none());
        assert!(report.candidate().is_none());
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
        assert!(report.checked_bundle().is_none());
        assert!(report.candidate().is_none());
    }

    #[test]
    fn rejects_definitions_in_base_schemas_that_are_omitted_from_the_bundle() {
        let base_function = FunctionDefinition::new(
            FunctionId::new(),
            QualifiedSemanticName::new(["sys", "health"]).unwrap(),
            FunctionDomain::Server,
            vec![],
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Boolean)),
            FunctionRevisionId::new(),
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Volatile,
        );
        let base = CatalogueSnapshot::new_with_functions(
            CatalogueRevisionId::new(),
            vec![schema(1, &["sys"])],
            vec![],
            vec![base_function],
        )
        .unwrap();

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
        assert!(report.candidate().is_none());
    }

    #[test]
    fn server_function_headers_preserve_ids_and_map_modifiers() {
        let existing = FunctionDefinition::new(
            FunctionId::new(),
            QualifiedSemanticName::new(["sys", "health"]).unwrap(),
            FunctionDomain::Server,
            vec![],
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Boolean)),
            FunctionRevisionId::new(),
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Volatile,
        );
        let existing_id = existing.id();
        let base = CatalogueSnapshot::new_with_functions(
            CatalogueRevisionId::new(),
            vec![schema(1, &["sys"])],
            vec![],
            vec![existing],
        )
        .unwrap();
        let parsed = parse_bundle(&bundle([(
            "functions.orna",
            "CREATE SERVER FUNCTION Sys.Health() RETURNS BOOL SECURITY DEFINER TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT TRUE FROM sys.health h;\
             CREATE SERVER FUNCTION sys.defaults() RETURNS BOOL AS SELECT TRUE FROM sys.health h;",
        )]));
        let known_schemas = HashSet::from([QualifiedSemanticName::new(["sys"]).unwrap()]);
        let mut diagnostics = Vec::new();

        let headers =
            resolve_server_function_headers(&parsed, &base, &known_schemas, &mut diagnostics);

        assert!(diagnostics.is_empty());
        assert_eq!(headers.len(), 2);
        assert_eq!(headers[0].id, existing_id);
        assert_eq!(headers[0].security, FunctionSecurity::Definer);
        assert_eq!(headers[0].transaction, Some(FunctionTransaction::ReadOnly));
        assert_eq!(headers[0].volatility, FunctionVolatility::Stable);
        assert_eq!(headers[1].security, FunctionSecurity::Invoker);
        assert_eq!(headers[1].transaction, None);
        assert_eq!(headers[1].volatility, FunctionVolatility::Volatile);
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
        assert!(report.checked_bundle().is_none());
        assert!(report.candidate().is_none());
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
        assert!(report.checked_bundle().is_none());
        assert!(report.candidate().is_none());
    }

    #[test]
    fn candidate_omits_unsubmitted_base_functions_and_schemas() {
        let function = FunctionDefinition::new(
            FunctionId::new(),
            QualifiedSemanticName::new(["sys", "health"]).unwrap(),
            FunctionDomain::Server,
            vec![],
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Boolean)),
            FunctionRevisionId::new(),
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Stable,
        );
        let base = CatalogueSnapshot::new_with_functions(
            CatalogueRevisionId::new(),
            vec![schema(1, &["sys"])],
            vec![],
            vec![function],
        )
        .unwrap();

        let report = check(
            &bundle([(
                "people.orna",
                "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (name TEXT);",
            )]),
            &base,
        );

        assert!(report.diagnostics().is_empty());
        let candidate = report.candidate().unwrap();
        assert!(candidate.functions().is_empty());
        assert_eq!(candidate.schemas().len(), 1);
        assert_eq!(candidate.schemas()[0].name().to_string(), "people");
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
        assert!(report.candidate().is_none());
    }
}
