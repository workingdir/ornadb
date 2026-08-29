//! Semantic resolution for parsed source bundles.
//!
//! The resolver consumes the `Parse` values retained by [`super::parse_bundle`].
//! It does not parse source text or expose syntax implementation values.

mod client;
mod identity;
mod model;
mod standard_library;
mod type_use;

pub(crate) use client::durable_state_slot_id;
use client::{
    ClientExpressionResultShape, check_client_functions, client_contract_identity,
    client_resource_targets, resolve_client_function_headers, resolve_client_function_inputs,
};
#[cfg(test)]
use client::{
    ClientExpressionType, ClientResourceTypeParser, client_local_resource_type,
    client_resource_stream_type_is_supported, validate_client_capability,
};

pub use identity::{
    CheckedExpressionId, CheckedFieldId, CheckedFunctionId, CheckedParameterId, CheckedSchemaId,
    CheckedTypeId, ProvisionalExpressionId, ProvisionalFieldId,
};

pub use standard_library::{
    check_standard_cli_repl, check_standard_json_encode, check_standard_library_source,
    check_standard_parameter_echo, check_standard_terminal_present_table,
    check_standard_ui_constructor, check_standard_ui_window,
};

pub use model::{
    CheckReport, CheckedApplicationTypeUse, CheckedBundle, CheckedClientBodyKind,
    CheckedClientCapability, CheckedClientCapabilityArgument, CheckedClientFunction,
    CheckedDefault, CheckedDefinitionReference, CheckedDefinitionReferenceTarget, CheckedField,
    CheckedObjectReferenceUse, CheckedObjectType, CheckedSchema, CheckedServerFunction,
    CheckedServerFunctionParameter, CheckedServerFunctionReturnColumn,
    CheckedStandardApplicationBundle, CheckedStandardApplicationClientFunction,
    CheckedStandardApplicationField, CheckedStandardApplicationObjectType,
    CheckedStandardApplicationParameter, CheckedStandardApplicationRecordValueField,
    CheckedStandardApplicationRecordValueType, CheckedStandardApplicationReturnColumn,
    CheckedStandardApplicationServerFunction, CheckedStandardExecutable, CheckedStandardJsonEncode,
    CheckedStandardLibrary, CheckedStandardParameterEcho, CheckedStandardSchema,
    CheckedStandardTerminalPresentTable, CheckedStandardTypeBinding, CheckedStandardTypeReference,
    CheckedStandardUiConstructor, CheckedStandardUiWindow, CheckedStandardValueType,
    CheckedTypeUseKind, CheckedValueTypeUse, ConstantValue, STANDARD_LIBRARY_V3_REVISION_ID,
    STANDARD_LIBRARY_V4_REVISION_ID, STANDARD_LIBRARY_V5_REVISION_ID,
    STANDARD_LIBRARY_V6_REVISION_ID, STANDARD_LIBRARY_V7_REVISION_ID,
    STANDARD_LIBRARY_V8_REVISION_ID, STANDARD_LIBRARY_V9_REVISION_ID,
    STANDARD_LIBRARY_V10_REVISION_ID, STD_ACTION_SCHEMA_ID, STD_ACTION_SOURCE_UNIT_ID,
    STD_ACTION_TYPE_ID, STD_BOOLEAN_TYPE_ID, STD_CHARACTER_LARGE_OBJECT_TYPE_ID,
    STD_CLI_REPL_FUNCTION_ID, STD_CLI_REPL_FUNCTION_REVISION_ID, STD_CLI_REPL_REVISION_NUMBER,
    STD_CLI_SCHEMA_ID, STD_CLI_SOURCE_UNIT_ID, STD_CSV_ENCODE_FUNCTION_ID,
    STD_DATA_ROWS_TYPE_BINDING_ID, STD_DATA_ROWS_TYPE_ID, STD_DATA_SCHEMA_ID,
    STD_DATA_SOURCE_UNIT_ID, STD_INTEGER_TYPE_ID, STD_INVOKE_ECHO_FUNCTION_ID,
    STD_INVOKE_ECHO_FUNCTION_REVISION_ID, STD_INVOKE_ECHO_PARAMETER_ID,
    STD_INVOKE_ECHO_REVISION_NUMBER, STD_INVOKE_SCHEMA_ID, STD_INVOKE_SOURCE_UNIT_ID,
    STD_IO_BYTE_STREAM_TYPE_ID, STD_IO_SCHEMA_ID, STD_JSON_ENCODE_FUNCTION_ID,
    STD_JSON_ENCODE_FUNCTION_REVISION_ID, STD_JSON_ENCODE_PARAMETER_ID, STD_JSON_SCHEMA_ID,
    STD_JSON_SOURCE_UNIT_ID, STD_JSON_VALUE_TYPE_ID, STD_OUTPUT_SOURCE_UNIT_ID,
    STD_TERMINAL_DOCUMENT_TYPE_ID, STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
    STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID, STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
    STD_TERMINAL_SCHEMA_ID, STD_TYPES_SOURCE_UNIT_ID, STD_UI_BUTTON_ENABLED_PARAMETER_ID,
    STD_UI_BUTTON_FUNCTION_ID, STD_UI_BUTTON_FUNCTION_REVISION_ID,
    STD_UI_BUTTON_LABEL_PARAMETER_ID, STD_UI_BUTTON_RUNTIME_CONTRACT,
    STD_UI_COLUMN_CONTENT_PARAMETER_ID, STD_UI_COLUMN_FUNCTION_ID,
    STD_UI_COLUMN_FUNCTION_REVISION_ID, STD_UI_COLUMN_RUNTIME_CONTRACT,
    STD_UI_CONSTRUCTORS_SOURCE_UNIT_ID, STD_UI_PANEL_CONTENT_PARAMETER_ID,
    STD_UI_PANEL_FUNCTION_ID, STD_UI_PANEL_FUNCTION_REVISION_ID, STD_UI_PANEL_RUNTIME_CONTRACT,
    STD_UI_ROW_CONTENT_PARAMETER_ID, STD_UI_ROW_FUNCTION_ID, STD_UI_ROW_FUNCTION_REVISION_ID,
    STD_UI_ROW_RUNTIME_CONTRACT, STD_UI_SCHEMA_ID, STD_UI_SOURCE_UNIT_ID,
    STD_UI_TABS_CONTENT_PARAMETER_ID, STD_UI_TABS_FUNCTION_ID, STD_UI_TABS_FUNCTION_REVISION_ID,
    STD_UI_TABS_RUNTIME_CONTRACT, STD_UI_TEXT_FUNCTION_ID, STD_UI_TEXT_FUNCTION_REVISION_ID,
    STD_UI_TEXT_INPUT_ENABLED_PARAMETER_ID, STD_UI_TEXT_INPUT_FUNCTION_ID,
    STD_UI_TEXT_INPUT_FUNCTION_REVISION_ID, STD_UI_TEXT_INPUT_PLACEHOLDER_PARAMETER_ID,
    STD_UI_TEXT_INPUT_RUNTIME_CONTRACT, STD_UI_TEXT_INPUT_TEXT_PARAMETER_ID,
    STD_UI_TEXT_PARAMETER_ID, STD_UI_TEXT_RUNTIME_CONTRACT, STD_UI_TYPE_ID,
    STD_UI_WINDOW_CONTENT_PARAMETER_ID, STD_UI_WINDOW_FUNCTION_ID,
    STD_UI_WINDOW_FUNCTION_REVISION_ID, STD_UI_WINDOW_REVISION_NUMBER,
    STD_UI_WINDOW_RUNTIME_CONTRACT, STD_UI_WINDOW_TITLE_PARAMETER_ID, STD_WINDOW_SOURCE_UNIT_ID,
    SemanticType, StandardApplicationCheckContext, StandardApplicationCheckReport,
    StandardApplicationContextError, StandardLibraryCheckError,
};
pub(crate) use model::{
    CheckedActionOperation, CheckedClientControlFlowBranch, CheckedClientControlFlowStatement,
    CheckedClientExpression, CheckedClientFunctionBody, CheckedClientLocal, CheckedClientLocalKind,
    CheckedClientReturnShape, CheckedClientStateSlot, CheckedClientStatement, CheckedFieldRename,
    CheckedInspectOperation, CheckedInspectProjection, CheckedResourceOperation,
    CheckedServerFunctionBody, CheckedServerFunctionReturn, CheckedStateDefault, CheckedStateScope,
    CheckedStateSlotId, QueryCatalogue, QueryField, QueryObjectType, ResolutionCatalogue,
    STD_ACTION_CONTRACT, STD_JSON_CONTRACT, STD_UI_CONTRACT,
};
use model::{CheckedEnumType, CheckedRecordValueField, CheckedRecordValueType};
#[cfg(test)]
pub(crate) use standard_library::checked_standard_library_with_contract_overrides_for_test;
#[cfg(test)]
use standard_library::{
    StandardSourceFamilies, check_standard_library_source_v1_identity,
    check_standard_library_source_v2_parts, check_standard_library_source_v3_parts,
    check_standard_library_source_v4_parts, check_standard_library_source_v5_parts,
    check_standard_library_source_v6_parts, expected_standard_json_executable,
    match_standard_source_facts, reconcile_standard_executable, reconcile_standard_json_executable,
    reconcile_standard_source, unquoted_prelude_name, unquoted_semantic_name,
    validate_standard_source_origins,
};

use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt,
};

use orna_artifact::client_plan::{
    ClientExpressionNode, ControlFlowBinaryOperator, ControlFlowUnaryOperator,
    ExpressionClientPlan, FORMAT_IDENTITY as CLIENT_PLAN_FORMAT, ResourceKind,
};
use orna_artifact::server_json_encode::{self, JsonEncodePlan};
use orna_artifact::server_parameter_echo::{self, ServerParameterEcho};
use orna_artifact::server_terminal_table;
use orna_core::{
    CallSiteId, ExpressionId, FunctionId, FunctionRevisionId, ParameterId, SchemaId, SourceUnitId,
    StateSlotId, TypeId,
    canonical_hash::{
        artifact_payload_digest, function_declaration_digest, function_semantic_digest_with_version,
    },
    catalogue::{
        CatalogueSnapshot, CatalogueSnapshotError, FunctionDomain, FunctionReturn,
        FunctionSecurity as CatalogueFunctionSecurity,
        FunctionTransaction as CatalogueFunctionTransaction,
        FunctionVolatility as CatalogueFunctionVolatility, OnDeleteAction, PreludeTypeName,
        QualifiedSemanticName, TypeBindingKind, TypeLookupName, ValueTypeKind, ValueTypeMutability,
        ValueTypePersistence,
    },
    inspect::{INSPECT_RENDER_CARRIER_SIGNATURE, INSPECT_RENDER_CONTRACT},
    revision::{
        DefinitionIdentity, DefinitionOrigin, DefinitionReference, DefinitionReferenceKind,
        DefinitionReferenceTarget, EMPTY_APPLICATION_CATALOGUE_REVISION_ID, ExecutableArtifact,
        ExecutableArtifactKind, FunctionRevisionRecord, FunctionSemanticHashVersion, SourceOrigin,
        StandardExecutable, StandardLibraryDigestVersion, StoredSourceRevision, StoredSourceUnit,
        VerifiedStandardLibrarySnapshot,
    },
    source::{SourceBundle, SourceUnit},
    system::SYS_SOURCE_FUNCTION_TYPE_ID,
    types::{ResolvedType, StandardScalar},
};
use orna_syntax::{
    CapabilitySpecification, ClientExpression, ClientFunctionDeclaration, FieldRenameDeclaration,
    FunctionReturnType, FunctionSecurity as SyntaxFunctionSecurity,
    FunctionTransaction as SyntaxFunctionTransaction,
    FunctionVolatility as SyntaxFunctionVolatility, NamePart, ObjectTypeDeclaration,
    OnDeletePolicy, OptionTypeSpelling, PrimitiveValueTypePersistence, QualifiedName,
    RecordValueTypeDeclaration, SelectQuantifier, ServerFunctionBody, ServerFunctionDeclaration,
    SourceSlice, SourceSpan, StandardLargeObjectKind, StateDefault, StateScope, TypeExportTarget,
    TypeSpecification,
};

use crate::mutation::{
    MutationCatalogue, MutationField, MutationParameter, MutationReference, check_delete_in,
    check_delete_with_intrinsic_boolean_in, check_insert_in,
    check_insert_with_intrinsic_boolean_in, check_update_in,
    check_update_with_intrinsic_boolean_in,
};
use crate::relational::{
    ExpressionIr, IdentitySelectedQueryReference, IntrinsicBooleanType, QueryParameter,
    QueryReference, QueryReferenceKind, QueryReferenceTarget, UniqueTextSelectedQueryReference,
    check_distinct_query_with_intrinsic_boolean_in,
    check_identity_selected_query_with_intrinsic_boolean_in, check_query_with_intrinsic_boolean_in,
    check_unique_text_selected_query_with_intrinsic_boolean_in,
};
use crate::{
    CompilerDiagnostic, DiagnosticCode, ParseReport, ParsedSourceUnit, SourceLocation,
    normalise_name_part as semantic_part, normalise_qualified_name as semantic_name, parse_bundle,
};

use self::identity::{CheckAssignments, IdentityAssignments};
use self::type_use::{StandardTypeUseRecorder, record_standard_type_use};

/// Checks one source bundle against an immutable catalogue snapshot.
///
/// This function parses the bundle exactly once. Resolution consumes the owned
/// `Parse` values that [`parse_bundle`] retains in the resulting report.
pub fn check(bundle: &SourceBundle, base: &CatalogueSnapshot) -> CheckReport {
    check_parsed(parse_bundle(bundle), base)
}

/// An error that prevents a new application from being checked offline.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NewApplicationCheckError {
    /// The submitted bundle does not contain exactly one source unit.
    SourceUnitCount {
        /// The submitted source-unit count.
        actual: usize,
    },
    /// The compiler could not construct its empty application catalogue.
    Catalogue {
        /// The exact empty-catalogue construction failure.
        source: CatalogueSnapshotError,
    },
    /// The checked standard library cannot establish application authority.
    Context {
        /// The exact standard-application context failure.
        source: StandardApplicationContextError,
    },
}

impl fmt::Display for NewApplicationCheckError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourceUnitCount { actual } => write!(
                formatter,
                "new-application check requires exactly one source unit; received {actual}"
            ),
            Self::Catalogue { source } => write!(
                formatter,
                "new-application check could not create the empty application catalogue: {source}"
            ),
            Self::Context { source } => write!(
                formatter,
                "new-application check could not establish the standard application context: {source}"
            ),
        }
    }
}

impl Error for NewApplicationCheckError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::SourceUnitCount { .. } => None,
            Self::Catalogue { source } => Some(source),
            Self::Context { source } => Some(source),
        }
    }
}

/// Checks one new application source file against checked standard-library authority.
///
/// The check uses an ephemeral empty catalogue. It cannot supply application
/// continuity to renames, deletions, or references that require prior state.
///
/// ```compile_fail
/// use orna_compiler::{CheckReport, StandardApplicationCheckReport};
///
/// fn cannot_convert(report: &StandardApplicationCheckReport) {
///     let _: &CheckReport = report;
/// }
/// ```
///
/// ```compile_fail
/// use orna_compiler::{CheckedBundle, CheckedStandardApplicationBundle};
///
/// fn cannot_convert(bundle: &CheckedStandardApplicationBundle) {
///     let _: &CheckedBundle = bundle;
/// }
/// ```
pub fn check_new_application(
    bundle: &SourceBundle,
    standard: &CheckedStandardLibrary,
) -> Result<StandardApplicationCheckReport, NewApplicationCheckError> {
    check_new_application_with_catalogue(bundle, standard, empty_application_catalogue)
}

fn check_new_application_with_catalogue(
    bundle: &SourceBundle,
    standard: &CheckedStandardLibrary,
    create_catalogue: impl FnOnce() -> Result<CatalogueSnapshot, CatalogueSnapshotError>,
) -> Result<StandardApplicationCheckReport, NewApplicationCheckError> {
    if bundle.len() != 1 {
        return Err(NewApplicationCheckError::SourceUnitCount {
            actual: bundle.len(),
        });
    }

    let application =
        create_catalogue().map_err(|source| NewApplicationCheckError::Catalogue { source })?;
    let context = StandardApplicationCheckContext::try_new(&application, standard)
        .map_err(|source| NewApplicationCheckError::Context { source })?;

    Ok(check_standard_application(bundle, &context))
}

fn empty_application_catalogue() -> Result<CatalogueSnapshot, CatalogueSnapshotError> {
    CatalogueSnapshot::new(
        EMPTY_APPLICATION_CATALOGUE_REVISION_ID,
        Vec::new(),
        Vec::new(),
    )
}

impl<'a> StandardApplicationCheckContext<'a> {
    /// Establishes authority for standard-backed application checking.
    ///
    /// The checked standard-library capability is already reconciled with its
    /// verified snapshot. This constructor checks only that the application
    /// catalogue cannot collide with that authority and that the compiler can
    /// derive one private compatibility scalar for every standard value type.
    pub fn try_new(
        application: &'a CatalogueSnapshot,
        standard: &'a CheckedStandardLibrary,
    ) -> Result<Self, StandardApplicationContextError> {
        for schema in standard.schemas() {
            if application.schema_by_id(schema.id()).is_some() {
                return Err(StandardApplicationContextError::SchemaIdentityConflict {
                    id: schema.id(),
                });
            }
        }

        for schema in standard.schemas() {
            if application.schema_by_name(schema.name()).is_some() {
                return Err(StandardApplicationContextError::SchemaNameConflict {
                    name: schema.name().clone(),
                });
            }
        }

        for value_type in standard.value_types() {
            if application.type_definition_by_id(value_type.id()).is_some() {
                return Err(StandardApplicationContextError::TypeIdentityConflict {
                    id: value_type.id(),
                });
            }
        }

        for binding in standard.type_bindings() {
            if application.type_binding_by_id(binding.id()).is_some() {
                return Err(
                    StandardApplicationContextError::TypeBindingIdentityConflict {
                        id: binding.id(),
                    },
                );
            }
        }

        for value_type in standard.value_types() {
            if value_type.kind() == ValueTypeKind::Primitive
                && compatibility_scalar(value_type.representation_contract()).is_none()
            {
                return Err(
                    StandardApplicationContextError::UnsupportedCompatibilityContract {
                        type_id: value_type.id(),
                        contract: value_type.representation_contract().to_owned(),
                    },
                );
            }
        }

        let mut contracts = HashSet::with_capacity(standard.value_types().len());
        for value_type in standard.value_types() {
            let contract = value_type.representation_contract();
            if !contracts.insert(contract) {
                return Err(
                    StandardApplicationContextError::CompatibilityContractConflict {
                        contract: contract.to_owned(),
                    },
                );
            }
        }

        Ok(Self {
            application,
            standard,
        })
    }
}

/// Checks one application source bundle against checked standard-library authority.
///
/// This is intentionally distinct from [`check`]: it resolves standard values
/// through durable type identities and returns no legacy checked-bundle escape.
pub fn check_standard_application(
    bundle: &SourceBundle,
    context: &StandardApplicationCheckContext<'_>,
) -> StandardApplicationCheckReport {
    let mut result = check_application_parsed(
        parse_bundle(bundle),
        context.application,
        Some(context.standard),
    );
    sort_standard_type_uses(&mut result.uses, &result.parse_report);
    let ApplicationCheckResult {
        parse_report,
        diagnostics,
        checked_bundle,
        uses,
    } = result;
    let use_indices = uses
        .iter()
        .enumerate()
        .map(|(index, type_use)| (type_use.kind(), index))
        .collect();
    let snapshot = context.standard.verified_snapshot();
    let checked_bundle = checked_bundle.map(|inner| {
        let standard_type_references =
            collect_standard_type_references(&uses, &inner, &parse_report);
        let preparation_evidence = model::StandardApplicationPreparationEvidence::from_canonical(
            &uses,
            &standard_type_references,
        );
        CheckedStandardApplicationBundle {
            inner,
            standard_catalogue_revision: snapshot.catalogue().revision(),
            standard_library_revision: snapshot.revision(),
            standard_library_digest: snapshot.digest(),
            uses,
            standard_type_references,
            use_indices,
            preparation_evidence,
        }
    });

    StandardApplicationCheckReport {
        standard_library: context.standard.clone(),
        parse_report,
        diagnostics,
        checked_bundle,
    }
}

fn decode_string_literal(slice: &SourceSlice) -> Option<String> {
    let text = slice.text.strip_prefix('\'')?.strip_suffix('\'')?;
    let mut decoded = String::with_capacity(text.len());
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        if character == '\'' && characters.next() != Some('\'') {
            return None;
        }
        decoded.push(character);
    }
    Some(decoded)
}

fn opaque_contract_is_valid(contract: &str) -> bool {
    !contract.is_empty()
        && contract.len() <= 128
        && contract.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
}

#[derive(Clone, Copy)]
struct Header<'a> {
    declaration: &'a ObjectTypeDeclaration,
    logical_path: &'a str,
    id: CheckedTypeId,
}

#[derive(Clone, Copy)]
struct RecordValueHeader<'a> {
    declaration: &'a RecordValueTypeDeclaration,
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

/// Resolved metadata for a CLIENT function before closed-shape checking.
#[derive(Clone, Copy)]
struct ClientFunctionHeader<'a> {
    declaration: &'a ClientFunctionDeclaration,
    logical_path: &'a str,
    id: CheckedFunctionId,
}

/// One resolved CLIENT function body and its closed semantic metadata.
struct ResolvedClientFunctionInput<'a> {
    id: CheckedFunctionId,
    name: QualifiedSemanticName,
    parameters: Vec<ResolvedServerFunctionParameter>,
    return_type: SemanticType<CheckedTypeId>,
    standard_value_type: Option<orna_core::TypeId>,
    result_shape: ClientExpressionResultShape,
    return_shape: CheckedClientReturnShape,
    body: &'a orna_syntax::ClientFunctionBody,
    capabilities: &'a [CapabilitySpecification],
    location: SourceLocation,
    declaration_span: SourceSpan,
    logical_path: &'a str,
    /// Whether this function uses the ADR 0020 expression/statement surface.
    control_flow_required: bool,
}

/// One function declaration in source order, independent of its execution domain.
enum FunctionDeclarationRef<'a> {
    Server {
        declaration: &'a ServerFunctionDeclaration,
        logical_path: &'a str,
    },
    Client {
        declaration: &'a ClientFunctionDeclaration,
        logical_path: &'a str,
    },
}

impl FunctionDeclarationRef<'_> {
    fn name(&self) -> QualifiedSemanticName {
        match self {
            Self::Server { declaration, .. } => semantic_name(&declaration.name),
            Self::Client { declaration, .. } => semantic_name(&declaration.name),
        }
    }

    fn domain(&self) -> FunctionDomain {
        match self {
            Self::Server { .. } => FunctionDomain::Server,
            Self::Client { .. } => FunctionDomain::Client,
        }
    }

    fn span(&self) -> &SourceSpan {
        match self {
            Self::Server { declaration, .. } => &declaration.name.span,
            Self::Client { declaration, .. } => &declaration.name.span,
        }
    }

    fn logical_path(&self) -> &str {
        match self {
            Self::Server { logical_path, .. } | Self::Client { logical_path, .. } => logical_path,
        }
    }
}

/// One resolved parameter accepted by this SERVER function slice.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ResolvedServerFunctionParameter {
    id: CheckedParameterId,
    name: String,
    ordinal: u32,
    semantic_type: SemanticType<CheckedTypeId>,
    standard_value_type: Option<TypeId>,
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
    standard_value_type: Option<TypeId>,
    location: SourceLocation,
    reference_location: Option<SourceLocation>,
}

/// The resolved result shape before relational planning.
#[derive(Clone, Debug, Eq, PartialEq)]
enum ResolvedServerFunctionReturn {
    Single {
        semantic_type: SemanticType<CheckedTypeId>,
        standard_value_type: Option<TypeId>,
        location: SourceLocation,
    },
    Stream {
        semantic_type: SemanticType<CheckedTypeId>,
        standard_value_type: Option<TypeId>,
        location: SourceLocation,
        reference_location: Option<SourceLocation>,
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
    let result = check_application_parsed(parse_report, base, None);
    CheckReport {
        parse_report: result.parse_report,
        diagnostics: result.diagnostics,
        checked_bundle: result.checked_bundle,
    }
}

struct ApplicationCheckResult {
    parse_report: ParseReport,
    diagnostics: Vec<CompilerDiagnostic>,
    checked_bundle: Option<CheckedBundle>,
    uses: Vec<CheckedApplicationTypeUse>,
}

fn check_application_parsed(
    parse_report: ParseReport,
    base: &CatalogueSnapshot,
    standard: Option<&CheckedStandardLibrary>,
) -> ApplicationCheckResult {
    let mut diagnostics = parse_report.diagnostics().to_vec();
    if !diagnostics.is_empty() {
        return application_failed(parse_report, diagnostics);
    }

    diagnostics.extend(check_protected_source(&parse_report));
    if !diagnostics.is_empty() {
        return application_failed(parse_report, diagnostics);
    }

    let mut assignments = CheckAssignments::new();
    let mut uses = Vec::new();
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
    let mut declarations_by_name = HashSet::<QualifiedSemanticName>::new();
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
            if !declarations_by_name.insert(name.clone()) {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DuplicateDefinition,
                    format!("duplicate object type definition {name}"),
                    unit.logical_path(),
                    &declaration.name.span,
                ));
                continue;
            }
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

    let mut checked_enum_types = Vec::new();
    for unit in parse_report.units() {
        for declaration in unit.parsed().enum_types() {
            let name = semantic_name(&declaration.name);
            let Some(namespace) = namespace_of(&name) else {
                diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    format!("enum type {name} has no declared schema"),
                    unit.logical_path(),
                    &declaration.name.span,
                ));
                continue;
            };
            if !known_schemas.contains(&namespace) {
                diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    format!("unknown schema {namespace} for enum type {name}"),
                    unit.logical_path(),
                    &declaration.name.span,
                ));
                continue;
            }
            if !declarations_by_name.insert(name.clone()) {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DuplicateDefinition,
                    format!("duplicate enum type definition {name}"),
                    unit.logical_path(),
                    &declaration.name.span,
                ));
                continue;
            }

            let mut labels = Vec::with_capacity(declaration.labels.len());
            let mut distinct_labels = HashSet::with_capacity(declaration.labels.len());
            let mut valid = true;
            for label in &declaration.labels {
                let decoded = decode_string_literal(&label.literal)
                    .expect("parser accepted one complete enum string literal");
                if !distinct_labels.insert(decoded.clone()) {
                    diagnostics.push(diagnostic(
                        DiagnosticCode::DuplicateDefinition,
                        format!("duplicate enum label {decoded:?} in {name}"),
                        unit.logical_path(),
                        &label.literal.span,
                    ));
                    valid = false;
                }
                labels.push(decoded);
            }
            if !valid {
                continue;
            }

            checked_enum_types.push(CheckedEnumType {
                id: assignments.type_id(
                    base.enum_type_by_name(&name)
                        .map(|enum_type| enum_type.id()),
                ),
                name,
                labels,
                location: location(unit.logical_path(), &declaration.span),
            });
        }
    }

    let mut record_value_headers = Vec::new();
    for unit in parse_report.units() {
        for declaration in unit.parsed().record_value_types() {
            let name = semantic_name(&declaration.name);
            let Some(namespace) = namespace_of(&name) else {
                diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    format!("record value type {name} has no declared schema"),
                    unit.logical_path(),
                    &declaration.name.span,
                ));
                continue;
            };
            if !known_schemas.contains(&namespace) {
                diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    format!("unknown schema {namespace} for record value type {name}"),
                    unit.logical_path(),
                    &declaration.name.span,
                ));
                continue;
            }
            if !declarations_by_name.insert(name.clone()) {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DuplicateDefinition,
                    format!("duplicate record value type definition {name}"),
                    unit.logical_path(),
                    &declaration.name.span,
                ));
                continue;
            }
            if standard.is_none() {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DomainIncompatible,
                    "record value types require checked standard-library authority",
                    unit.logical_path(),
                    &declaration.name.span,
                ));
                continue;
            }
            record_value_headers.push(RecordValueHeader {
                declaration,
                logical_path: unit.logical_path(),
                id: assignments.type_id(
                    base.record_value_type_by_name(&name)
                        .map(|record_value_type| record_value_type.id()),
                ),
            });
        }
    }

    let mut submitted_ids = HashMap::new();
    for header in &headers {
        submitted_ids.insert(
            semantic_name(&header.declaration.name),
            SubmittedType::Object(header.id),
        );
    }
    for enum_type in &checked_enum_types {
        submitted_ids.insert(enum_type.name.clone(), SubmittedType::Enum(enum_type.id));
    }
    for header in &record_value_headers {
        submitted_ids.insert(
            semantic_name(&header.declaration.name),
            SubmittedType::RecordValue(header.id),
        );
    }

    let mut checked_record_value_types = Vec::with_capacity(record_value_headers.len());
    for header in &record_value_headers {
        let type_name = semantic_name(&header.declaration.name);
        let base_type = base.record_value_type_by_name(&type_name);
        let mut field_names = HashSet::new();
        let mut fields = Vec::with_capacity(header.declaration.fields.len());
        for field in &header.declaration.fields {
            let name = semantic_part(&field.name);
            if !field_names.insert(name.clone()) {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DuplicateDefinition,
                    format!("duplicate record field definition {name} in {type_name}"),
                    header.logical_path,
                    &field.name.span,
                ));
                continue;
            }
            let Some(resolved_type) = resolve_record_value_field_type(
                &field.type_specification,
                &submitted_ids,
                header.logical_path,
                &mut diagnostics,
                standard.expect("record value headers require checked standard authority"),
            ) else {
                continue;
            };
            let id = assignments.field_id(
                base_type
                    .and_then(|record_value_type| record_value_type.field_by_name(&name))
                    .map(|field| field.id()),
            );
            fields.push(CheckedRecordValueField {
                id,
                name,
                ordinal: field.order as u32,
                semantic_type: resolved_type.semantic_type,
                location: location(header.logical_path, &field.span),
            });
            record_standard_type_use(
                &mut uses,
                standard,
                CheckedTypeUseKind::Field {
                    owner: header.id,
                    field: id,
                },
                resolved_type,
                type_use_location(&field.type_specification, header.logical_path),
            );
        }
        checked_record_value_types.push(CheckedRecordValueType {
            id: header.id,
            name: type_name,
            fields,
            location: location(header.logical_path, &header.declaration.span),
        });
    }

    let diagnostics_before_record_value_graph = diagnostics.len();
    if diagnostics_before_record_value_graph == 0 {
        validate_record_value_field_graph(
            &record_value_headers,
            &checked_record_value_types,
            &mut diagnostics,
        );
    }
    if diagnostics.len() != diagnostics_before_record_value_graph {
        return application_failed(parse_report, diagnostics);
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

            let resolved_type = resolve_application_type(
                &field.type_specification,
                &submitted_ids,
                header.logical_path,
                &mut diagnostics,
                standard,
            );
            let semantic_type = resolved_type.map(|resolved| resolved.semantic_type);
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

            if field.unique
                && semantic_type.is_some_and(|semantic_type| {
                    !supports_unique_text_or_required_reference(semantic_type, field.nullable)
                })
            {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    UNIQUE_FIELD_MESSAGE,
                    header.logical_path,
                    &field.span,
                ));
            }

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
                if let Some(resolved_type) = resolved_type {
                    record_standard_type_use(
                        &mut uses,
                        standard,
                        CheckedTypeUseKind::Field {
                            owner: header.id,
                            field: id,
                        },
                        resolved_type,
                        type_use_location(&field.type_specification, header.logical_path),
                    );
                }
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
        return application_failed(parse_report, diagnostics);
    }

    reject_unplanned_server_function_features(&parse_report, &mut diagnostics);
    if !diagnostics.is_empty() {
        return application_failed(parse_report, diagnostics);
    }

    let query_catalogue = checked_query_catalogue(&checked_types, &uses);

    let function_ids = if diagnostics.is_empty() {
        resolve_function_namespace(
            &parse_report,
            base,
            &known_schemas,
            &mut assignments,
            &mut diagnostics,
        )
    } else {
        HashMap::new()
    };
    let function_headers = if diagnostics.is_empty() {
        resolve_server_function_headers(&parse_report, &function_ids)
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
            standard,
            &mut uses,
        )
    } else {
        Vec::new()
    };
    let checked_functions = if diagnostics.is_empty() {
        check_server_functions(
            &function_inputs,
            &query_catalogue,
            &checked_record_value_types,
            &checked_enum_types,
            &mut diagnostics,
            standard,
            &mut uses,
        )
    } else {
        Vec::new()
    };
    let client_headers = if diagnostics.is_empty() {
        resolve_client_function_headers(&parse_report, &function_ids)
    } else {
        Vec::new()
    };
    let client_inputs = if diagnostics.is_empty() {
        resolve_client_function_inputs(
            &client_headers,
            &submitted_ids,
            base,
            &mut assignments,
            &mut diagnostics,
            standard,
            &mut uses,
        )
    } else {
        Vec::new()
    };
    let checked_client_functions = if diagnostics.is_empty() {
        let server_names = function_inputs
            .iter()
            .map(|input| input.name.clone())
            .collect::<Vec<_>>();
        check_client_functions(
            &client_inputs,
            &function_inputs,
            &submitted_ids,
            &query_catalogue,
            &server_names,
            &client_resource_targets(&function_inputs, base, standard),
            base,
            &mut diagnostics,
            standard,
            &mut uses,
        )
    } else {
        Vec::new()
    };

    if !diagnostics.is_empty() {
        return application_failed(parse_report, diagnostics);
    }

    ApplicationCheckResult {
        parse_report,
        diagnostics,
        checked_bundle: Some(CheckedBundle {
            base_catalogue_revision: base.revision(),
            schemas: checked_schemas,
            object_types: checked_types,
            enum_types: checked_enum_types,
            record_value_types: checked_record_value_types,
            server_functions: checked_functions,
            client_functions: checked_client_functions,
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
        uses,
    }
}

fn check_protected_source(parse_report: &ParseReport) -> Vec<CompilerDiagnostic> {
    let mut diagnostics = Vec::new();
    let mut protected_declarations = HashSet::new();

    for (unit_index, unit) in parse_report.units().iter().enumerate() {
        let mut owners = Vec::new();
        for declaration in unit.parsed().schemas() {
            owners.push((&declaration.name, &declaration.name.span, &declaration.span));
        }
        for declaration in unit.parsed().object_types() {
            owners.push((&declaration.name, &declaration.name.span, &declaration.span));
        }
        for declaration in unit.parsed().enum_types() {
            owners.push((&declaration.name, &declaration.name.span, &declaration.span));
        }
        for declaration in unit.parsed().record_value_types() {
            owners.push((&declaration.name, &declaration.name.span, &declaration.span));
        }
        for declaration in unit.parsed().primitive_value_types() {
            owners.push((&declaration.name, &declaration.name.span, &declaration.span));
        }
        for declaration in unit.parsed().opaque_value_types() {
            owners.push((&declaration.name, &declaration.name.span, &declaration.span));
        }
        for declaration in unit.parsed().field_renames() {
            owners.push((
                &declaration.type_name,
                &declaration.type_name.span,
                &declaration.span,
            ));
        }
        for declaration in unit.parsed().server_functions() {
            owners.push((&declaration.name, &declaration.name.span, &declaration.span));
        }
        for declaration in unit.parsed().client_functions() {
            owners.push((&declaration.name, &declaration.name.span, &declaration.span));
        }
        for declaration in unit.parsed().type_exports() {
            if let orna_syntax::TypeExportTarget::Qualified { name } = &declaration.target {
                owners.push((name, &name.span, &declaration.span));
            }
        }
        owners.sort_by_key(|(_, _, declaration_span)| declaration_span.start);
        for (name, span, declaration_span) in owners {
            if name
                .parts
                .first()
                .is_some_and(|part| semantic_part(part) == "std")
            {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DomainIncompatible,
                    "the std namespace is owned by the standard library",
                    unit.logical_path(),
                    span,
                ));
                protected_declarations.insert((
                    unit_index,
                    declaration_span.start,
                    declaration_span.end,
                ));
            }
        }
    }

    for (unit_index, unit) in parse_report.units().iter().enumerate() {
        let protected_value_declarations = unit
            .parsed()
            .primitive_value_types()
            .iter()
            .map(|declaration| {
                (
                    &declaration.span,
                    &declaration.kernel_contract_modifier_span,
                )
            })
            .chain(
                unit.parsed()
                    .opaque_value_types()
                    .iter()
                    .map(|declaration| {
                        (
                            &declaration.span,
                            &declaration.kernel_contract_modifier_span,
                        )
                    }),
            );
        for (declaration_span, modifier_span) in protected_value_declarations {
            if !protected_declarations.contains(&(
                unit_index,
                declaration_span.start,
                declaration_span.end,
            )) {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DomainIncompatible,
                    "KERNEL CONTRACT is available only to the standard library",
                    unit.logical_path(),
                    modifier_span,
                ));
                protected_declarations.insert((
                    unit_index,
                    declaration_span.start,
                    declaration_span.end,
                ));
            }
        }
    }

    for (unit_index, unit) in parse_report.units().iter().enumerate() {
        for declaration in unit.parsed().type_exports() {
            if protected_declarations.contains(&(
                unit_index,
                declaration.span.start,
                declaration.span.end,
            )) {
                continue;
            }
            if let orna_syntax::TypeExportTarget::Qualified { name } = &declaration.target {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DomainIncompatible,
                    "qualified type exports are available only to the standard library",
                    unit.logical_path(),
                    &name.span,
                ));
                protected_declarations.insert((
                    unit_index,
                    declaration.span.start,
                    declaration.span.end,
                ));
            }
        }
    }

    for (unit_index, unit) in parse_report.units().iter().enumerate() {
        for declaration in unit.parsed().type_exports() {
            if protected_declarations.contains(&(
                unit_index,
                declaration.span.start,
                declaration.span.end,
            )) {
                continue;
            }
            if let orna_syntax::TypeExportTarget::Prelude { modifier_span, .. } =
                &declaration.target
            {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DomainIncompatible,
                    "only the standard library can export a type to the prelude",
                    unit.logical_path(),
                    modifier_span,
                ));
            }
        }
    }

    diagnostics
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
    function_ids: &HashMap<QualifiedSemanticName, CheckedFunctionId>,
) -> Vec<ServerFunctionHeader<'a>> {
    let mut headers = Vec::new();
    let mut declarations_by_name = HashSet::new();

    for unit in parse_report.units() {
        for declaration in unit.parsed().server_functions() {
            let name = semantic_name(&declaration.name);
            if !declarations_by_name.insert(name.clone()) {
                continue;
            }

            let Some(&id) = function_ids.get(&name) else {
                continue;
            };

            let security = map_function_security(declaration.security);
            let transaction = map_function_transaction(declaration.transaction);
            let volatility = map_function_volatility(declaration.volatility);

            headers.push(ServerFunctionHeader {
                declaration,
                logical_path: unit.logical_path(),
                id,
                security,
                transaction,
                volatility,
            });
        }
    }

    headers
}

fn function_declarations_in_source_order<'a>(
    parse_report: &'a ParseReport,
) -> Vec<FunctionDeclarationRef<'a>> {
    let mut declarations = Vec::new();
    for unit in parse_report.units() {
        let mut unit_declarations = Vec::with_capacity(
            unit.parsed().server_functions().len() + unit.parsed().client_functions().len(),
        );
        unit_declarations.extend(unit.parsed().server_functions().iter().map(|declaration| {
            FunctionDeclarationRef::Server {
                declaration,
                logical_path: unit.logical_path(),
            }
        }));
        unit_declarations.extend(unit.parsed().client_functions().iter().map(|declaration| {
            FunctionDeclarationRef::Client {
                declaration,
                logical_path: unit.logical_path(),
            }
        }));
        unit_declarations.sort_by_key(|declaration| declaration.span().start);
        declarations.extend(unit_declarations);
    }
    declarations
}

/// Resolves the shared function namespace before any function body checking.
///
/// This pass gives CLIENT and SERVER declarations one source-order identity
/// stream. It also rejects a domain change for an active function before a
/// later resolver stage can inspect its body.
fn resolve_function_namespace(
    parse_report: &ParseReport,
    base: &CatalogueSnapshot,
    known_schemas: &HashSet<QualifiedSemanticName>,
    assignments: &mut CheckAssignments,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) -> HashMap<QualifiedSemanticName, CheckedFunctionId> {
    let mut function_ids = HashMap::new();
    let mut declarations_by_name = HashMap::<QualifiedSemanticName, FunctionDomain>::new();

    for declaration in function_declarations_in_source_order(parse_report) {
        let name = declaration.name();
        if let Some(previous_domain) = declarations_by_name.get(&name).copied() {
            let message = match (previous_domain, declaration.domain()) {
                (FunctionDomain::Server, FunctionDomain::Server) => {
                    format!("duplicate server function definition {name}")
                }
                (FunctionDomain::Client, FunctionDomain::Client) => {
                    format!("duplicate client function definition {name}")
                }
                _ => format!("duplicate function definition {name}"),
            };
            diagnostics.push(diagnostic(
                DiagnosticCode::DuplicateDefinition,
                message,
                declaration.logical_path(),
                declaration.span(),
            ));
            continue;
        }
        declarations_by_name.insert(name.clone(), declaration.domain());

        let Some(namespace) = namespace_of(&name) else {
            let kind = match declaration.domain() {
                FunctionDomain::Server => "server",
                FunctionDomain::Client => "client",
            };
            diagnostics.push(diagnostic(
                DiagnosticCode::UnknownQualifiedName,
                format!("{kind} function {name} has no declared schema"),
                declaration.logical_path(),
                declaration.span(),
            ));
            continue;
        };
        if !known_schemas.contains(&namespace) {
            let kind = match declaration.domain() {
                FunctionDomain::Server => "server",
                FunctionDomain::Client => "client",
            };
            diagnostics.push(diagnostic(
                DiagnosticCode::UnknownQualifiedName,
                format!("unknown schema {namespace} for {kind} function {name}"),
                declaration.logical_path(),
                declaration.span(),
            ));
            continue;
        }

        let existing = base.function_by_name(&name);
        if let Some(existing) = existing
            && existing.domain() != declaration.domain()
        {
            let existing_domain = match existing.domain() {
                FunctionDomain::Server => "SERVER",
                FunctionDomain::Client => "CLIENT",
            };
            diagnostics.push(diagnostic(
                DiagnosticCode::DomainIncompatible,
                format!("this function is already declared as a {existing_domain} function"),
                declaration.logical_path(),
                declaration.span(),
            ));
            continue;
        }

        if let FunctionDeclarationRef::Server {
            declaration: server,
            logical_path,
        } = &declaration
            && map_function_transaction(server.transaction)
                == Some(CatalogueFunctionTransaction::Manual)
        {
            diagnostics.push(diagnostic(
                DiagnosticCode::DomainIncompatible,
                "SERVER functions do not yet support TRANSACTION MANUAL",
                logical_path,
                &server.span,
            ));
            continue;
        }

        let id = assignments.function_id(existing.map(|function| function.id()));
        function_ids.insert(name, id);
    }

    function_ids
}

fn resolve_server_function_inputs<'a>(
    headers: &[ServerFunctionHeader<'a>],
    submitted_ids: &HashMap<QualifiedSemanticName, SubmittedType>,
    base: &CatalogueSnapshot,
    assignments: &mut CheckAssignments,
    diagnostics: &mut Vec<CompilerDiagnostic>,
    standard: Option<&CheckedStandardLibrary>,
    uses: &mut Vec<CheckedApplicationTypeUse>,
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

            let Some(resolved_type) = resolve_application_type_with_named_standard(
                &parameter.type_specification,
                submitted_ids,
                header.logical_path,
                diagnostics,
                standard,
                true,
            ) else {
                continue;
            };
            let id = assignments.parameter_id(
                base_function
                    .and_then(|function| function.parameter_by_name(&parameter_name))
                    .map(|parameter| parameter.id()),
            );
            record_standard_type_use(
                uses,
                standard,
                CheckedTypeUseKind::Parameter {
                    owner: header.id,
                    parameter: id,
                },
                resolved_type,
                type_use_location(&parameter.type_specification, header.logical_path),
            );
            parameters.push(ResolvedServerFunctionParameter {
                id,
                name: parameter_name,
                ordinal: parameter.order as u32,
                semantic_type: resolved_type.semantic_type,
                standard_value_type: resolved_type.standard_value_type,
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
            standard,
            header.id,
            uses,
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
    submitted_ids: &HashMap<QualifiedSemanticName, SubmittedType>,
    logical_path: &str,
    diagnostics: &mut Vec<CompilerDiagnostic>,
    standard: Option<&CheckedStandardLibrary>,
    owner: CheckedFunctionId,
    uses: &mut Vec<CheckedApplicationTypeUse>,
) -> Option<ResolvedServerFunctionReturn> {
    match return_type {
        FunctionReturnType::Single(specification) => resolve_application_type_with_named_standard(
            specification,
            submitted_ids,
            logical_path,
            diagnostics,
            standard,
            true,
        )
        .map(|resolved| {
            record_standard_type_use(
                uses,
                standard,
                CheckedTypeUseKind::Return { owner, ordinal: 0 },
                resolved,
                type_use_location(specification, logical_path),
            );
            ResolvedServerFunctionReturn::Single {
                semantic_type: resolved.semantic_type,
                standard_value_type: resolved.standard_value_type,
                location: location(logical_path, specification.span()),
            }
        }),
        FunctionReturnType::Stream { element, span } => {
            resolve_application_type_with_named_standard(
                element,
                submitted_ids,
                logical_path,
                diagnostics,
                standard,
                true,
            )
            .map(|resolved| {
                record_standard_type_use(
                    uses,
                    standard,
                    CheckedTypeUseKind::Return { owner, ordinal: 0 },
                    resolved,
                    type_use_location(element, logical_path),
                );
                ResolvedServerFunctionReturn::Stream {
                    semantic_type: resolved.semantic_type,
                    standard_value_type: resolved.standard_value_type,
                    location: location(logical_path, span),
                    reference_location: reference_location(element, logical_path),
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
                let Some(resolved_type) = resolve_application_type(
                    &column.type_specification,
                    submitted_ids,
                    logical_path,
                    diagnostics,
                    standard,
                ) else {
                    continue;
                };
                record_standard_type_use(
                    uses,
                    standard,
                    CheckedTypeUseKind::Return {
                        owner,
                        ordinal: column.order as u32,
                    },
                    resolved_type,
                    type_use_location(&column.type_specification, logical_path),
                );
                resolved_columns.push(ResolvedServerFunctionReturnColumn {
                    name,
                    ordinal: column.order as u32,
                    semantic_type: resolved_type.semantic_type,
                    standard_value_type: resolved_type.standard_value_type,
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

struct RecordAwareMutationCatalogue<'a> {
    objects: &'a ResolutionCatalogue<CheckedTypeId, CheckedFieldId>,
    record_value_types: &'a [CheckedRecordValueType],
    enum_types: &'a [CheckedEnumType],
    standard_field_types: HashMap<(CheckedTypeId, CheckedFieldId), TypeId>,
}

impl MutationCatalogue<CheckedTypeId, CheckedFieldId> for RecordAwareMutationCatalogue<'_> {
    fn object_type_id_by_name(&self, name: &QualifiedSemanticName) -> Option<CheckedTypeId> {
        QueryCatalogue::object_type_id_by_name(self.objects, name)
    }

    fn field_by_name(
        &self,
        owner: CheckedTypeId,
        name: &str,
    ) -> Option<MutationField<CheckedTypeId, CheckedFieldId>> {
        MutationCatalogue::field_by_name(self.objects, owner, name)
    }

    fn visit_fields(
        &self,
        owner: CheckedTypeId,
        visitor: &mut dyn FnMut(&str, MutationField<CheckedTypeId, CheckedFieldId>),
    ) {
        MutationCatalogue::visit_fields(self.objects, owner, visitor);
    }

    fn record_type_id_by_name(&self, name: &QualifiedSemanticName) -> Option<CheckedTypeId> {
        self.record_value_types
            .iter()
            .find(|record| record.name() == name)
            .map(CheckedRecordValueType::id)
    }

    fn record_field_by_name(
        &self,
        owner: CheckedTypeId,
        name: &str,
    ) -> Option<MutationField<CheckedTypeId, CheckedFieldId>> {
        self.record_value_types
            .iter()
            .find(|record| record.id() == owner)
            .and_then(|record| record.fields().iter().find(|field| field.name() == name))
            .map(|field| self.record_field(owner, field))
    }

    fn visit_record_fields(
        &self,
        owner: CheckedTypeId,
        visitor: &mut dyn FnMut(&str, MutationField<CheckedTypeId, CheckedFieldId>),
    ) {
        let Some(record) = self
            .record_value_types
            .iter()
            .find(|record| record.id() == owner)
        else {
            return;
        };
        for field in record.fields() {
            visitor(field.name(), self.record_field(owner, field));
        }
    }

    fn named_type_is_enum(&self, id: CheckedTypeId) -> bool {
        self.enum_types.iter().any(|enum_type| enum_type.id == id)
    }
}

impl RecordAwareMutationCatalogue<'_> {
    fn record_field(
        &self,
        owner: CheckedTypeId,
        field: &CheckedRecordValueField,
    ) -> MutationField<CheckedTypeId, CheckedFieldId> {
        let result = MutationField::new(field.id(), field.semantic_type(), false);
        self.standard_field_types
            .get(&(owner, field.id()))
            .copied()
            .map_or(result, |type_id| result.with_standard_value_type(type_id))
    }
}

fn checked_query_catalogue(
    object_types: &[CheckedObjectType],
    uses: &[CheckedApplicationTypeUse],
) -> ResolutionCatalogue<CheckedTypeId, CheckedFieldId> {
    let standard_field_types = uses
        .iter()
        .filter_map(|type_use| {
            let CheckedTypeUseKind::Field { owner, field } = type_use.kind() else {
                return None;
            };
            type_use
                .value()
                .map(|value| ((owner, field), value.type_id()))
        })
        .collect::<HashMap<_, _>>();
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
                            let query_field =
                                QueryField::new(field.id, field.semantic_type, field.nullable);
                            let query_field = if field.unique {
                                query_field.with_unique()
                            } else {
                                query_field
                            };
                            let query_field = standard_field_types
                                .get(&(object_type.id, field.id))
                                .copied()
                                .map_or(query_field, |type_id| {
                                    query_field.with_standard_value_type(type_id)
                                });
                            (field.name.clone(), query_field)
                        })
                        .collect(),
                )
            })
            .collect(),
    )
    .expect("checked definitions satisfy resolver-local query catalogue invariants")
}

fn parameter_references(
    parameters: &[ResolvedServerFunctionParameter],
) -> Vec<CheckedDefinitionReference> {
    parameters
        .iter()
        .filter_map(|parameter| {
            object_reference(
                parameter.semantic_type,
                parameter.reference_location.as_ref(),
            )
        })
        .collect()
}

fn signature_references(
    parameters: &[ResolvedServerFunctionParameter],
    return_type: &ResolvedServerFunctionReturn,
) -> Vec<CheckedDefinitionReference> {
    let mut references = parameter_references(parameters);
    match return_type {
        ResolvedServerFunctionReturn::Rows { columns, .. } => {
            references.extend(columns.iter().filter_map(|column| {
                object_reference(column.semantic_type, column.reference_location.as_ref())
            }));
        }
        ResolvedServerFunctionReturn::Stream {
            semantic_type,
            reference_location,
            ..
        } => {
            if let Some(reference) = object_reference(*semantic_type, reference_location.as_ref()) {
                references.push(reference);
            }
        }
        ResolvedServerFunctionReturn::Single { .. } => {}
    }
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

fn unique_text_selected_query_reference(
    reference: &UniqueTextSelectedQueryReference<
        CheckedTypeId,
        CheckedFieldId,
        CheckedFunctionId,
        CheckedParameterId,
    >,
) -> CheckedDefinitionReference {
    let (target, kind, location) = match reference {
        UniqueTextSelectedQueryReference::QueryObject {
            object_type,
            location,
        } => (
            CheckedDefinitionReferenceTarget::ObjectType(*object_type),
            DefinitionReferenceKind::QueryObject,
            location,
        ),
        UniqueTextSelectedQueryReference::ObjectReference {
            object_type,
            location,
        } => (
            CheckedDefinitionReferenceTarget::ObjectType(*object_type),
            DefinitionReferenceKind::ObjectReference,
            location,
        ),
        UniqueTextSelectedQueryReference::QueryField {
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
        UniqueTextSelectedQueryReference::ParameterRead {
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
        MutationReference::NamedValueType { value_type, .. } => (
            CheckedDefinitionReferenceTarget::ValueType(*value_type),
            DefinitionReferenceKind::NamedType,
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
    Some(location(logical_path, target.span()))
}

fn type_use_location(specification: &TypeSpecification, logical_path: &str) -> SourceLocation {
    match specification {
        TypeSpecification::Reference { target, .. } => location(logical_path, target.span()),
        TypeSpecification::Named(_) | TypeSpecification::StandardLargeObject { .. } => {
            location(logical_path, specification.span())
        }
        TypeSpecification::List { .. }
        | TypeSpecification::Set { .. }
        | TypeSpecification::Map { .. }
        | TypeSpecification::Option { .. }
        | TypeSpecification::Stream { .. } => location(logical_path, specification.span()),
    }
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

fn application_failed(
    parse_report: ParseReport,
    mut diagnostics: Vec<CompilerDiagnostic>,
) -> ApplicationCheckResult {
    if parse_report.diagnostics().is_empty() {
        sort_application_diagnostics(&mut diagnostics, &parse_report);
    }
    ApplicationCheckResult {
        parse_report,
        diagnostics,
        checked_bundle: None,
        uses: Vec::new(),
    }
}

fn sort_application_diagnostics(
    diagnostics: &mut [CompilerDiagnostic],
    parse_report: &ParseReport,
) {
    let unit_indices = source_unit_indices(parse_report);
    diagnostics.sort_by(|left, right| {
        let left_location = left.location();
        let right_location = right.location();
        (
            unit_indices
                .get(left_location.logical_path())
                .copied()
                .unwrap_or(usize::MAX),
            left_location.logical_path(),
            left_location.span().start(),
            left_location.span().end(),
            left.code().as_str(),
            left.message(),
        )
            .cmp(&(
                unit_indices
                    .get(right_location.logical_path())
                    .copied()
                    .unwrap_or(usize::MAX),
                right_location.logical_path(),
                right_location.span().start(),
                right_location.span().end(),
                right.code().as_str(),
                right.message(),
            ))
    });
}

fn sort_standard_type_uses(uses: &mut [CheckedApplicationTypeUse], parse_report: &ParseReport) {
    let unit_indices = source_unit_indices(parse_report);
    uses.sort_by_key(|type_use| {
        let location = type_use.location();
        (
            unit_indices
                .get(location.logical_path())
                .copied()
                .unwrap_or(usize::MAX),
            location.span().start(),
            location.span().end(),
            type_use_kind_tag(type_use.kind()),
            type_use_tie_break(type_use.kind()),
        )
    });
}

fn source_unit_indices(parse_report: &ParseReport) -> HashMap<&str, usize> {
    parse_report
        .units()
        .iter()
        .enumerate()
        .map(|(index, unit)| (unit.logical_path(), index))
        .collect()
}

struct StandardFunctionReferenceMetadata {
    source_unit_index: usize,
    declaration_start: usize,
    parameter_ordinals: HashMap<CheckedParameterId, u32>,
    return_offset: u32,
}

fn collect_standard_type_references(
    uses: &[CheckedApplicationTypeUse],
    checked_bundle: &CheckedBundle,
    parse_report: &ParseReport,
) -> Vec<CheckedStandardTypeReference> {
    let source_unit_indices = source_unit_indices(parse_report);
    let mut functions = HashMap::new();

    for function in &checked_bundle.server_functions {
        functions.insert(
            function.id,
            StandardFunctionReferenceMetadata {
                source_unit_index: source_unit_indices
                    .get(function.location.logical_path())
                    .copied()
                    .unwrap_or(usize::MAX),
                declaration_start: function.location.span().start(),
                parameter_ordinals: function
                    .parameters
                    .iter()
                    .map(|parameter| (parameter.id, parameter.ordinal))
                    .collect(),
                return_offset: function.parameters.len() as u32,
            },
        );
    }
    for function in &checked_bundle.client_functions {
        functions.insert(
            function.id,
            StandardFunctionReferenceMetadata {
                source_unit_index: source_unit_indices
                    .get(function.location.logical_path())
                    .copied()
                    .unwrap_or(usize::MAX),
                declaration_start: function.location.span().start(),
                parameter_ordinals: function
                    .parameters
                    .iter()
                    .map(|parameter| (parameter.id, parameter.ordinal))
                    .collect(),
                return_offset: function.parameters.len() as u32,
            },
        );
    }
    let mut references = uses
        .iter()
        .filter_map(|type_use| {
            let value = type_use.value()?;
            let (owner, ordinal) = match value.kind() {
                CheckedTypeUseKind::Parameter { owner, parameter } => {
                    let function = functions.get(&owner)?;
                    (owner, *function.parameter_ordinals.get(&parameter)?)
                }
                CheckedTypeUseKind::Return { owner, ordinal } => {
                    let function = functions.get(&owner)?;
                    (owner, function.return_offset.checked_add(ordinal)?)
                }
                CheckedTypeUseKind::Field { .. }
                | CheckedTypeUseKind::State { .. }
                | CheckedTypeUseKind::Expression { .. }
                | CheckedTypeUseKind::Result { .. } => return None,
            };
            let function = functions.get(&owner)?;
            Some((
                function.source_unit_index,
                function.declaration_start,
                ordinal,
                CheckedStandardTypeReference {
                    owner,
                    ordinal,
                    target: value.type_id(),
                    location: value.location().clone(),
                },
            ))
        })
        .collect::<Vec<_>>();
    references.sort_by_key(|(source_unit_index, declaration_start, ordinal, _)| {
        (*source_unit_index, *declaration_start, *ordinal)
    });
    references
        .into_iter()
        .map(|(_, _, _, reference)| reference)
        .collect()
}

const fn type_use_kind_tag(kind: CheckedTypeUseKind) -> u8 {
    match kind {
        CheckedTypeUseKind::Field { .. } => 0,
        CheckedTypeUseKind::Parameter { .. } => 1,
        CheckedTypeUseKind::State { .. } => 2,
        CheckedTypeUseKind::Return { .. } => 3,
        CheckedTypeUseKind::Expression { .. } => 4,
        CheckedTypeUseKind::Result { .. } => 5,
    }
}

#[derive(Eq, Ord, PartialEq, PartialOrd)]
enum TypeUseTieBreak {
    Field(CheckedTypeId, CheckedFieldId),
    Parameter(CheckedFunctionId, CheckedParameterId),
    State(u32, CheckedFunctionId),
    Return(u32, CheckedFunctionId),
    Expression(u32, CheckedFunctionId),
    Result(u32, CheckedFunctionId),
}

const fn type_use_tie_break(kind: CheckedTypeUseKind) -> TypeUseTieBreak {
    match kind {
        CheckedTypeUseKind::Field { owner, field } => TypeUseTieBreak::Field(owner, field),
        CheckedTypeUseKind::Parameter { owner, parameter } => {
            TypeUseTieBreak::Parameter(owner, parameter)
        }
        CheckedTypeUseKind::State { owner, ordinal } => TypeUseTieBreak::State(ordinal, owner),
        CheckedTypeUseKind::Return { owner, ordinal } => TypeUseTieBreak::Return(ordinal, owner),
        CheckedTypeUseKind::Expression { owner, ordinal } => {
            TypeUseTieBreak::Expression(ordinal, owner)
        }
        CheckedTypeUseKind::Result { owner, ordinal } => TypeUseTieBreak::Result(ordinal, owner),
    }
}

#[derive(Clone, Copy)]
struct ResolvedApplicationType {
    semantic_type: SemanticType<CheckedTypeId>,
    standard_value_type: Option<orna_core::TypeId>,
}

#[derive(Clone, Copy)]
enum SubmittedType {
    Object(CheckedTypeId),
    Enum(CheckedTypeId),
    RecordValue(CheckedTypeId),
}

fn sealed_system_type_id(name: &QualifiedSemanticName) -> Option<TypeId> {
    match name.to_string().as_str() {
        orna_core::system::SYS_SOURCE_FUNCTION_TYPE_NAME => {
            Some(orna_core::system::SYS_SOURCE_FUNCTION_TYPE_ID)
        }
        orna_core::system::SYS_INSPECT_INVOCATION_TYPE_NAME => {
            Some(orna_core::system::SYS_INSPECT_INVOCATION_TYPE_ID)
        }
        orna_core::system::SYS_INSPECT_SNAPSHOT_TYPE_NAME => {
            Some(orna_core::system::SYS_INSPECT_SNAPSHOT_TYPE_ID)
        }
        orna_core::system::SYS_INSPECT_SNAPSHOT_OPTIONS_TYPE_NAME => {
            Some(orna_core::system::SYS_INSPECT_SNAPSHOT_OPTIONS_TYPE_ID)
        }
        orna_core::system::SYS_INSPECT_TRACE_EVENT_TYPE_NAME => {
            Some(orna_core::system::SYS_INSPECT_TRACE_EVENT_TYPE_ID)
        }
        orna_core::system::SYS_INSPECT_INVOCATION_NODES_TYPE_NAME => {
            Some(orna_core::system::SYS_INSPECT_INVOCATION_NODES_TYPE_ID)
        }
        orna_core::system::SYS_INSPECT_CALLS_TYPE_NAME => {
            Some(orna_core::system::SYS_INSPECT_CALLS_TYPE_ID)
        }
        orna_core::system::SYS_INSPECT_RESOURCES_TYPE_NAME => {
            Some(orna_core::system::SYS_INSPECT_RESOURCES_TYPE_ID)
        }
        orna_core::system::SYS_INSPECT_STATE_CELLS_TYPE_NAME => {
            Some(orna_core::system::SYS_INSPECT_STATE_CELLS_TYPE_ID)
        }
        orna_core::system::SYS_INSPECT_UI_NODES_TYPE_NAME => {
            Some(orna_core::system::SYS_INSPECT_UI_NODES_TYPE_ID)
        }
        orna_core::system::SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_NAME => {
            Some(orna_core::system::SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID)
        }
        orna_core::system::SYS_INSPECT_RUNTIME_BINDINGS_TYPE_NAME => {
            Some(orna_core::system::SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID)
        }
        orna_core::system::SYS_INSPECT_SECURITY_DECISIONS_TYPE_NAME => {
            Some(orna_core::system::SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID)
        }
        _ => None,
    }
}

fn sealed_inspect_type_id(name: &QualifiedSemanticName) -> Option<TypeId> {
    sealed_system_type_id(name).filter(|type_id| *type_id != SYS_SOURCE_FUNCTION_TYPE_ID)
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

fn resolve_application_type(
    specification: &TypeSpecification,
    submitted_ids: &HashMap<QualifiedSemanticName, SubmittedType>,
    logical_path: &str,
    diagnostics: &mut Vec<CompilerDiagnostic>,
    standard: Option<&CheckedStandardLibrary>,
) -> Option<ResolvedApplicationType> {
    resolve_application_type_with_named_standard(
        specification,
        submitted_ids,
        logical_path,
        diagnostics,
        standard,
        false,
    )
}

fn resolve_application_type_with_named_standard(
    specification: &TypeSpecification,
    submitted_ids: &HashMap<QualifiedSemanticName, SubmittedType>,
    logical_path: &str,
    diagnostics: &mut Vec<CompilerDiagnostic>,
    standard: Option<&CheckedStandardLibrary>,
    allow_standard_named: bool,
) -> Option<ResolvedApplicationType> {
    match specification {
        TypeSpecification::Named(name) => {
            if allow_standard_named
                && let Some(type_id) = sealed_system_type_id(&semantic_name(name))
            {
                return Some(ResolvedApplicationType {
                    semantic_type: SemanticType::Named(CheckedTypeId::Existing(type_id)),
                    standard_value_type: None,
                });
            }
            if allow_standard_named
                && let Some(type_id) = sealed_inspect_type_id(&semantic_name(name))
            {
                return Some(ResolvedApplicationType {
                    semantic_type: SemanticType::Named(CheckedTypeId::Existing(type_id)),
                    standard_value_type: None,
                });
            }
            let value_type = standard.map_or_else(
                || resolve_closed_scalar(name).map(|scalar| (None, scalar)),
                |standard| {
                    standard_value_by_name(name, standard)
                        .map(|(type_id, scalar)| (Some(type_id), scalar))
                },
            );
            if let Some((standard_value_type, scalar)) = value_type {
                return Some(ResolvedApplicationType {
                    semantic_type: SemanticType::scalar(scalar),
                    standard_value_type,
                });
            }
            if allow_standard_named
                && let Some(standard) = standard
                && let Some(type_id) = standard_type_id_by_name(name, standard)
            {
                return Some(ResolvedApplicationType {
                    semantic_type: SemanticType::Named(CheckedTypeId::Existing(type_id)),
                    standard_value_type: None,
                });
            }
            let semantic_name = semantic_name(name);
            match submitted_ids.get(&semantic_name).copied() {
                Some(SubmittedType::Enum(id)) => {
                    return Some(ResolvedApplicationType {
                        semantic_type: SemanticType::Named(id),
                        standard_value_type: None,
                    });
                }
                Some(SubmittedType::Object(_)) => diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    format!("object type {semantic_name} must be declared with REF"),
                    logical_path,
                    &name.span,
                )),
                Some(SubmittedType::RecordValue(id)) => {
                    return Some(ResolvedApplicationType {
                        semantic_type: SemanticType::Named(id),
                        standard_value_type: None,
                    });
                }
                None => diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    format!("unknown type name {semantic_name}"),
                    logical_path,
                    &name.span,
                )),
            }
            None
        }
        TypeSpecification::StandardLargeObject { kind, source } => {
            let value_type = standard.map_or_else(
                || {
                    let scalar = match kind {
                        StandardLargeObjectKind::Character => StandardScalar::CharacterLargeObject,
                        StandardLargeObjectKind::Binary => StandardScalar::BinaryLargeObject,
                    };
                    Some((None, scalar))
                },
                |standard| {
                    standard_large_object_value(*kind, standard)
                        .map(|(type_id, scalar)| (Some(type_id), scalar))
                },
            );
            let Some((standard_value_type, scalar)) = value_type else {
                diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    format!("unknown type name {}", source.text),
                    logical_path,
                    &source.span,
                ));
                return None;
            };
            Some(ResolvedApplicationType {
                semantic_type: SemanticType::scalar(scalar),
                standard_value_type,
            })
        }
        TypeSpecification::Reference { target, .. } => {
            let TypeSpecification::Named(target) = target.as_ref() else {
                diagnostics.push(diagnostic(
                    DiagnosticCode::InvalidReferenceTarget,
                    "REF target must be one named object type",
                    logical_path,
                    target.span(),
                ));
                return None;
            };
            let target_name = semantic_name(target);
            if allow_standard_named && let Some(type_id) = sealed_inspect_type_id(&target_name) {
                if type_id == orna_core::system::SYS_INSPECT_INVOCATION_TYPE_ID {
                    return Some(ResolvedApplicationType {
                        semantic_type: SemanticType::Reference {
                            target: CheckedTypeId::Existing(type_id),
                        },
                        standard_value_type: None,
                    });
                }
                diagnostics.push(diagnostic(
                    DiagnosticCode::InvalidReferenceTarget,
                    format!("REF target {target_name} is a sealed INSPECT carrier"),
                    logical_path,
                    &target.span,
                ));
                return None;
            }
            let scalar_target = standard.map_or_else(
                || resolve_closed_scalar(target).is_some(),
                |standard| standard_value_by_name(target, standard).is_some(),
            );
            if scalar_target {
                diagnostics.push(diagnostic(
                    DiagnosticCode::InvalidReferenceTarget,
                    format!("REF target {} is a scalar type", semantic_name(target)),
                    logical_path,
                    &target.span,
                ));
                return None;
            }
            let name = semantic_name(target);
            match submitted_ids.get(&name).copied() {
                Some(SubmittedType::Object(id)) => Some(ResolvedApplicationType {
                    semantic_type: SemanticType::reference(id),
                    standard_value_type: None,
                }),
                Some(SubmittedType::Enum(_)) => {
                    diagnostics.push(diagnostic(
                        DiagnosticCode::InvalidReferenceTarget,
                        format!("REF target {name} is an enum type"),
                        logical_path,
                        &target.span,
                    ));
                    None
                }
                Some(SubmittedType::RecordValue(_)) => {
                    diagnostics.push(diagnostic(
                        DiagnosticCode::InvalidReferenceTarget,
                        format!("REF target {name} is a record value type"),
                        logical_path,
                        &target.span,
                    ));
                    None
                }
                None => {
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
        TypeSpecification::List { .. }
        | TypeSpecification::Set { .. }
        | TypeSpecification::Map { .. }
        | TypeSpecification::Option { .. }
        | TypeSpecification::Stream { .. } => {
            diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                "constructed types are not admitted in this position",
                logical_path,
                specification.span(),
            ));
            None
        }
    }
}

fn resolve_record_value_field_type(
    specification: &TypeSpecification,
    submitted_ids: &HashMap<QualifiedSemanticName, SubmittedType>,
    logical_path: &str,
    diagnostics: &mut Vec<CompilerDiagnostic>,
    standard: &CheckedStandardLibrary,
) -> Option<ResolvedApplicationType> {
    let resolved = resolve_application_type(
        specification,
        submitted_ids,
        logical_path,
        diagnostics,
        Some(standard),
    )?;
    let supported = match (resolved.semantic_type, resolved.standard_value_type) {
        (SemanticType::Named(_), None) => true,
        (SemanticType::Scalar(scalar), Some(type_id)) => standard
            .value_types()
            .iter()
            .find(|value_type| value_type.id() == type_id)
            .is_some_and(|value_type| {
                value_type.kind() == ValueTypeKind::Primitive
                    && value_type.mutability() == ValueTypeMutability::Immutable
                    && value_type.persistence() == ValueTypePersistence::Persistable
                    && supports_record_value_scalar(scalar)
            }),
        (SemanticType::Scalar(_), None)
        | (SemanticType::Named(_), Some(_))
        | (SemanticType::Reference { .. }, _) => false,
    };
    if supported {
        return Some(resolved);
    }
    diagnostics.push(diagnostic(
        DiagnosticCode::TypeMismatch,
        "record value field uses a type outside the initial record family",
        logical_path,
        specification.span(),
    ));
    None
}

struct RecordValueFieldGraphEdge<'a> {
    target: usize,
    logical_path: &'a str,
    span: &'a SourceSpan,
}

struct RecordValueFieldGraphNode<'a> {
    name: &'a QualifiedSemanticName,
    edges: Vec<RecordValueFieldGraphEdge<'a>>,
}

fn validate_record_value_field_graph(
    headers: &[RecordValueHeader<'_>],
    record_value_types: &[CheckedRecordValueType],
    diagnostics: &mut Vec<CompilerDiagnostic>,
) {
    debug_assert_eq!(headers.len(), record_value_types.len());
    let indices = record_value_types
        .iter()
        .enumerate()
        .map(|(index, record_value_type)| (record_value_type.id(), index))
        .collect::<HashMap<_, _>>();
    let mut nodes = Vec::with_capacity(record_value_types.len());
    for (header, record_value_type) in headers.iter().zip(record_value_types) {
        let mut edges = Vec::new();
        for (declaration, checked_field) in header
            .declaration
            .fields
            .iter()
            .zip(record_value_type.fields())
        {
            let SemanticType::Named(target) = checked_field.semantic_type() else {
                continue;
            };
            let Some(target) = indices.get(&target).copied() else {
                continue;
            };
            edges.push(RecordValueFieldGraphEdge {
                target,
                logical_path: header.logical_path,
                span: declaration.type_specification.span(),
            });
        }
        nodes.push(RecordValueFieldGraphNode {
            name: record_value_type.name(),
            edges,
        });
    }

    let mut colours = vec![0_u8; nodes.len()];
    for root in 0..nodes.len() {
        if colours[root] != 0 {
            continue;
        }
        colours[root] = 1;
        let mut stack = vec![(root, 0_usize)];
        while let Some((node, edge_index)) = stack.pop() {
            if edge_index == nodes[node].edges.len() {
                colours[node] = 2;
                continue;
            }
            let edge = &nodes[node].edges[edge_index];
            stack.push((node, edge_index + 1));
            match colours[edge.target] {
                0 => {
                    colours[edge.target] = 1;
                    stack.push((edge.target, 0));
                }
                1 => {
                    diagnostics.push(diagnostic(
                        DiagnosticCode::TypeMismatch,
                        format!(
                            "record value fields must not form a recursive cycle through {}",
                            nodes[edge.target].name
                        ),
                        edge.logical_path,
                        edge.span,
                    ));
                    return;
                }
                2 => {}
                _ => unreachable!("record graph colour is valid"),
            }
        }
    }

    let mut greatest_fully_explored_depth = vec![None::<usize>; nodes.len()];
    for root in 0..nodes.len() {
        let mut stack = vec![(root, 0_usize, 0_usize)];
        while let Some((node, edge_index, depth)) = stack.pop() {
            if greatest_fully_explored_depth[node].is_some_and(|cached| cached >= depth) {
                continue;
            }
            if edge_index == nodes[node].edges.len() {
                greatest_fully_explored_depth[node] = Some(
                    greatest_fully_explored_depth[node].map_or(depth, |cached| cached.max(depth)),
                );
                continue;
            }
            let edge = &nodes[node].edges[edge_index];
            stack.push((node, edge_index + 1, depth));
            let next_depth = depth + 1;
            if next_depth == 33 {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    format!(
                        "record value nesting exceeds 32 levels through {}",
                        nodes[edge.target].name
                    ),
                    edge.logical_path,
                    edge.span,
                ));
                return;
            }
            stack.push((edge.target, 0, next_depth));
        }
    }
}

const fn supports_record_value_scalar(scalar: StandardScalar) -> bool {
    matches!(
        scalar,
        StandardScalar::Boolean
            | StandardScalar::Integer
            | StandardScalar::BigInt
            | StandardScalar::Float
            | StandardScalar::CharacterLargeObject
            | StandardScalar::BinaryLargeObject
    )
}

fn standard_type_id_by_name(
    name: &QualifiedName,
    standard: &CheckedStandardLibrary,
) -> Option<orna_core::TypeId> {
    let lookup = if name.parts.len() == 1 && !name.parts[0].text.starts_with('"') {
        PreludeTypeName::new([semantic_part(&name.parts[0])])
            .ok()
            .map(TypeLookupName::prelude)?
    } else {
        TypeLookupName::qualified(semantic_name(name))
    };
    standard
        .verified_snapshot()
        .catalogue()
        .type_id_by_name(&lookup)
}

fn standard_value_by_name(
    name: &QualifiedName,
    standard: &CheckedStandardLibrary,
) -> Option<(orna_core::TypeId, StandardScalar)> {
    let lookup = if name.parts.len() == 1 && !name.parts[0].text.starts_with('"') {
        PreludeTypeName::new([semantic_part(&name.parts[0])])
            .ok()
            .map(TypeLookupName::prelude)?
    } else {
        TypeLookupName::qualified(semantic_name(name))
    };
    standard_value_by_lookup(&lookup, standard)
}

fn standard_large_object_value(
    kind: StandardLargeObjectKind,
    standard: &CheckedStandardLibrary,
) -> Option<(orna_core::TypeId, StandardScalar)> {
    let words = match kind {
        StandardLargeObjectKind::Character => ["character", "large", "object"],
        StandardLargeObjectKind::Binary => ["binary", "large", "object"],
    };
    let lookup = PreludeTypeName::new(words)
        .ok()
        .map(TypeLookupName::prelude)?;
    standard_value_by_lookup(&lookup, standard)
}

fn standard_value_by_lookup(
    lookup: &TypeLookupName,
    standard: &CheckedStandardLibrary,
) -> Option<(orna_core::TypeId, StandardScalar)> {
    let direct = standard
        .verified_snapshot()
        .catalogue()
        .type_id_by_name(lookup)?;
    let value_type = standard
        .value_types()
        .iter()
        .find(|value_type| value_type.id() == direct)?;
    if value_type.kind() != ValueTypeKind::Primitive {
        return None;
    }
    compatibility_scalar(value_type.representation_contract()).map(|scalar| (direct, scalar))
}

fn intrinsic_boolean_type(standard: Option<&CheckedStandardLibrary>) -> IntrinsicBooleanType {
    let Some(standard) = standard else {
        return IntrinsicBooleanType::Legacy;
    };
    standard
        .value_types()
        .iter()
        .find(|value_type| {
            value_type.kind() == ValueTypeKind::Primitive
                && value_type.representation_contract() == "orna.kernel.value.boolean@1"
        })
        .map_or(IntrinsicBooleanType::Missing, |value_type| {
            IntrinsicBooleanType::Standard(value_type.id())
        })
}

const fn intrinsic_boolean_id(
    intrinsic_boolean: IntrinsicBooleanType,
) -> Option<orna_core::TypeId> {
    match intrinsic_boolean {
        IntrinsicBooleanType::Standard(type_id) => Some(type_id),
        IntrinsicBooleanType::Legacy | IntrinsicBooleanType::Missing => None,
    }
}

pub(crate) fn supports_unique_text_or_required_reference(
    semantic_type: SemanticType<CheckedTypeId>,
    nullable: bool,
) -> bool {
    matches!(
        semantic_type,
        SemanticType::Scalar(StandardScalar::CharacterLargeObject)
    ) || (!nullable && matches!(semantic_type, SemanticType::Reference { .. }))
}

pub(crate) const UNIQUE_FIELD_MESSAGE: &str =
    "UNIQUE is only available for TEXT fields or REF fields that are NOT NULL";

fn resolve_closed_scalar(name: &QualifiedName) -> Option<StandardScalar> {
    if name.parts.len() != 1 || name.parts[0].text.starts_with('"') {
        return None;
    }

    let spelling = &name.parts[0].text;
    if spelling.eq_ignore_ascii_case("BOOLEAN") || spelling.eq_ignore_ascii_case("BOOL") {
        Some(StandardScalar::Boolean)
    } else if spelling.eq_ignore_ascii_case("INTEGER") || spelling.eq_ignore_ascii_case("INT") {
        Some(StandardScalar::Integer)
    } else if spelling.eq_ignore_ascii_case("BIGINT") {
        Some(StandardScalar::BigInt)
    } else if spelling.eq_ignore_ascii_case("FLOAT") {
        Some(StandardScalar::Float)
    } else if spelling.eq_ignore_ascii_case("DECIMAL") {
        Some(StandardScalar::Decimal)
    } else if spelling.eq_ignore_ascii_case("TEXT") {
        Some(StandardScalar::CharacterLargeObject)
    } else if spelling.eq_ignore_ascii_case("BYTES") {
        Some(StandardScalar::BinaryLargeObject)
    } else if spelling.eq_ignore_ascii_case("UUID") {
        Some(StandardScalar::Uuid)
    } else if spelling.eq_ignore_ascii_case("DATE") {
        Some(StandardScalar::Date)
    } else if spelling.eq_ignore_ascii_case("TIME") {
        Some(StandardScalar::Time)
    } else if spelling.eq_ignore_ascii_case("TIMESTAMP") {
        Some(StandardScalar::Timestamp)
    } else if spelling.eq_ignore_ascii_case("DURATION") {
        Some(StandardScalar::Duration)
    } else if spelling.eq_ignore_ascii_case("VOID") {
        Some(StandardScalar::Void)
    } else {
        None
    }
}

fn compatibility_scalar(contract: &str) -> Option<StandardScalar> {
    match contract {
        "orna.kernel.value.boolean@1" => Some(StandardScalar::Boolean),
        "orna.kernel.value.integer@1" => Some(StandardScalar::Integer),
        "orna.kernel.value.bigint@1" => Some(StandardScalar::BigInt),
        "orna.kernel.value.float@1" => Some(StandardScalar::Float),
        "orna.kernel.value.decimal@1" => Some(StandardScalar::Decimal),
        "orna.kernel.value.character-large-object@1" => Some(StandardScalar::CharacterLargeObject),
        "orna.kernel.value.binary-large-object@1" => Some(StandardScalar::BinaryLargeObject),
        "orna.kernel.value.uuid@1" => Some(StandardScalar::Uuid),
        "orna.kernel.value.date@1" => Some(StandardScalar::Date),
        "orna.kernel.value.time@1" => Some(StandardScalar::Time),
        "orna.kernel.value.timestamp@1" => Some(StandardScalar::Timestamp),
        "orna.kernel.value.duration@1" => Some(StandardScalar::Duration),
        "orna.kernel.value.void@1" => Some(StandardScalar::Void),
        _ => None,
    }
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
mod tests;
