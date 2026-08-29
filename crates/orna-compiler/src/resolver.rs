//! Semantic resolution for parsed source bundles.
//!
//! The resolver consumes the `Parse` values retained by [`super::parse_bundle`].
//! It does not parse source text or expose syntax implementation values.

mod identity;
mod model;
mod type_use;

pub use identity::{
    CheckedExpressionId, CheckedFieldId, CheckedFunctionId, CheckedParameterId, CheckedSchemaId,
    CheckedTypeId, ProvisionalExpressionId, ProvisionalFieldId,
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
        false,
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

/// Checks source authored inside the protected standard namespace.
///
/// This entry point is reserved for the standard-library builder. Ordinary
/// application checks continue to reject declarations under `std`.
pub fn check_standard_source(
    bundle: &SourceBundle,
    base: &CatalogueSnapshot,
    standard: &CheckedStandardLibrary,
) -> StandardApplicationCheckReport {
    let mut result = check_application_parsed(parse_bundle(bundle), base, Some(standard), true);
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
    let snapshot = standard.verified_snapshot();
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
        standard_library: standard.clone(),
        parse_report,
        diagnostics,
        checked_bundle,
    }
}


/// Checks retained standard source against its verified catalogue and origins.
///
/// Version 1 keeps the original one-unit, type-only reconcile contract. The
/// V2 contract (`StandardLibraryDigestVersion::Version2`) additionally
/// reconciles the ordered two-unit bundle (`std/types.orna` then
/// `std/invoke.orna`), the fixed identities, the exact `std.invoke.echo`
/// executable (artifact, semantic digest, and three durable references), and
/// every schema, function, and parameter origin against the retained units.
/// The V3 standard revision (ADR 0058) reuses the V2 digest contract but
/// carries the ordered three-unit bundle (`std/types.orna`,
/// `std/invoke.orna`, then `std/output.orna`); its branch reconciles the first
/// two units exactly as V2 does and additionally reconciles the output unit
/// closed against the `std.terminal` and `std.io` schemas, the two opaque
/// output value types, their exports, and every origin on the retained unit.
/// The V4 standard revision (ADR 0062) carries the ordered four-unit bundle
/// (`std/types.orna`, `std/invoke.orna`, `std/output.orna`, then `std/ui.orna`);
/// its branch reconciles the first
/// three units exactly as V3 does and additionally reconciles the ui unit,
/// opaque `std.ui.ui` value type, its export, and every origin on the retained
/// unit. The V5 standard revision (ADR 0075) retains those four units and adds
/// `std/json.orna`; its explicit branch reconciles the JSON schema, opaque value
/// type, export, and existing `std.json.encode` presenter against the installed
/// catalogue. The checker does not trust a source file because its path looks
/// standard.
pub fn check_standard_library_source(
    snapshot: &VerifiedStandardLibrarySnapshot,
) -> Result<CheckedStandardLibrary, StandardLibraryCheckError> {
    match snapshot.digest_version() {
        StandardLibraryDigestVersion::Version1 => check_standard_library_source_v1(snapshot),
        StandardLibraryDigestVersion::Version2 => match snapshot.revision() {
            STANDARD_LIBRARY_V10_REVISION_ID => check_standard_library_source_v10(snapshot),
            STANDARD_LIBRARY_V9_REVISION_ID => check_standard_library_source_v9(snapshot),
            STANDARD_LIBRARY_V8_REVISION_ID => check_standard_library_source_v8(snapshot),
            STANDARD_LIBRARY_V7_REVISION_ID => check_standard_library_source_v7(snapshot),
            STANDARD_LIBRARY_V6_REVISION_ID => check_standard_library_source_v6(snapshot),
            STANDARD_LIBRARY_V5_REVISION_ID => check_standard_library_source_v5(snapshot),
            STANDARD_LIBRARY_V4_REVISION_ID => check_standard_library_source_v4(snapshot),
            STANDARD_LIBRARY_V3_REVISION_ID => check_standard_library_source_v3(snapshot),
            _ => check_standard_library_source_v2(snapshot),
        },
        _ => Err(StandardLibraryCheckError::SourceMismatch),
    }
}

const STANDARD_SOURCE_UNIT_ID: SourceUnitId =
    SourceUnitId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);

fn check_standard_library_source_v1_identity(
    stored_unit: &StoredSourceUnit,
) -> Result<(), StandardLibraryCheckError> {
    if stored_unit.id() != STANDARD_SOURCE_UNIT_ID
        || stored_unit.logical_path() != "std/types.orna"
        || stored_unit.ordinal() != 0
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    Ok(())
}

/// Checks one retained version-1 type-only standard source unit.
///
/// This is the original `orna.std/1` contract: exactly one source unit, no
/// functions, and the full schema/value-type/binding reconcile.
fn check_standard_library_source_v1(
    snapshot: &VerifiedStandardLibrarySnapshot,
) -> Result<CheckedStandardLibrary, StandardLibraryCheckError> {
    let source_units = snapshot.source().units();
    let [stored_unit] = source_units else {
        return Err(StandardLibraryCheckError::SourceUnitCount {
            actual: source_units.len(),
        });
    };
    check_standard_library_source_v1_identity(stored_unit)?;

    let bundle = SourceBundle::new([SourceUnit::new(
        stored_unit.logical_path(),
        stored_unit.content(),
    )])
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let report = parse_bundle(&bundle);
    if !report.diagnostics().is_empty() {
        return Err(StandardLibraryCheckError::Diagnostics {
            diagnostics: report.diagnostics().to_vec(),
        });
    }
    let parsed_unit = report
        .units()
        .first()
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let families = reconcile_standard_source(
        stored_unit,
        parsed_unit,
        snapshot.catalogue(),
        snapshot.origins(),
    )?;

    Ok(CheckedStandardLibrary {
        verified_snapshot: snapshot.clone(),
        schemas: families.schemas,
        value_types: families.value_types,
        type_bindings: families.type_bindings,
        checked_executables: Vec::new(),
    })
}

/// Checks one retained V2 executable standard source bundle.
///
/// The ordered bundle must be exactly `std/types.orna` (`...02`) followed by
/// `std/invoke.orna` (`...03`). The types unit reconciles exactly as V1 does.
/// The invoke unit must contain exactly the `std.invoke` schema declaration
/// and the `std.invoke.echo` server function; the function is checked closed
/// by [`check_standard_parameter_echo`], and every stored executable fact
/// (function and revision identities, revision number, semantic-hash
/// contract, declaration origin and content hash, semantic digest, language
/// version, artifact, and the three ordered references) plus every origin
/// must agree with the checked source facts or the snapshot fails closed.
fn check_standard_library_source_v2(
    snapshot: &VerifiedStandardLibrarySnapshot,
) -> Result<CheckedStandardLibrary, StandardLibraryCheckError> {
    let (families, checked_executable) = check_standard_library_source_v2_parts(
        snapshot.source(),
        snapshot.catalogue(),
        snapshot.origins(),
        snapshot.executables(),
    )?;
    Ok(CheckedStandardLibrary {
        verified_snapshot: snapshot.clone(),
        schemas: families.schemas,
        value_types: families.value_types,
        type_bindings: families.type_bindings,
        checked_executables: vec![checked_executable],
    })
}

/// Checks the retained V2 source bundle, catalogue, origins, and executable
/// evidence without a retained digest.
///
/// The digest gate is a separate, prior verification step
/// (`verify_standard_library_v2_snapshot`); this function reconciles the
/// source facts against the supplied stored facts and fails closed on any
/// disagreement. The checked executable facts fix the ADR 0055 language
/// version `orna.language/1`; a snapshot that retained any other label fails
/// the stored-executable cross-check.
fn check_standard_library_source_v2_parts(
    source: &StoredSourceRevision,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    executables: &[StandardExecutable],
) -> Result<(StandardSourceFamilies, CheckedStandardExecutable), StandardLibraryCheckError> {
    let source_units = source.units();
    let [types_unit, invoke_unit] = source_units else {
        return Err(StandardLibraryCheckError::SourceUnitCount {
            actual: source_units.len(),
        });
    };
    if types_unit.id() != STD_TYPES_SOURCE_UNIT_ID
        || types_unit.logical_path() != "std/types.orna"
        || types_unit.ordinal() != 0
        || invoke_unit.id() != STD_INVOKE_SOURCE_UNIT_ID
        || invoke_unit.logical_path() != "std/invoke.orna"
        || invoke_unit.ordinal() != 1
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let bundle = SourceBundle::new([
        SourceUnit::new(types_unit.logical_path(), types_unit.content()),
        SourceUnit::new(invoke_unit.logical_path(), invoke_unit.content()),
    ])
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let report = parse_bundle(&bundle);
    if !report.diagnostics().is_empty() {
        return Err(StandardLibraryCheckError::Diagnostics {
            diagnostics: report.diagnostics().to_vec(),
        });
    }
    let [parsed_types, parsed_invoke] = report.units() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };

    let (types_origins, invoke_origins) = partition_standard_origins(origins)?;
    let types_catalogue = standard_types_catalogue(catalogue)?;
    let families =
        reconcile_standard_source(types_unit, parsed_types, &types_catalogue, &types_origins)?;
    let checked_executable = reconcile_standard_invoke_executable(
        catalogue,
        &invoke_origins,
        executables,
        invoke_unit,
        parsed_invoke,
    )?;
    Ok((families, checked_executable))
}

/// Checks one retained V3 output standard source bundle.
///
/// The ordered bundle must be exactly `std/types.orna` (`...02`),
/// `std/invoke.orna` (`...03`), then `std/output.orna` (`...04`). Units zero
/// and one reconcile exactly as the V2 checker does, including the unchanged
/// `std.invoke.echo` executable. The output unit must declare exactly the
/// `std.terminal` (`...04`) and `std.io` (`...05`) schemas, the two opaque
/// value types `std.terminal.Document` (`...15`) and `std.io.ByteStream`
/// (`...16`) with their ADR 0058 kernel contracts and `IMMUTABLE TRANSIENT`
/// catalogue facts, and the two qualified exports (`std.Document`,
/// `std.ByteStream`); every origin must sit on the retained output unit at
/// the exact declaration byte ranges, and any extra, missing, or mismatched
/// declaration, identity, contract, binding, or origin fails closed.
fn check_standard_library_source_v3(
    snapshot: &VerifiedStandardLibrarySnapshot,
) -> Result<CheckedStandardLibrary, StandardLibraryCheckError> {
    let (families, checked_executable) = check_standard_library_source_v3_parts(
        snapshot.source(),
        snapshot.catalogue(),
        snapshot.origins(),
        snapshot.executables(),
    )?;
    Ok(CheckedStandardLibrary {
        verified_snapshot: snapshot.clone(),
        schemas: families.schemas,
        value_types: families.value_types,
        type_bindings: families.type_bindings,
        checked_executables: vec![checked_executable],
    })
}

/// Checks the retained V3 source bundle, catalogue, origins, and executable
/// evidence without a retained digest.
///
/// The digest gate is a separate, prior verification step
/// (`verify_standard_library_v3_snapshot`); this function reconciles the
/// source facts against the supplied stored facts and fails closed on any
/// disagreement, exactly as [`check_standard_library_source_v2_parts`] does
/// for the first two units, then reconciles the output unit.
fn check_standard_library_source_v3_parts(
    source: &StoredSourceRevision,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    executables: &[StandardExecutable],
) -> Result<(StandardSourceFamilies, CheckedStandardExecutable), StandardLibraryCheckError> {
    let source_units = source.units();
    let [types_unit, invoke_unit, output_unit] = source_units else {
        return Err(StandardLibraryCheckError::SourceUnitCount {
            actual: source_units.len(),
        });
    };
    if types_unit.id() != STD_TYPES_SOURCE_UNIT_ID
        || types_unit.logical_path() != "std/types.orna"
        || types_unit.ordinal() != 0
        || invoke_unit.id() != STD_INVOKE_SOURCE_UNIT_ID
        || invoke_unit.logical_path() != "std/invoke.orna"
        || invoke_unit.ordinal() != 1
        || output_unit.id() != STD_OUTPUT_SOURCE_UNIT_ID
        || output_unit.logical_path() != "std/output.orna"
        || output_unit.ordinal() != 2
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let bundle = SourceBundle::new([
        SourceUnit::new(types_unit.logical_path(), types_unit.content()),
        SourceUnit::new(invoke_unit.logical_path(), invoke_unit.content()),
        SourceUnit::new(output_unit.logical_path(), output_unit.content()),
    ])
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let report = parse_bundle(&bundle);
    if !report.diagnostics().is_empty() {
        return Err(StandardLibraryCheckError::Diagnostics {
            diagnostics: report.diagnostics().to_vec(),
        });
    }
    let [parsed_types, parsed_invoke, parsed_output] = report.units() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };

    let (types_origins, invoke_origins, output_origins) = partition_standard_v3_origins(origins)?;
    let types_catalogue = standard_v3_types_catalogue(catalogue)?;
    let families =
        reconcile_standard_source(types_unit, parsed_types, &types_catalogue, &types_origins)?;
    let checked_executable = reconcile_standard_invoke_executable(
        catalogue,
        &invoke_origins,
        executables,
        invoke_unit,
        parsed_invoke,
    )?;
    reconcile_standard_output_unit(output_unit, parsed_output, catalogue, &output_origins)?;
    Ok((families, checked_executable))
}

/// Checks one retained V4 UI standard source bundle.
///
/// The ordered bundle must be exactly `std/types.orna` (`...02`),
/// `std/invoke.orna` (`...03`), `std/output.orna` (`...04`), then
/// `std/ui.orna` (`...05`). Units zero to two reconcile exactly as the V3
/// checker does, including the unchanged `std.invoke.echo` executable. The
/// ui unit must declare exactly the `std.ui` (`...08`) schema, the single
/// opaque value type `std.ui.UI` (`...19`) with its ADR 0062 kernel contract
/// and `IMMUTABLE TRANSIENT` catalogue facts, and the single qualified
/// export (`std.UI`); every origin must sit on the retained ui unit at the
/// exact declaration byte ranges, and any extra, missing, or mismatched
/// declaration, identity, contract, binding, or origin fails closed.
fn check_standard_library_source_v4(
    snapshot: &VerifiedStandardLibrarySnapshot,
) -> Result<CheckedStandardLibrary, StandardLibraryCheckError> {
    let (families, checked_executable) = check_standard_library_source_v4_parts(
        snapshot.source(),
        snapshot.catalogue(),
        snapshot.origins(),
        snapshot.executables(),
    )?;
    Ok(CheckedStandardLibrary {
        verified_snapshot: snapshot.clone(),
        schemas: families.schemas,
        value_types: families.value_types,
        type_bindings: families.type_bindings,
        checked_executables: vec![checked_executable],
    })
}

/// Checks the retained V4 source bundle, catalogue, origins, and executable
/// evidence without a retained digest.
///
/// The digest gate is a separate, prior verification step
/// (`verify_standard_library_v4_snapshot`); this function reconciles the
/// source facts against the supplied stored facts and fails closed on any
/// disagreement, exactly as [`check_standard_library_source_v3_parts`] does
/// for the first three units, then reconciles the ui unit.
fn check_standard_library_source_v4_parts(
    source: &StoredSourceRevision,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    executables: &[StandardExecutable],
) -> Result<(StandardSourceFamilies, CheckedStandardExecutable), StandardLibraryCheckError> {
    let types_catalogue = standard_v4_types_catalogue(catalogue)?;
    let origin_partitions = partition_standard_v4_origins(origins)?;
    check_standard_library_source_v4_units(
        source.units(),
        catalogue,
        executables,
        &types_catalogue,
        origin_partitions,
    )
}

fn check_standard_library_source_v4_units(
    source_units: &[StoredSourceUnit],
    catalogue: &CatalogueSnapshot,
    executables: &[StandardExecutable],
    types_catalogue: &CatalogueSnapshot,
    (types_origins, invoke_origins, output_origins, ui_origins): StandardV4OriginPartitions,
) -> Result<(StandardSourceFamilies, CheckedStandardExecutable), StandardLibraryCheckError> {
    let [types_unit, invoke_unit, output_unit, ui_unit] = source_units else {
        return Err(StandardLibraryCheckError::SourceUnitCount {
            actual: source_units.len(),
        });
    };
    if types_unit.id() != STD_TYPES_SOURCE_UNIT_ID
        || types_unit.logical_path() != "std/types.orna"
        || types_unit.ordinal() != 0
        || invoke_unit.id() != STD_INVOKE_SOURCE_UNIT_ID
        || invoke_unit.logical_path() != "std/invoke.orna"
        || invoke_unit.ordinal() != 1
        || output_unit.id() != STD_OUTPUT_SOURCE_UNIT_ID
        || output_unit.logical_path() != "std/output.orna"
        || output_unit.ordinal() != 2
        || ui_unit.id() != STD_UI_SOURCE_UNIT_ID
        || ui_unit.logical_path() != "std/ui.orna"
        || ui_unit.ordinal() != 3
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let bundle = SourceBundle::new([
        SourceUnit::new(types_unit.logical_path(), types_unit.content()),
        SourceUnit::new(invoke_unit.logical_path(), invoke_unit.content()),
        SourceUnit::new(output_unit.logical_path(), output_unit.content()),
        SourceUnit::new(ui_unit.logical_path(), ui_unit.content()),
    ])
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let report = parse_bundle(&bundle);
    if !report.diagnostics().is_empty() {
        return Err(StandardLibraryCheckError::Diagnostics {
            diagnostics: report.diagnostics().to_vec(),
        });
    }
    let [parsed_types, parsed_invoke, parsed_output, parsed_ui] = report.units() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };

    let families =
        reconcile_standard_source(types_unit, parsed_types, types_catalogue, &types_origins)?;
    let checked_executable = reconcile_standard_invoke_executable(
        catalogue,
        &invoke_origins,
        executables,
        invoke_unit,
        parsed_invoke,
    )?;
    reconcile_standard_output_unit(output_unit, parsed_output, catalogue, &output_origins)?;
    reconcile_standard_ui_unit(ui_unit, parsed_ui, catalogue, &ui_origins)?;
    Ok((families, checked_executable))
}

/// Scopes the V2 catalogue to the declarations retained in `std/types.orna`:
/// the standard schemas, value types, and type bindings only.
///
/// The `std.invoke` schema and the standard functions are declared in
/// `std/invoke.orna` and are reconciled by the invoke path; the V1 type-only
/// reconcile contract must not see them. Any other catalogue schema, function,
/// object, enum, or record type fails closed.
fn standard_types_catalogue(
    catalogue: &CatalogueSnapshot,
) -> Result<CatalogueSnapshot, StandardLibraryCheckError> {
    scope_standard_catalogue(catalogue, &[STD_INVOKE_SCHEMA_ID], &[])
}
fn check_standard_source_units(
    source_units: &[StoredSourceUnit],
    expected: &[(SourceUnitId, &str, u32)],
) -> Result<(), StandardLibraryCheckError> {
    if source_units.len() != expected.len()
        || source_units
            .iter()
            .zip(expected)
            .any(|(unit, (id, path, ordinal))| {
                unit.id() != *id || unit.logical_path() != *path || unit.ordinal() != *ordinal
            })
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    Ok(())
}

fn checked_standard_json_executable_for_snapshot(
    snapshot: &VerifiedStandardLibrarySnapshot,
) -> Result<CheckedStandardExecutable, StandardLibraryCheckError> {
    let json_unit = snapshot
        .source()
        .units()
        .iter()
        .find(|unit| unit.id() == STD_JSON_SOURCE_UNIT_ID)
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let json_origins = snapshot
        .origins()
        .iter()
        .filter(|origin| origin.source().source_unit() == STD_JSON_SOURCE_UNIT_ID)
        .cloned()
        .collect::<Vec<_>>();
    let json_bundle = SourceBundle::new([SourceUnit::new(
        json_unit.logical_path(),
        json_unit.content(),
    )])
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let report = parse_bundle(&json_bundle);
    if !report.diagnostics().is_empty() {
        return Err(StandardLibraryCheckError::Diagnostics {
            diagnostics: report.diagnostics().to_vec(),
        });
    }
    let [parsed] = report.units() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let [declaration] = parsed.parsed().server_functions() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let record = expected_standard_json_executable(
        declaration,
        snapshot.catalogue(),
        &json_origins,
        json_unit,
    )?;
    let schema_origin = json_origins
        .iter()
        .find(|origin| origin.identity() == DefinitionIdentity::Schema(STD_JSON_SCHEMA_ID))
        .map(DefinitionOrigin::source)
        .ok_or(StandardLibraryCheckError::MissingSchemaOrigin)?;
    checked_standard_executable_from_record(
        &record,
        snapshot.catalogue(),
        &json_origins,
        schema_origin,
    )
}

/// Checks one retained V5 JSON standard source bundle.
fn check_standard_library_source_v5(
    snapshot: &VerifiedStandardLibrarySnapshot,
) -> Result<CheckedStandardLibrary, StandardLibraryCheckError> {
    let (families, checked_executable) = check_standard_library_source_v5_parts(
        snapshot.source().units(),
        snapshot.catalogue(),
        snapshot.origins(),
        snapshot.executables(),
    )?;
    let checked_json = checked_standard_json_executable_for_snapshot(snapshot)?;
    Ok(CheckedStandardLibrary {
        verified_snapshot: snapshot.clone(),
        schemas: families.schemas,
        value_types: families.value_types,
        type_bindings: families.type_bindings,
        checked_executables: vec![checked_executable, checked_json],
    })
}

/// Checks one retained V6 action standard source bundle.
fn check_standard_library_source_v6(
    snapshot: &VerifiedStandardLibrarySnapshot,
) -> Result<CheckedStandardLibrary, StandardLibraryCheckError> {
    let (families, checked_executable) = check_standard_library_source_v6_parts(
        snapshot.source().units(),
        snapshot.catalogue(),
        snapshot.origins(),
        snapshot.executables(),
    )?;
    let checked_json = checked_standard_json_executable_for_snapshot(snapshot)?;
    Ok(CheckedStandardLibrary {
        verified_snapshot: snapshot.clone(),
        schemas: families.schemas,
        value_types: families.value_types,
        type_bindings: families.type_bindings,
        checked_executables: vec![checked_executable, checked_json],
    })
}

/// Checks one retained V7 standard source bundle.
fn check_standard_library_source_v7(
    snapshot: &VerifiedStandardLibrarySnapshot,
) -> Result<CheckedStandardLibrary, StandardLibraryCheckError> {
    let (families, checked_executables) = check_standard_library_source_v7_parts(
        snapshot.source().units(),
        snapshot.catalogue(),
        snapshot.origins(),
        snapshot.executables(),
    )?;
    Ok(CheckedStandardLibrary {
        verified_snapshot: snapshot.clone(),
        schemas: families.schemas,
        value_types: families.value_types,
        type_bindings: families.type_bindings,
        checked_executables,
    })
}

fn check_standard_library_source_v7_parts(
    source_units: &[StoredSourceUnit],
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    executables: &[StandardExecutable],
) -> Result<(StandardSourceFamilies, Vec<CheckedStandardExecutable>), StandardLibraryCheckError> {
    let [
        types_unit,
        invoke_unit,
        output_unit,
        ui_unit,
        json_unit,
        action_unit,
        window_unit,
    ] = source_units
    else {
        return Err(StandardLibraryCheckError::SourceUnitCount {
            actual: source_units.len(),
        });
    };
    check_standard_source_units(
        source_units,
        &[
            (STD_TYPES_SOURCE_UNIT_ID, "std/types.orna", 0),
            (STD_INVOKE_SOURCE_UNIT_ID, "std/invoke.orna", 1),
            (STD_OUTPUT_SOURCE_UNIT_ID, "std/output.orna", 2),
            (STD_UI_SOURCE_UNIT_ID, "std/ui.orna", 3),
            (STD_JSON_SOURCE_UNIT_ID, "std/json.orna", 4),
            (STD_ACTION_SOURCE_UNIT_ID, "std/action.orna", 5),
            (STD_WINDOW_SOURCE_UNIT_ID, "std/window.orna", 6),
        ],
    )?;
    if executables.len() != 3 {
        return Err(StandardLibraryCheckError::ExecutableCount {
            actual: executables.len(),
        });
    }
    let echo_executable = executables
        .iter()
        .find(|executable| executable.function() == STD_INVOKE_ECHO_FUNCTION_ID)
        .ok_or(StandardLibraryCheckError::ExecutableMismatch)?;
    let json_executable = executables
        .iter()
        .find(|executable| executable.function() == STD_JSON_ENCODE_FUNCTION_ID)
        .ok_or(StandardLibraryCheckError::ExecutableMismatch)?;
    let window_executable = executables
        .iter()
        .find(|executable| executable.function() == STD_UI_WINDOW_FUNCTION_ID)
        .ok_or(StandardLibraryCheckError::ExecutableMismatch)?;
    if [
        echo_executable.function(),
        json_executable.function(),
        window_executable.function(),
    ]
    .into_iter()
    .enumerate()
    .any(|(index, function)| {
        [
            STD_INVOKE_ECHO_FUNCTION_ID,
            STD_JSON_ENCODE_FUNCTION_ID,
            STD_UI_WINDOW_FUNCTION_ID,
        ]
        .into_iter()
        .position(|expected| expected == function)
            != Some(index)
    }) {
        return Err(StandardLibraryCheckError::ExecutableMismatch);
    }

    let mut v6_origins = Vec::with_capacity(origins.len());
    let mut window_origins = Vec::new();
    for origin in origins {
        if origin.source().source_unit() == STD_WINDOW_SOURCE_UNIT_ID {
            window_origins.push(origin.clone());
        } else {
            v6_origins.push(origin.clone());
        }
    }
    let v6_executables = vec![echo_executable.clone(), json_executable.clone()];
    let (families, checked_echo) = check_standard_library_source_v6_parts(
        &[
            types_unit.clone(),
            invoke_unit.clone(),
            output_unit.clone(),
            ui_unit.clone(),
            json_unit.clone(),
            action_unit.clone(),
        ],
        catalogue,
        &v6_origins,
        &v6_executables,
    )?;

    let json_bundle = SourceBundle::new([SourceUnit::new(
        json_unit.logical_path(),
        json_unit.content(),
    )])
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let json_report = parse_bundle(&json_bundle);
    if !json_report.diagnostics().is_empty() {
        return Err(StandardLibraryCheckError::Diagnostics {
            diagnostics: json_report.diagnostics().to_vec(),
        });
    }
    let [parsed_json] = json_report.units() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let [json_function] = parsed_json.parsed().server_functions() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let json_origins = v6_origins
        .iter()
        .filter(|origin| origin.source().source_unit() == STD_JSON_SOURCE_UNIT_ID)
        .cloned()
        .collect::<Vec<_>>();
    let json_record =
        expected_standard_json_executable(json_function, catalogue, &json_origins, json_unit)?;
    let json_schema_origin = json_origins
        .iter()
        .find(|origin| origin.identity() == DefinitionIdentity::Schema(STD_JSON_SCHEMA_ID))
        .map(DefinitionOrigin::source)
        .ok_or(StandardLibraryCheckError::MissingSchemaOrigin)?;
    let checked_json = checked_standard_executable_from_record(
        &json_record,
        catalogue,
        &json_origins,
        json_schema_origin,
    )?;

    let window_bundle = SourceBundle::new([SourceUnit::new(
        window_unit.logical_path(),
        window_unit.content(),
    )])
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let window_report = parse_bundle(&window_bundle);
    if !window_report.diagnostics().is_empty() {
        return Err(StandardLibraryCheckError::Diagnostics {
            diagnostics: window_report.diagnostics().to_vec(),
        });
    }
    let [parsed_window] = window_report.units() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let ui_schema_origin = origins
        .iter()
        .find(|origin| origin.identity() == DefinitionIdentity::Schema(STD_UI_SCHEMA_ID))
        .map(DefinitionOrigin::source)
        .ok_or(StandardLibraryCheckError::MissingSchemaOrigin)?;
    let checked_window = reconcile_standard_window_executable(
        catalogue,
        &window_origins,
        window_executable,
        window_unit,
        parsed_window,
        ui_schema_origin,
    )?;
    Ok((families, vec![checked_echo, checked_json, checked_window]))
}
/// Checks one retained V8 standard source bundle.
///
/// V8 is the append-only V7 child: it retains the seven historical units and
/// executable records byte-for-byte, then appends `std/data.orna` and the
/// retained terminal-table executable. The appended unit owns the `std.data`
/// schema, Rows value/export, and the table declaration; its result type is a
/// checked cross-unit reference to the retained terminal Document type.
fn check_standard_library_source_v8(
    snapshot: &VerifiedStandardLibrarySnapshot,
) -> Result<CheckedStandardLibrary, StandardLibraryCheckError> {
    let (families, checked_executables) = check_standard_library_source_v8_parts(
        snapshot.source().units(),
        snapshot.catalogue(),
        snapshot.origins(),
        snapshot.executables(),
    )?;
    Ok(CheckedStandardLibrary {
        verified_snapshot: snapshot.clone(),
        schemas: families.schemas,
        value_types: families.value_types,
        type_bindings: families.type_bindings,
        checked_executables,
    })
}

fn check_standard_library_source_v8_parts(
    source_units: &[StoredSourceUnit],
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    executables: &[StandardExecutable],
) -> Result<(StandardSourceFamilies, Vec<CheckedStandardExecutable>), StandardLibraryCheckError> {
    let [
        types_unit,
        invoke_unit,
        output_unit,
        ui_unit,
        json_unit,
        action_unit,
        window_unit,
        data_unit,
    ] = source_units
    else {
        return Err(StandardLibraryCheckError::SourceUnitCount {
            actual: source_units.len(),
        });
    };
    check_standard_source_units(
        source_units,
        &[
            (STD_TYPES_SOURCE_UNIT_ID, "std/types.orna", 0),
            (STD_INVOKE_SOURCE_UNIT_ID, "std/invoke.orna", 1),
            (STD_OUTPUT_SOURCE_UNIT_ID, "std/output.orna", 2),
            (STD_UI_SOURCE_UNIT_ID, "std/ui.orna", 3),
            (STD_JSON_SOURCE_UNIT_ID, "std/json.orna", 4),
            (STD_ACTION_SOURCE_UNIT_ID, "std/action.orna", 5),
            (STD_WINDOW_SOURCE_UNIT_ID, "std/window.orna", 6),
            (STD_DATA_SOURCE_UNIT_ID, "std/data.orna", 7),
        ],
    )?;
    if executables.len() != 4 {
        return Err(StandardLibraryCheckError::ExecutableCount {
            actual: executables.len(),
        });
    }
    let expected_functions = [
        STD_INVOKE_ECHO_FUNCTION_ID,
        STD_JSON_ENCODE_FUNCTION_ID,
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        STD_UI_WINDOW_FUNCTION_ID,
    ];
    if executables
        .iter()
        .map(StandardExecutable::function)
        .ne(expected_functions)
    {
        return Err(StandardLibraryCheckError::ExecutableMismatch);
    }

    let mut v7_origins = Vec::with_capacity(origins.len());
    let mut data_origins = Vec::new();
    for origin in origins {
        if origin.source().source_unit() == STD_DATA_SOURCE_UNIT_ID {
            data_origins.push(origin.clone());
        } else {
            v7_origins.push(origin.clone());
        }
    }
    let v7_executables = vec![
        executables[0].clone(),
        executables[1].clone(),
        executables[3].clone(),
    ];
    let v7_catalogue = CatalogueSnapshot::new_with_functions_and_types(
        catalogue.revision(),
        catalogue
            .schemas()
            .iter()
            .filter(|schema| schema.id() != STD_DATA_SCHEMA_ID)
            .cloned()
            .collect(),
        catalogue.object_types().to_vec(),
        catalogue
            .value_types()
            .iter()
            .filter(|value_type| value_type.id() != STD_DATA_ROWS_TYPE_ID)
            .cloned()
            .collect(),
        catalogue
            .type_bindings()
            .iter()
            .filter(|binding| binding.target() != STD_DATA_ROWS_TYPE_ID)
            .cloned()
            .collect(),
        catalogue
            .functions()
            .iter()
            .filter(|function| function.id() != STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID)
            .cloned()
            .collect(),
    )
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let (mut families, mut checked_executables) = check_standard_library_source_v7_parts(
        &[
            types_unit.clone(),
            invoke_unit.clone(),
            output_unit.clone(),
            ui_unit.clone(),
            json_unit.clone(),
            action_unit.clone(),
            window_unit.clone(),
        ],
        &v7_catalogue,
        &v7_origins,
        &v7_executables,
    )?;
    let data_bundle = SourceBundle::new([SourceUnit::new(
        data_unit.logical_path(),
        data_unit.content(),
    )])
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let data_report = parse_bundle(&data_bundle);
    if !data_report.diagnostics().is_empty() {
        return Err(StandardLibraryCheckError::Diagnostics {
            diagnostics: data_report.diagnostics().to_vec(),
        });
    }
    let [parsed_data] = data_report.units() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let data_families =
        reconcile_standard_data_unit(data_unit, parsed_data, catalogue, &data_origins)?;
    families.schemas.extend(data_families.schemas);
    families.value_types.extend(data_families.value_types);
    families.type_bindings.extend(data_families.type_bindings);

    let terminal_schema_origin = v7_origins
        .iter()
        .find(|origin| origin.identity() == DefinitionIdentity::Schema(STD_TERMINAL_SCHEMA_ID))
        .map(DefinitionOrigin::source)
        .ok_or(StandardLibraryCheckError::MissingSchemaOrigin)?;
    let table_executable = reconcile_standard_terminal_executable(
        catalogue,
        &data_origins,
        &executables[2],
        data_unit,
        parsed_data,
        terminal_schema_origin,
    )?;
    checked_executables.insert(2, table_executable);
    Ok((families, checked_executables))
}

/// Checks one retained V9 standard source bundle.
///
/// V9 retains the complete verified V8 Rows snapshot and appends the exact
/// `std/ui_constructors.orna` unit. The appended unit contributes no schema,
/// value type, or binding; it contributes exactly seven external CLIENT
/// constructor functions and their executable evidence.
fn check_standard_library_source_v9(
    snapshot: &VerifiedStandardLibrarySnapshot,
) -> Result<CheckedStandardLibrary, StandardLibraryCheckError> {
    let (families, checked_executables) = check_standard_library_source_v9_parts(
        snapshot.source().units(),
        snapshot.catalogue(),
        snapshot.origins(),
        snapshot.executables(),
    )?;
    Ok(CheckedStandardLibrary {
        verified_snapshot: snapshot.clone(),
        schemas: families.schemas,
        value_types: families.value_types,
        type_bindings: families.type_bindings,
        checked_executables,
    })
}

/// Checks the source-authored CLI session function retained by V10.
pub fn check_standard_cli_repl(
    declaration: &ClientFunctionDeclaration,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    stored_unit: &StoredSourceUnit,
) -> Result<CheckedStandardExecutable, StandardLibraryCheckError> {
    let expected_schema =
        QualifiedSemanticName::new(["std", "cli"]).expect("the fixed CLI schema is valid");
    let expected_function = QualifiedSemanticName::new(["std", "cli", "repl"])
        .expect("the fixed CLI function is valid");
    let expected_ui =
        QualifiedSemanticName::new(["std", "ui", "ui"]).expect("the fixed UI type is valid");
    if stored_unit.id() != STD_CLI_SOURCE_UNIT_ID
        || stored_unit.logical_path() != "std/cli.orna"
        || declaration.external
        || declaration.runtime_contract.is_some()
        || !declaration.capabilities.is_empty()
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let FunctionReturnType::Single(result_type) = &declaration.return_type else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    if !matches!(
        result_type,
        TypeSpecification::Named(name)
            if unquoted_semantic_name(name)? == expected_ui
                && resolved_standard_type_id(result_type, catalogue) == Some(STD_UI_TYPE_ID)
    ) {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let Some(ClientExpression::Call {
        callee: evaluate_callee,
        arguments: evaluate_arguments,
        ..
    }) = declaration.body.as_expression()
    else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    if semantic_name(evaluate_callee)
        != QualifiedSemanticName::new(["std", "cli", "evaluate"])
            .expect("the fixed CLI evaluate intrinsic is valid")
        || evaluate_arguments.len() != 1
        || evaluate_arguments[0].name.is_some()
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let ClientExpression::Call {
        callee: input_callee,
        arguments: input_arguments,
        ..
    } = &evaluate_arguments[0].value
    else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    if semantic_name(input_callee)
        != QualifiedSemanticName::new(["std", "cli", "input"])
            .expect("the fixed CLI input intrinsic is valid")
        || !input_arguments.is_empty()
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let schema = catalogue
        .schema_by_id(STD_CLI_SCHEMA_ID)
        .ok_or(StandardLibraryCheckError::MissingSchema)?;
    if schema.name() != &expected_schema {
        return Err(StandardLibraryCheckError::SchemaNameMismatch {
            actual: schema.name().clone(),
        });
    }
    let function = catalogue
        .function_by_id(STD_CLI_REPL_FUNCTION_ID)
        .ok_or(StandardLibraryCheckError::MissingFunction)?;
    if function.name() != &expected_function
        || function.domain() != FunctionDomain::Client
        || function.security() != CatalogueFunctionSecurity::Invoker
        || function.transaction().is_some()
        || function.volatility() != CatalogueFunctionVolatility::Volatile
        || function.current_revision() != STD_CLI_REPL_FUNCTION_REVISION_ID
        || !function.parameters().is_empty()
        || function.return_type() != &FunctionReturn::Single(ResolvedType::value(STD_UI_TYPE_ID))
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    if origins.len() != 2 {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let source_origin = |span: &SourceSpan| -> Result<SourceOrigin, StandardLibraryCheckError> {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
        let end = u32::try_from(span.end).map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
        SourceOrigin::new(stored_unit.id(), start, end)
            .map_err(|_| StandardLibraryCheckError::SourceMismatch)
    };
    let expected_schema_origin = source_origin(
        &orna_syntax::parse(stored_unit.content())
            .schemas()
            .first()
            .ok_or(StandardLibraryCheckError::SourceMismatch)?
            .span,
    )?;
    let expected_function_origin = source_origin(&declaration.span)?;
    let schema_origin = origins
        .iter()
        .find(|origin| origin.identity() == DefinitionIdentity::Schema(STD_CLI_SCHEMA_ID))
        .map(DefinitionOrigin::source)
        .ok_or(StandardLibraryCheckError::MissingSchemaOrigin)?;
    let function_origin = origins
        .iter()
        .find(|origin| origin.identity() == DefinitionIdentity::Function(STD_CLI_REPL_FUNCTION_ID))
        .map(DefinitionOrigin::source)
        .ok_or(StandardLibraryCheckError::MissingFunctionOrigin)?;
    if schema_origin != expected_schema_origin
        || function_origin != expected_function_origin
        || origins.iter().any(|origin| {
            !matches!(
                origin.identity(),
                DefinitionIdentity::Schema(STD_CLI_SCHEMA_ID)
                    | DefinitionIdentity::Function(STD_CLI_REPL_FUNCTION_ID)
            )
        })
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let result_origin = source_origin(result_type.span())?;
    let references = vec![DefinitionReference::new(
        STD_CLI_REPL_FUNCTION_ID,
        STD_CLI_REPL_FUNCTION_REVISION_ID,
        0,
        DefinitionReferenceTarget::ValueType(STD_UI_TYPE_ID),
        DefinitionReferenceKind::NamedType,
        result_origin,
    )];
    let plan = ExpressionClientPlan::new(ClientExpressionNode::Evaluate {
        expression: Box::new(ClientExpressionNode::Input),
    });
    let payload = plan
        .encode()
        .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let artifact_hash = artifact_payload_digest(&payload)
        .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        CLIENT_PLAN_FORMAT,
        plan.format_version(),
        payload,
        artifact_hash,
    )
    .map_err(|source| StandardLibraryCheckError::Revision { source })?;
    let declaration_bytes = &stored_unit.content().as_bytes()
        [function_origin.byte_start() as usize..function_origin.byte_end() as usize];
    let declaration_content_hash = function_declaration_digest(declaration_bytes)
        .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        function,
        orna_artifact::client_plan::LANGUAGE_VERSION_IDENTITY,
        &artifact,
        &[],
        &references,
    )
    .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let revision = FunctionRevisionRecord::new(
        STD_CLI_REPL_FUNCTION_ID,
        STD_CLI_REPL_FUNCTION_REVISION_ID,
        STD_CLI_REPL_REVISION_NUMBER,
        function_origin,
        declaration_content_hash,
        semantic_hash,
        orna_artifact::client_plan::LANGUAGE_VERSION_IDENTITY,
        artifact,
    )
    .map_err(|source| StandardLibraryCheckError::Revision { source })?
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    let executable =
        StandardExecutable::new(STD_CLI_REPL_FUNCTION_ID, revision, references.clone())
            .map_err(|source| StandardLibraryCheckError::Revision { source })?;
    Ok(CheckedStandardExecutable {
        function_id: STD_CLI_REPL_FUNCTION_ID,
        parameter_ids: Vec::new(),
        revision_id: STD_CLI_REPL_FUNCTION_REVISION_ID,
        revision_number: STD_CLI_REPL_REVISION_NUMBER,
        declaration_origin: function_origin,
        declaration_content_hash,
        semantic_hash,
        semantic_hash_version: FunctionSemanticHashVersion::Version2,
        language_version: orna_artifact::client_plan::LANGUAGE_VERSION_IDENTITY.to_owned(),
        artifact: executable.revision().artifact().clone(),
        references,
        schema_origin,
        function_origin,
        parameter_origins: Vec::new(),
    })
}

fn check_standard_library_source_v10(
    snapshot: &VerifiedStandardLibrarySnapshot,
) -> Result<CheckedStandardLibrary, StandardLibraryCheckError> {
    let source_units = snapshot.source().units();
    let executables = snapshot.executables();
    if source_units.len() != 10 {
        return Err(StandardLibraryCheckError::SourceUnitCount {
            actual: source_units.len(),
        });
    }
    if executables.len() != 12 {
        return Err(StandardLibraryCheckError::ExecutableCount {
            actual: executables.len(),
        });
    }
    let expected_functions = [
        STD_INVOKE_ECHO_FUNCTION_ID,
        STD_JSON_ENCODE_FUNCTION_ID,
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        STD_UI_WINDOW_FUNCTION_ID,
        STD_UI_TEXT_FUNCTION_ID,
        STD_UI_BUTTON_FUNCTION_ID,
        STD_UI_PANEL_FUNCTION_ID,
        STD_UI_ROW_FUNCTION_ID,
        STD_UI_COLUMN_FUNCTION_ID,
        STD_UI_TEXT_INPUT_FUNCTION_ID,
        STD_UI_TABS_FUNCTION_ID,
        STD_CLI_REPL_FUNCTION_ID,
    ];
    if executables
        .iter()
        .map(StandardExecutable::function)
        .ne(expected_functions)
    {
        return Err(StandardLibraryCheckError::ExecutableMismatch);
    }
    let cli_unit = source_units
        .last()
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let cli_origins = snapshot
        .origins()
        .iter()
        .filter(|origin| origin.source().source_unit() == STD_CLI_SOURCE_UNIT_ID)
        .cloned()
        .collect::<Vec<_>>();
    let parent_origins = snapshot
        .origins()
        .iter()
        .filter(|origin| origin.source().source_unit() != STD_CLI_SOURCE_UNIT_ID)
        .cloned()
        .collect::<Vec<_>>();
    let parent_catalogue = CatalogueSnapshot::new_with_functions_and_types(
        snapshot.catalogue().revision(),
        snapshot
            .catalogue()
            .schemas()
            .iter()
            .filter(|schema| schema.id() != STD_CLI_SCHEMA_ID)
            .cloned()
            .collect(),
        snapshot.catalogue().object_types().to_vec(),
        snapshot.catalogue().value_types().to_vec(),
        snapshot.catalogue().type_bindings().to_vec(),
        snapshot
            .catalogue()
            .functions()
            .iter()
            .filter(|function| function.id() != STD_CLI_REPL_FUNCTION_ID)
            .cloned()
            .collect(),
    )
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let (mut families, mut checked_executables) = check_standard_library_source_v9_parts(
        &source_units[..9],
        &parent_catalogue,
        &parent_origins,
        &executables[..11],
    )?;
    let cli_bundle =
        SourceBundle::new([SourceUnit::new(cli_unit.logical_path(), cli_unit.content())])
            .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let cli_report = parse_bundle(&cli_bundle);
    if !cli_report.diagnostics().is_empty() {
        return Err(StandardLibraryCheckError::Diagnostics {
            diagnostics: cli_report.diagnostics().to_vec(),
        });
    }
    let [parsed_cli] = cli_report.units() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    if parsed_cli.source_text() != cli_unit.content()
        || parsed_cli.source_text() != parsed_cli.syntax_text()
        || parsed_cli.parsed().schemas().len() != 1
        || parsed_cli.parsed().client_functions().len() != 1
        || !parsed_cli.parsed().server_functions().is_empty()
        || !parsed_cli.parsed().object_types().is_empty()
        || !parsed_cli.parsed().enum_types().is_empty()
        || !parsed_cli.parsed().primitive_value_types().is_empty()
        || !parsed_cli.parsed().opaque_value_types().is_empty()
        || !parsed_cli.parsed().record_value_types().is_empty()
        || !parsed_cli.parsed().field_renames().is_empty()
        || !parsed_cli.parsed().type_exports().is_empty()
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let checked_cli = check_standard_cli_repl(
        &parsed_cli.parsed().client_functions()[0],
        snapshot.catalogue(),
        &cli_origins,
        cli_unit,
    )?;
    families.schemas.push(CheckedStandardSchema {
        id: STD_CLI_SCHEMA_ID,
        name: unquoted_semantic_name(&parsed_cli.parsed().schemas()[0].name)?,
        origin: checked_cli.schema_origin(),
    });
    checked_executables.push(checked_cli);
    Ok(CheckedStandardLibrary {
        verified_snapshot: snapshot.clone(),
        schemas: families.schemas,
        value_types: families.value_types,
        type_bindings: families.type_bindings,
        checked_executables,
    })
}
fn check_standard_library_source_v9_parts(
    source_units: &[StoredSourceUnit],
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    executables: &[StandardExecutable],
) -> Result<(StandardSourceFamilies, Vec<CheckedStandardExecutable>), StandardLibraryCheckError> {
    let [
        types_unit,
        invoke_unit,
        output_unit,
        ui_unit,
        json_unit,
        action_unit,
        window_unit,
        data_unit,
        constructors_unit,
    ] = source_units
    else {
        return Err(StandardLibraryCheckError::SourceUnitCount {
            actual: source_units.len(),
        });
    };
    check_standard_source_units(
        source_units,
        &[
            (STD_TYPES_SOURCE_UNIT_ID, "std/types.orna", 0),
            (STD_INVOKE_SOURCE_UNIT_ID, "std/invoke.orna", 1),
            (STD_OUTPUT_SOURCE_UNIT_ID, "std/output.orna", 2),
            (STD_UI_SOURCE_UNIT_ID, "std/ui.orna", 3),
            (STD_JSON_SOURCE_UNIT_ID, "std/json.orna", 4),
            (STD_ACTION_SOURCE_UNIT_ID, "std/action.orna", 5),
            (STD_WINDOW_SOURCE_UNIT_ID, "std/window.orna", 6),
            (STD_DATA_SOURCE_UNIT_ID, "std/data.orna", 7),
            (
                STD_UI_CONSTRUCTORS_SOURCE_UNIT_ID,
                "std/ui_constructors.orna",
                8,
            ),
        ],
    )?;
    if executables.len() != 11 {
        return Err(StandardLibraryCheckError::ExecutableCount {
            actual: executables.len(),
        });
    }
    let expected_functions = [
        STD_INVOKE_ECHO_FUNCTION_ID,
        STD_JSON_ENCODE_FUNCTION_ID,
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        STD_UI_WINDOW_FUNCTION_ID,
        STD_UI_TEXT_FUNCTION_ID,
        STD_UI_BUTTON_FUNCTION_ID,
        STD_UI_PANEL_FUNCTION_ID,
        STD_UI_ROW_FUNCTION_ID,
        STD_UI_COLUMN_FUNCTION_ID,
        STD_UI_TEXT_INPUT_FUNCTION_ID,
        STD_UI_TABS_FUNCTION_ID,
    ];
    if executables.len() != expected_functions.len()
        || executables
            .iter()
            .map(StandardExecutable::function)
            .ne(expected_functions)
    {
        return Err(StandardLibraryCheckError::ExecutableMismatch);
    }

    let mut v8_origins = Vec::with_capacity(origins.len());
    let mut constructor_origins = Vec::new();
    for origin in origins {
        if origin.source().source_unit() == STD_UI_CONSTRUCTORS_SOURCE_UNIT_ID {
            constructor_origins.push(origin.clone());
        } else {
            v8_origins.push(origin.clone());
        }
    }
    let v8_functions = catalogue
        .functions()
        .iter()
        .filter(|function| expected_functions[..4].contains(&function.id()))
        .cloned()
        .collect::<Vec<_>>();
    if v8_functions.len() != 4 {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let v8_catalogue = CatalogueSnapshot::new_with_functions_and_types(
        catalogue.revision(),
        catalogue.schemas().to_vec(),
        catalogue.object_types().to_vec(),
        catalogue.value_types().to_vec(),
        catalogue.type_bindings().to_vec(),
        v8_functions,
    )
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let (families, mut checked_executables) = check_standard_library_source_v8_parts(
        &[
            types_unit.clone(),
            invoke_unit.clone(),
            output_unit.clone(),
            ui_unit.clone(),
            json_unit.clone(),
            action_unit.clone(),
            window_unit.clone(),
            data_unit.clone(),
        ],
        &v8_catalogue,
        &v8_origins,
        &executables[..4],
    )?;

    let constructors_bundle = SourceBundle::new([SourceUnit::new(
        constructors_unit.logical_path(),
        constructors_unit.content(),
    )])
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let constructors_report = parse_bundle(&constructors_bundle);
    if !constructors_report.diagnostics().is_empty() {
        return Err(StandardLibraryCheckError::Diagnostics {
            diagnostics: constructors_report.diagnostics().to_vec(),
        });
    }
    let [parsed_constructors] = constructors_report.units() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    if parsed_constructors.source_text() != constructors_unit.content()
        || parsed_constructors.source_text() != parsed_constructors.syntax_text()
        || !parsed_constructors.parsed().schemas().is_empty()
        || !parsed_constructors.parsed().object_types().is_empty()
        || !parsed_constructors.parsed().enum_types().is_empty()
        || !parsed_constructors
            .parsed()
            .primitive_value_types()
            .is_empty()
        || !parsed_constructors.parsed().opaque_value_types().is_empty()
        || !parsed_constructors.parsed().record_value_types().is_empty()
        || !parsed_constructors.parsed().field_renames().is_empty()
        || !parsed_constructors.parsed().type_exports().is_empty()
        || !parsed_constructors.parsed().server_functions().is_empty()
        || parsed_constructors.parsed().client_functions().len() != 7
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let ui_schema_origin = v8_origins
        .iter()
        .find(|origin| origin.identity() == DefinitionIdentity::Schema(STD_UI_SCHEMA_ID))
        .map(DefinitionOrigin::source)
        .ok_or(StandardLibraryCheckError::MissingSchemaOrigin)?;

    for (index, declaration) in parsed_constructors
        .parsed()
        .client_functions()
        .iter()
        .enumerate()
    {
        let expected_name = unquoted_semantic_name(&declaration.name)?;
        let spec = standard_ui_constructor_spec(&expected_name)
            .ok_or(StandardLibraryCheckError::SourceMismatch)?;
        if spec.function_id != expected_functions[index + 4] {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
        let declaration_origins = constructor_origins
            .iter()
            .filter(|origin| match origin.identity() {
                DefinitionIdentity::Function(function) => function == spec.function_id,
                DefinitionIdentity::Parameter { owner, .. } => owner == spec.function_id,
                _ => false,
            })
            .cloned()
            .collect::<Vec<_>>();
        let checked = check_standard_ui_constructor(declaration, catalogue, &declaration_origins)?;
        let checked_executable = reconcile_standard_ui_constructor_executable(
            catalogue,
            &declaration_origins,
            &executables[index + 4],
            constructors_unit,
            declaration,
            checked,
            ui_schema_origin,
        )?;
        checked_executables.push(checked_executable);
    }
    if constructor_origins.len()
        != parsed_constructors
            .parsed()
            .client_functions()
            .iter()
            .map(|declaration| declaration.parameters.len() + 1)
            .sum::<usize>()
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    Ok((families, checked_executables))
}
/// Reconciles the appended `std/data.orna` unit and returns its catalogue
/// families. The table declaration is checked through the shared retained
/// terminal-table checker below; this function additionally requires the
/// complete source-owned origin set and the cross-unit terminal reference.
fn reconcile_standard_data_unit(
    stored_unit: &StoredSourceUnit,
    parsed_unit: &ParsedSourceUnit,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
) -> Result<StandardSourceFamilies, StandardLibraryCheckError> {
    if parsed_unit.source_text() != stored_unit.content()
        || parsed_unit.source_text() != parsed_unit.syntax_text()
        || !parsed_unit.parsed().object_types().is_empty()
        || !parsed_unit.parsed().enum_types().is_empty()
        || !parsed_unit.parsed().primitive_value_types().is_empty()
        || !parsed_unit.parsed().record_value_types().is_empty()
        || !parsed_unit.parsed().field_renames().is_empty()
        || !parsed_unit.parsed().client_functions().is_empty()
        || parsed_unit.parsed().schemas().len() != 1
        || parsed_unit.parsed().opaque_value_types().len() != 1
        || parsed_unit.parsed().type_exports().len() != 1
        || parsed_unit.parsed().server_functions().len() != 1
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let [schema_declaration] = parsed_unit.parsed().schemas() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let [rows_type_declaration] = parsed_unit.parsed().opaque_value_types() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let [rows_export] = parsed_unit.parsed().type_exports() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let [table_function] = parsed_unit.parsed().server_functions() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };

    let expected_schema_name =
        QualifiedSemanticName::new(["std", "data"]).expect("fixed data schema is valid");
    let schema_name = unquoted_semantic_name(&schema_declaration.name)?;
    if schema_name != expected_schema_name
        || catalogue
            .schema_by_id(STD_DATA_SCHEMA_ID)
            .ok_or(StandardLibraryCheckError::MissingSchema)?
            .name()
            != &schema_name
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let expected_rows_name =
        QualifiedSemanticName::new(["std", "data", "rows"]).expect("fixed Rows type is valid");
    let rows_name = unquoted_semantic_name(&rows_type_declaration.name)?;
    let rows_contract = decode_string_literal(&rows_type_declaration.kernel_contract)
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let rows_definition = catalogue
        .value_type_by_id(STD_DATA_ROWS_TYPE_ID)
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    if rows_name != expected_rows_name
        || rows_contract != "orna.std.value.rows@1"
        || rows_definition.name() != &rows_name
        || rows_definition.kind() != ValueTypeKind::Opaque
        || rows_definition.mutability() != ValueTypeMutability::Immutable
        || rows_definition.persistence() != ValueTypePersistence::Transient
        || rows_definition.representation_contract() != rows_contract
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let expected_binding_name =
        QualifiedSemanticName::new(["std", "rows"]).expect("fixed Rows export is valid");
    let rows_binding = catalogue
        .type_binding_by_name(&TypeLookupName::qualified(expected_binding_name.clone()))
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let TypeExportTarget::Qualified { name: target_name } = &rows_export.target else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    if unquoted_semantic_name(&rows_export.source_type)? != rows_name
        || unquoted_semantic_name(target_name)? != expected_binding_name
        || rows_binding.id() != STD_DATA_ROWS_TYPE_BINDING_ID
        || rows_binding.kind() != TypeBindingKind::Qualified
        || rows_binding.name() != &TypeLookupName::qualified(expected_binding_name)
        || rows_binding.target() != STD_DATA_ROWS_TYPE_ID
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let mut origins_by_identity = HashMap::with_capacity(origins.len());
    for origin in origins {
        if !matches!(
            origin.identity(),
            DefinitionIdentity::Schema(_)
                | DefinitionIdentity::ValueType(_)
                | DefinitionIdentity::TypeBinding(_)
                | DefinitionIdentity::Function(_)
                | DefinitionIdentity::Parameter { .. }
        ) || origins_by_identity
            .insert(origin.identity(), origin.source())
            .is_some()
        {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
    }
    let schema_origin = take_origin(
        &mut origins_by_identity,
        DefinitionIdentity::Schema(STD_DATA_SCHEMA_ID),
        stored_unit.id(),
        &schema_declaration.span,
    )?;
    let rows_origin = take_origin(
        &mut origins_by_identity,
        DefinitionIdentity::ValueType(STD_DATA_ROWS_TYPE_ID),
        stored_unit.id(),
        &rows_type_declaration.span,
    )?;
    let binding_origin = take_origin(
        &mut origins_by_identity,
        DefinitionIdentity::TypeBinding(rows_binding.id()),
        stored_unit.id(),
        &rows_export.span,
    )?;
    let function_origin = take_origin(
        &mut origins_by_identity,
        DefinitionIdentity::Function(STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID),
        stored_unit.id(),
        &table_function.span,
    )?;
    let parameter = table_function
        .parameters
        .first()
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let parameter_origin = take_origin(
        &mut origins_by_identity,
        DefinitionIdentity::Parameter {
            owner: STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
            parameter: STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
        },
        stored_unit.id(),
        &parameter.span,
    )?;
    if !origins_by_identity.is_empty()
        || schema_origin.source_unit() != stored_unit.id()
        || rows_origin.source_unit() != stored_unit.id()
        || binding_origin.source_unit() != stored_unit.id()
        || function_origin.source_unit() != stored_unit.id()
        || parameter_origin.source_unit() != stored_unit.id()
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    check_standard_terminal_present_table(
        table_function,
        catalogue,
        origins,
        STD_DATA_ROWS_TYPE_ID,
    )?;
    Ok(StandardSourceFamilies {
        schemas: vec![CheckedStandardSchema {
            id: STD_DATA_SCHEMA_ID,
            name: schema_name,
            origin: schema_origin,
        }],
        value_types: vec![CheckedStandardValueType {
            id: STD_DATA_ROWS_TYPE_ID,
            name: rows_name,
            kind: rows_definition.kind(),
            mutability: rows_definition.mutability(),
            persistence: rows_definition.persistence(),
            representation_contract: rows_definition.representation_contract().to_owned(),
            origin: rows_origin,
        }],
        type_bindings: vec![CheckedStandardTypeBinding {
            id: rows_binding.id(),
            kind: rows_binding.kind(),
            name: rows_binding.name().clone(),
            target: rows_binding.target(),
            origin: binding_origin,
        }],
    })
}

fn check_standard_library_source_v6_parts(
    source_units: &[StoredSourceUnit],
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    executables: &[StandardExecutable],
) -> Result<(StandardSourceFamilies, CheckedStandardExecutable), StandardLibraryCheckError> {
    let [
        types_unit,
        invoke_unit,
        output_unit,
        ui_unit,
        json_unit,
        action_unit,
    ] = source_units
    else {
        return Err(StandardLibraryCheckError::SourceUnitCount {
            actual: source_units.len(),
        });
    };
    check_standard_source_units(
        source_units,
        &[
            (STD_TYPES_SOURCE_UNIT_ID, "std/types.orna", 0),
            (STD_INVOKE_SOURCE_UNIT_ID, "std/invoke.orna", 1),
            (STD_OUTPUT_SOURCE_UNIT_ID, "std/output.orna", 2),
            (STD_UI_SOURCE_UNIT_ID, "std/ui.orna", 3),
            (STD_JSON_SOURCE_UNIT_ID, "std/json.orna", 4),
            (STD_ACTION_SOURCE_UNIT_ID, "std/action.orna", 5),
        ],
    )?;

    let mut v5_origins = Vec::with_capacity(origins.len());
    let mut action_origins = Vec::new();
    for origin in origins {
        if origin.source().source_unit() == STD_ACTION_SOURCE_UNIT_ID {
            action_origins.push(origin.clone());
        } else {
            v5_origins.push(origin.clone());
        }
    }
    let (mut families, checked_executable) = check_standard_library_source_v5_parts(
        &[
            types_unit.clone(),
            invoke_unit.clone(),
            output_unit.clone(),
            ui_unit.clone(),
            json_unit.clone(),
        ],
        catalogue,
        &v5_origins,
        executables,
    )?;
    let bundle = SourceBundle::new([SourceUnit::new(
        action_unit.logical_path(),
        action_unit.content(),
    )])
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let report = parse_bundle(&bundle);
    if !report.diagnostics().is_empty() {
        return Err(StandardLibraryCheckError::Diagnostics {
            diagnostics: report.diagnostics().to_vec(),
        });
    }
    let [parsed_action] = report.units() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let action_families =
        reconcile_standard_action_unit(action_unit, parsed_action, catalogue, &action_origins)?;
    families.schemas.extend(action_families.schemas);
    families.value_types.extend(action_families.value_types);
    families.type_bindings.extend(action_families.type_bindings);

    Ok((families, checked_executable))
}

fn check_standard_library_source_v5_parts(
    source_units: &[StoredSourceUnit],
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    executables: &[StandardExecutable],
) -> Result<(StandardSourceFamilies, CheckedStandardExecutable), StandardLibraryCheckError> {
    let [types_unit, invoke_unit, output_unit, ui_unit, json_unit] = source_units else {
        return Err(StandardLibraryCheckError::SourceUnitCount {
            actual: source_units.len(),
        });
    };
    check_standard_source_units(
        source_units,
        &[
            (STD_TYPES_SOURCE_UNIT_ID, "std/types.orna", 0),
            (STD_INVOKE_SOURCE_UNIT_ID, "std/invoke.orna", 1),
            (STD_OUTPUT_SOURCE_UNIT_ID, "std/output.orna", 2),
            (STD_UI_SOURCE_UNIT_ID, "std/ui.orna", 3),
            (STD_JSON_SOURCE_UNIT_ID, "std/json.orna", 4),
        ],
    )?;
    let bundle = SourceBundle::new([
        SourceUnit::new(types_unit.logical_path(), types_unit.content()),
        SourceUnit::new(invoke_unit.logical_path(), invoke_unit.content()),
        SourceUnit::new(output_unit.logical_path(), output_unit.content()),
        SourceUnit::new(ui_unit.logical_path(), ui_unit.content()),
        SourceUnit::new(json_unit.logical_path(), json_unit.content()),
    ])
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let report = parse_bundle(&bundle);
    if !report.diagnostics().is_empty() {
        return Err(StandardLibraryCheckError::Diagnostics {
            diagnostics: report.diagnostics().to_vec(),
        });
    }
    let [
        parsed_types,
        parsed_invoke,
        parsed_output,
        parsed_ui,
        parsed_json,
    ] = report.units()
    else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };

    let mut retained_v4_origins = Vec::with_capacity(origins.len());
    let mut json_origins = Vec::new();
    for origin in origins {
        if origin.source().source_unit() == STD_JSON_SOURCE_UNIT_ID {
            json_origins.push(origin.clone());
        } else {
            retained_v4_origins.push(origin.clone());
        }
    }
    let origin_partitions = partition_standard_v4_origins(&retained_v4_origins)?;
    let types_catalogue = standard_v5_types_catalogue(catalogue)?;
    let families = reconcile_standard_source(
        types_unit,
        parsed_types,
        &types_catalogue,
        &origin_partitions.0,
    )?;
    let Some(echo_executable) = executables
        .iter()
        .find(|executable| executable.function() == STD_INVOKE_ECHO_FUNCTION_ID)
    else {
        return Err(StandardLibraryCheckError::ExecutableMismatch);
    };
    let Some(json_executable) = executables
        .iter()
        .find(|executable| executable.function() == STD_JSON_ENCODE_FUNCTION_ID)
    else {
        return Err(StandardLibraryCheckError::ExecutableMismatch);
    };
    if executables.len() != 2 {
        return Err(StandardLibraryCheckError::ExecutableCount {
            actual: executables.len(),
        });
    }
    let checked_executable = reconcile_standard_invoke_executable(
        catalogue,
        &origin_partitions.1,
        std::slice::from_ref(echo_executable),
        invoke_unit,
        parsed_invoke,
    )?;
    reconcile_standard_output_unit(output_unit, parsed_output, catalogue, &origin_partitions.2)?;
    reconcile_standard_ui_unit(ui_unit, parsed_ui, catalogue, &origin_partitions.3)?;
    reconcile_standard_json_unit(json_unit, parsed_json, catalogue, &json_origins)?;
    let [json_function] = parsed_json.parsed().server_functions() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    reconcile_standard_json_executable(
        json_executable,
        json_function,
        catalogue,
        &json_origins,
        json_unit,
    )?;
    Ok((families, checked_executable))
}

fn expected_standard_json_executable(
    declaration: &ServerFunctionDeclaration,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    stored_unit: &StoredSourceUnit,
) -> Result<StandardExecutable, StandardLibraryCheckError> {
    check_standard_json_encode(declaration, catalogue, origins, STD_JSON_VALUE_TYPE_ID)?;
    let function_origin = origins
        .iter()
        .find(|origin| {
            origin.identity() == DefinitionIdentity::Function(STD_JSON_ENCODE_FUNCTION_ID)
        })
        .ok_or(StandardLibraryCheckError::PresenterMissingFunctionOrigin)?
        .source();
    let declaration_bytes = &stored_unit.content().as_bytes()
        [function_origin.byte_start() as usize..function_origin.byte_end() as usize];
    let declaration_content_hash = function_declaration_digest(declaration_bytes)
        .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let function = catalogue
        .function_by_id(STD_JSON_ENCODE_FUNCTION_ID)
        .ok_or(StandardLibraryCheckError::PresenterMissingFunction)?;
    let payload = JsonEncodePlan::new(STD_JSON_ENCODE_PARAMETER_ID, STD_JSON_VALUE_TYPE_ID)
        .expect("fixed JSON presenter identities are valid")
        .encode()
        .expect("the fixed JSON presenter payload is within the format limit");
    let artifact_hash = artifact_payload_digest(&payload)
        .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        server_json_encode::FORMAT_IDENTITY,
        server_json_encode::FORMAT_VERSION,
        payload,
        artifact_hash,
    )
    .map_err(|source| StandardLibraryCheckError::Revision { source })?;
    let semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        function,
        server_json_encode::LANGUAGE_VERSION_IDENTITY,
        &artifact,
        &[],
        &[],
    )
    .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let revision = FunctionRevisionRecord::new(
        STD_JSON_ENCODE_FUNCTION_ID,
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        u64::from(server_json_encode::FORMAT_VERSION),
        function_origin,
        declaration_content_hash,
        semantic_hash,
        server_json_encode::LANGUAGE_VERSION_IDENTITY,
        artifact,
    )
    .map_err(|source| StandardLibraryCheckError::Revision { source })?
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    StandardExecutable::new(STD_JSON_ENCODE_FUNCTION_ID, revision, Vec::new())
        .map_err(|source| StandardLibraryCheckError::Revision { source })
}

fn expected_standard_terminal_executable(
    declaration: &ServerFunctionDeclaration,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    stored_unit: &StoredSourceUnit,
) -> Result<StandardExecutable, StandardLibraryCheckError> {
    let checked = check_standard_terminal_present_table(
        declaration,
        catalogue,
        origins,
        STD_DATA_ROWS_TYPE_ID,
    )?;
    let function_origin = origins
        .iter()
        .find(|origin| {
            origin.identity()
                == DefinitionIdentity::Function(STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID)
        })
        .ok_or(StandardLibraryCheckError::PresenterMissingFunctionOrigin)?
        .source();
    let declaration_bytes = &stored_unit.content().as_bytes()
        [function_origin.byte_start() as usize..function_origin.byte_end() as usize];
    let declaration_content_hash = function_declaration_digest(declaration_bytes)
        .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let function = catalogue
        .function_by_id(STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID)
        .ok_or(StandardLibraryCheckError::PresenterMissingFunction)?;
    let payload = server_terminal_table::TerminalTablePlan::new(
        STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
        STD_DATA_ROWS_TYPE_ID,
    )
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)?
    .encode()
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let artifact_hash = artifact_payload_digest(&payload)
        .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        server_terminal_table::FORMAT_IDENTITY,
        server_terminal_table::FORMAT_VERSION,
        payload,
        artifact_hash,
    )
    .map_err(|source| StandardLibraryCheckError::Revision { source })?;
    let source_origin = |span: &SourceSpan| -> Result<SourceOrigin, StandardLibraryCheckError> {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
        let end = u32::try_from(span.end).map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
        SourceOrigin::new(stored_unit.id(), start, end)
            .map_err(|_| StandardLibraryCheckError::SourceMismatch)
    };
    let parameter = declaration
        .parameters
        .first()
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let result = match &declaration.return_type {
        FunctionReturnType::Single(result) => result,
        FunctionReturnType::Rows { .. } | FunctionReturnType::Stream { .. } => {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
    };
    let body = declaration
        .body
        .as_no_input_parameter_select()
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let references = vec![
        DefinitionReference::new(
            checked.function_id(),
            checked.revision_id(),
            0,
            DefinitionReferenceTarget::ValueType(STD_DATA_ROWS_TYPE_ID),
            DefinitionReferenceKind::NamedType,
            source_origin(parameter.type_specification.span())?,
        ),
        DefinitionReference::new(
            checked.function_id(),
            checked.revision_id(),
            1,
            DefinitionReferenceTarget::ValueType(STD_TERMINAL_DOCUMENT_TYPE_ID),
            DefinitionReferenceKind::NamedType,
            source_origin(result.span())?,
        ),
        DefinitionReference::new(
            checked.function_id(),
            checked.revision_id(),
            2,
            DefinitionReferenceTarget::Parameter {
                owner: STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
                parameter: STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
            },
            DefinitionReferenceKind::ParameterRead,
            source_origin(&body.parameter.span)?,
        ),
    ];
    let semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        function,
        server_terminal_table::LANGUAGE_VERSION_IDENTITY,
        &artifact,
        &[],
        &references,
    )
    .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let revision = FunctionRevisionRecord::new(
        checked.function_id(),
        checked.revision_id(),
        u64::from(server_terminal_table::FORMAT_VERSION),
        function_origin,
        declaration_content_hash,
        semantic_hash,
        server_terminal_table::LANGUAGE_VERSION_IDENTITY,
        artifact,
    )
    .map_err(|source| StandardLibraryCheckError::Revision { source })?
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    StandardExecutable::new(checked.function_id(), revision, references)
        .map_err(|source| StandardLibraryCheckError::Revision { source })
}

fn reconcile_standard_terminal_executable(
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    stored: &StandardExecutable,
    stored_unit: &StoredSourceUnit,
    parsed_unit: &ParsedSourceUnit,
    schema_origin: SourceOrigin,
) -> Result<CheckedStandardExecutable, StandardLibraryCheckError> {
    if parsed_unit.source_text() != stored_unit.content()
        || parsed_unit.source_text() != parsed_unit.syntax_text()
        || parsed_unit.parsed().schemas().len() != 1
        || !parsed_unit.parsed().object_types().is_empty()
        || !parsed_unit.parsed().enum_types().is_empty()
        || !parsed_unit.parsed().primitive_value_types().is_empty()
        || parsed_unit.parsed().opaque_value_types().len() != 1
        || !parsed_unit.parsed().record_value_types().is_empty()
        || !parsed_unit.parsed().field_renames().is_empty()
        || !parsed_unit.parsed().client_functions().is_empty()
        || parsed_unit.parsed().server_functions().len() != 1
        || parsed_unit.parsed().type_exports().len() != 1
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let [declaration] = parsed_unit.parsed().server_functions() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let expected =
        expected_standard_terminal_executable(declaration, catalogue, origins, stored_unit)?;
    if stored != &expected {
        return Err(StandardLibraryCheckError::ExecutableMismatch);
    }
    checked_standard_executable_from_record(&expected, catalogue, origins, schema_origin)
}

fn reconcile_standard_json_executable(
    stored: &StandardExecutable,
    declaration: &ServerFunctionDeclaration,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    stored_unit: &StoredSourceUnit,
) -> Result<(), StandardLibraryCheckError> {
    let expected = expected_standard_json_executable(declaration, catalogue, origins, stored_unit)?;
    if stored != &expected {
        return Err(StandardLibraryCheckError::ExecutableMismatch);
    }
    Ok(())
}

fn checked_standard_executable_from_record(
    record: &StandardExecutable,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    schema_origin: SourceOrigin,
) -> Result<CheckedStandardExecutable, StandardLibraryCheckError> {
    let function_id = record.function();
    let function = catalogue
        .function_by_id(function_id)
        .ok_or(StandardLibraryCheckError::MissingFunction)?;
    let parameter_ids = function
        .parameters()
        .iter()
        .map(|parameter| parameter.id())
        .collect::<Vec<_>>();
    let parameter_origins = parameter_ids
        .iter()
        .map(|parameter| {
            origins
                .iter()
                .find(|origin| {
                    origin.identity()
                        == DefinitionIdentity::Parameter {
                            owner: function_id,
                            parameter: *parameter,
                        }
                })
                .ok_or(StandardLibraryCheckError::PresenterMissingParameterOrigin)
                .map(DefinitionOrigin::source)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let function_origin = origins
        .iter()
        .find(|origin| origin.identity() == DefinitionIdentity::Function(function_id))
        .ok_or(StandardLibraryCheckError::PresenterMissingFunctionOrigin)?
        .source();
    let revision = record.revision();
    Ok(CheckedStandardExecutable {
        function_id,
        parameter_ids,
        revision_id: revision.id(),
        revision_number: revision.revision_number(),
        declaration_origin: revision.declaration_origin(),
        declaration_content_hash: revision.declaration_content_hash(),
        semantic_hash: revision.semantic_hash(),
        semantic_hash_version: revision.semantic_hash_version(),
        language_version: revision.language_version().to_owned(),
        artifact: revision.artifact().clone(),
        references: record.references().to_vec(),
        parameter_origins,
        schema_origin,
        function_origin,
    })
}

fn reconcile_standard_ui_constructor_executable(
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    stored: &StandardExecutable,
    stored_unit: &StoredSourceUnit,
    declaration: &ClientFunctionDeclaration,
    checked: CheckedStandardUiConstructor,
    schema_origin: SourceOrigin,
) -> Result<CheckedStandardExecutable, StandardLibraryCheckError> {
    let function_origin = origins
        .iter()
        .find(|origin| origin.identity() == DefinitionIdentity::Function(checked.function_id()))
        .ok_or(StandardLibraryCheckError::MissingFunctionOrigin)?
        .source();
    let source_origin = |span: &SourceSpan| -> Result<SourceOrigin, StandardLibraryCheckError> {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
        let end = u32::try_from(span.end).map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
        SourceOrigin::new(stored_unit.id(), start, end)
            .map_err(|_| StandardLibraryCheckError::SourceMismatch)
    };
    let mut references = Vec::with_capacity(declaration.parameters.len() + 1);
    for (ordinal, parameter) in declaration.parameters.iter().enumerate() {
        let target = resolved_standard_type_id(&parameter.type_specification, catalogue)
            .ok_or(StandardLibraryCheckError::SourceMismatch)?;
        references.push(DefinitionReference::new(
            checked.function_id(),
            checked.revision_id(),
            ordinal as u32,
            DefinitionReferenceTarget::ValueType(target),
            DefinitionReferenceKind::NamedType,
            source_origin(parameter.type_specification.span())?,
        ));
    }
    let FunctionReturnType::Single(result_type) = &declaration.return_type else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    references.push(DefinitionReference::new(
        checked.function_id(),
        checked.revision_id(),
        declaration.parameters.len() as u32,
        DefinitionReferenceTarget::ValueType(STD_UI_TYPE_ID),
        DefinitionReferenceKind::NamedType,
        source_origin(result_type.span())?,
    ));
    let plan = ExpressionClientPlan::new(ClientExpressionNode::ExternalContract {
        identity: checked.runtime_contract().to_owned(),
    });
    let payload = plan
        .encode()
        .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let artifact_hash = artifact_payload_digest(&payload)
        .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        CLIENT_PLAN_FORMAT,
        plan.format_version(),
        payload,
        artifact_hash,
    )
    .map_err(|source| StandardLibraryCheckError::Revision { source })?;
    let function = catalogue
        .function_by_id(checked.function_id())
        .ok_or(StandardLibraryCheckError::MissingFunction)?;
    let semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        function,
        orna_artifact::client_plan::LANGUAGE_VERSION_IDENTITY,
        &artifact,
        &[],
        &references,
    )
    .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let declaration_bytes = &stored_unit.content().as_bytes()
        [function_origin.byte_start() as usize..function_origin.byte_end() as usize];
    let declaration_content_hash = function_declaration_digest(declaration_bytes)
        .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let revision = FunctionRevisionRecord::new(
        checked.function_id(),
        checked.revision_id(),
        1,
        function_origin,
        declaration_content_hash,
        semantic_hash,
        orna_artifact::client_plan::LANGUAGE_VERSION_IDENTITY,
        artifact,
    )
    .map_err(|source| StandardLibraryCheckError::Revision { source })?
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    let expected = StandardExecutable::new(checked.function_id(), revision, references)
        .map_err(|source| StandardLibraryCheckError::Revision { source })?;
    if stored != &expected {
        return Err(StandardLibraryCheckError::ExecutableMismatch);
    }
    checked_standard_executable_from_record(&expected, catalogue, origins, schema_origin)
}
fn reconcile_standard_window_executable(
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    stored: &StandardExecutable,
    stored_unit: &StoredSourceUnit,
    parsed_unit: &ParsedSourceUnit,
    schema_origin: SourceOrigin,
) -> Result<CheckedStandardExecutable, StandardLibraryCheckError> {
    if parsed_unit.source_text() != stored_unit.content()
        || parsed_unit.source_text() != parsed_unit.syntax_text()
        || !parsed_unit.parsed().schemas().is_empty()
        || !parsed_unit.parsed().object_types().is_empty()
        || !parsed_unit.parsed().enum_types().is_empty()
        || !parsed_unit.parsed().primitive_value_types().is_empty()
        || !parsed_unit.parsed().opaque_value_types().is_empty()
        || !parsed_unit.parsed().record_value_types().is_empty()
        || !parsed_unit.parsed().field_renames().is_empty()
        || !parsed_unit.parsed().server_functions().is_empty()
        || parsed_unit.parsed().client_functions().len() != 1
        || !parsed_unit.parsed().type_exports().is_empty()
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let [declaration] = parsed_unit.parsed().client_functions() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let checked = check_standard_ui_window(declaration, catalogue, origins)?;
    let function_origin = origins
        .iter()
        .find(|origin| origin.identity() == DefinitionIdentity::Function(STD_UI_WINDOW_FUNCTION_ID))
        .ok_or(StandardLibraryCheckError::MissingFunctionOrigin)?
        .source();
    let source_origin = |span: &SourceSpan| -> Result<SourceOrigin, StandardLibraryCheckError> {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
        let end = u32::try_from(span.end).map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
        SourceOrigin::new(stored_unit.id(), start, end)
            .map_err(|_| StandardLibraryCheckError::SourceMismatch)
    };
    let title_type_origin = source_origin(declaration.parameters[0].type_specification.span())?;
    let content_type_origin = source_origin(declaration.parameters[1].type_specification.span())?;
    let result_type = match &declaration.return_type {
        FunctionReturnType::Single(result) => result,
        FunctionReturnType::Rows { .. } | FunctionReturnType::Stream { .. } => {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
    };
    let result_type_origin = source_origin(result_type.span())?;
    let references = vec![
        DefinitionReference::new(
            checked.function_id(),
            checked.revision_id(),
            0,
            DefinitionReferenceTarget::ValueType(STD_CHARACTER_LARGE_OBJECT_TYPE_ID),
            DefinitionReferenceKind::NamedType,
            title_type_origin,
        ),
        DefinitionReference::new(
            checked.function_id(),
            checked.revision_id(),
            1,
            DefinitionReferenceTarget::ValueType(STD_UI_TYPE_ID),
            DefinitionReferenceKind::NamedType,
            content_type_origin,
        ),
        DefinitionReference::new(
            checked.function_id(),
            checked.revision_id(),
            2,
            DefinitionReferenceTarget::ValueType(STD_UI_TYPE_ID),
            DefinitionReferenceKind::NamedType,
            result_type_origin,
        ),
    ];
    let plan = ExpressionClientPlan::new(ClientExpressionNode::ExternalContract {
        identity: STD_UI_WINDOW_RUNTIME_CONTRACT.to_owned(),
    });
    let payload = plan
        .encode()
        .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let artifact_hash = artifact_payload_digest(&payload)
        .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        CLIENT_PLAN_FORMAT,
        plan.format_version(),
        payload,
        artifact_hash,
    )
    .map_err(|source| StandardLibraryCheckError::Revision { source })?;
    let function = catalogue
        .function_by_id(STD_UI_WINDOW_FUNCTION_ID)
        .ok_or(StandardLibraryCheckError::MissingFunction)?;
    let semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        function,
        orna_artifact::client_plan::LANGUAGE_VERSION_IDENTITY,
        &artifact,
        &[],
        &references,
    )
    .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let declaration_bytes = &stored_unit.content().as_bytes()
        [function_origin.byte_start() as usize..function_origin.byte_end() as usize];
    let declaration_content_hash = function_declaration_digest(declaration_bytes)
        .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let revision = FunctionRevisionRecord::new(
        checked.function_id(),
        checked.revision_id(),
        STD_UI_WINDOW_REVISION_NUMBER,
        function_origin,
        declaration_content_hash,
        semantic_hash,
        orna_artifact::client_plan::LANGUAGE_VERSION_IDENTITY,
        artifact,
    )
    .map_err(|source| StandardLibraryCheckError::Revision { source })?
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    let expected = StandardExecutable::new(checked.function_id(), revision, references)
        .map_err(|source| StandardLibraryCheckError::Revision { source })?;
    if stored != &expected {
        return Err(StandardLibraryCheckError::ExecutableMismatch);
    }
    let mut checked_executable =
        checked_standard_executable_from_record(&expected, catalogue, origins, schema_origin)?;
    checked_executable.parameter_ids =
        vec![checked.title_parameter_id(), checked.content_parameter_id()];
    Ok(checked_executable)
}

/// Scopes one standard catalogue to the declarations retained in one source
/// unit, dropping the excluded schemas and value types and every type binding
/// that targets an excluded value type.
///
/// The returned scope carries no functions; the invoke path reconciles the
/// executable functions separately. Object, enum, and record value types are
/// not part of any retained standard source and fail closed.
fn scope_standard_catalogue(
    catalogue: &CatalogueSnapshot,
    excluded_schemas: &[SchemaId],
    excluded_value_types: &[TypeId],
) -> Result<CatalogueSnapshot, StandardLibraryCheckError> {
    if !catalogue.object_types().is_empty()
        || !catalogue.enum_types().is_empty()
        || !catalogue.record_value_types().is_empty()
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let mut schemas = Vec::with_capacity(catalogue.schemas().len());
    for schema in catalogue.schemas() {
        if excluded_schemas.contains(&schema.id()) {
            continue;
        }
        schemas.push(schema.clone());
    }
    let mut value_types = Vec::with_capacity(catalogue.value_types().len());
    for value_type in catalogue.value_types() {
        if excluded_value_types.contains(&value_type.id()) {
            continue;
        }
        value_types.push(value_type.clone());
    }
    let mut type_bindings = Vec::with_capacity(catalogue.type_bindings().len());
    for binding in catalogue.type_bindings() {
        if excluded_value_types.contains(&binding.target()) {
            continue;
        }
        type_bindings.push(binding.clone());
    }
    CatalogueSnapshot::new_with_functions_and_types(
        catalogue.revision(),
        schemas,
        vec![],
        value_types,
        type_bindings,
        vec![],
    )
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)
}

/// Scopes the V3 catalogue to the declarations retained in `std/types.orna`:
/// the standard schemas, value types, and type bindings only.
///
/// The `std.invoke`, `std.terminal`, and `std.io` schemas, the two opaque
/// output value types, and their exports are declared in the other retained
/// units and are reconciled by their own paths; the V1 type-only reconcile
/// contract must not see them. Any other catalogue schema, function, object,
/// enum, or record type fails closed.
fn standard_v3_types_catalogue(
    catalogue: &CatalogueSnapshot,
) -> Result<CatalogueSnapshot, StandardLibraryCheckError> {
    scope_standard_catalogue(
        catalogue,
        &[
            STD_INVOKE_SCHEMA_ID,
            STD_TERMINAL_SCHEMA_ID,
            STD_IO_SCHEMA_ID,
        ],
        &[STD_TERMINAL_DOCUMENT_TYPE_ID, STD_IO_BYTE_STREAM_TYPE_ID],
    )
}

/// Scopes the V4 catalogue to the declarations retained in `std/types.orna`:
/// the standard schemas, value types, and type bindings only.
///
/// The `std.invoke`, `std.terminal`, `std.io`, and `std.ui` schemas, the
/// three opaque output and ui value types, and their exports are declared in
/// the other retained units and are reconciled by their own paths; the V1
/// type-only reconcile contract must not see them. Any other catalogue
/// schema, function, object, enum, or record type fails closed.
fn standard_v4_types_catalogue(
    catalogue: &CatalogueSnapshot,
) -> Result<CatalogueSnapshot, StandardLibraryCheckError> {
    scope_standard_catalogue(
        catalogue,
        &[
            STD_INVOKE_SCHEMA_ID,
            STD_TERMINAL_SCHEMA_ID,
            STD_IO_SCHEMA_ID,
            STD_UI_SCHEMA_ID,
        ],
        &[
            STD_TERMINAL_DOCUMENT_TYPE_ID,
            STD_IO_BYTE_STREAM_TYPE_ID,
            STD_UI_TYPE_ID,
        ],
    )
}

/// Scopes the V5 and V6 catalogues to declarations retained in `std/types.orna`.
/// The JSON and action schemas and value types are reconciled in their own units.
fn standard_v5_types_catalogue(
    catalogue: &CatalogueSnapshot,
) -> Result<CatalogueSnapshot, StandardLibraryCheckError> {
    scope_standard_catalogue(
        catalogue,
        &[
            STD_INVOKE_SCHEMA_ID,
            STD_TERMINAL_SCHEMA_ID,
            STD_IO_SCHEMA_ID,
            STD_UI_SCHEMA_ID,
            STD_JSON_SCHEMA_ID,
            STD_ACTION_SCHEMA_ID,
        ],
        &[
            STD_TERMINAL_DOCUMENT_TYPE_ID,
            STD_IO_BYTE_STREAM_TYPE_ID,
            STD_UI_TYPE_ID,
            STD_JSON_VALUE_TYPE_ID,
            STD_ACTION_TYPE_ID,
        ],
    )
}

/// Reconciles the retained `std/output.orna` unit against the snapshot
/// catalogue and origins.
///
/// The unit must round-trip exactly and contain nothing besides the
/// `std.terminal` and `std.io` schema declarations, the two opaque output
/// value type declarations (`std.terminal.Document` `...15` and
/// `std.io.ByteStream` `...16`, both with their ADR 0058 kernel contracts and
/// their `IMMUTABLE TRANSIENT` catalogue facts), and their two qualified
/// exports (`std.Document`, `std.ByteStream`). Every catalogue definition
/// must sit at the fixed identity and agree with the declaration, and the
/// snapshot origins must cover exactly those six declarations at their exact
/// byte ranges on the retained unit; any extra, missing, or mismatched
/// declaration, identity, contract, binding, or origin fails closed.
fn reconcile_standard_output_unit(
    stored_unit: &StoredSourceUnit,
    parsed_unit: &ParsedSourceUnit,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
) -> Result<(), StandardLibraryCheckError> {
    if parsed_unit.source_text() != stored_unit.content()
        || parsed_unit.source_text() != parsed_unit.syntax_text()
        || !parsed_unit.parsed().object_types().is_empty()
        || !parsed_unit.parsed().enum_types().is_empty()
        || !parsed_unit.parsed().primitive_value_types().is_empty()
        || !parsed_unit.parsed().record_value_types().is_empty()
        || !parsed_unit.parsed().field_renames().is_empty()
        || !parsed_unit.parsed().server_functions().is_empty()
        || !parsed_unit.parsed().client_functions().is_empty()
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let [terminal_declaration, io_declaration] = parsed_unit.parsed().schemas() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let [document_declaration, bytestream_declaration] = parsed_unit.parsed().opaque_value_types()
    else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let [document_export, bytestream_export] = parsed_unit.parsed().type_exports() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };

    let expected_terminal_name = QualifiedSemanticName::new(["std", "terminal"])
        .expect("the fixed standard schema is valid");
    let expected_io_name =
        QualifiedSemanticName::new(["std", "io"]).expect("the fixed standard schema is valid");
    let terminal_name = unquoted_semantic_name(&terminal_declaration.name)?;
    let io_name = unquoted_semantic_name(&io_declaration.name)?;
    if terminal_name != expected_terminal_name || io_name != expected_io_name {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let terminal = catalogue
        .schema_by_id(STD_TERMINAL_SCHEMA_ID)
        .ok_or(StandardLibraryCheckError::MissingSchema)?;
    let io = catalogue
        .schema_by_id(STD_IO_SCHEMA_ID)
        .ok_or(StandardLibraryCheckError::MissingSchema)?;
    if terminal.name() != &terminal_name || io.name() != &io_name {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let expected_document_name = QualifiedSemanticName::new(["std", "terminal", "document"])
        .expect("the fixed standard value type is valid");
    let expected_bytestream_name = QualifiedSemanticName::new(["std", "io", "bytestream"])
        .expect("the fixed standard value type is valid");
    let document_name = unquoted_semantic_name(&document_declaration.name)?;
    let bytestream_name = unquoted_semantic_name(&bytestream_declaration.name)?;
    if document_name != expected_document_name || bytestream_name != expected_bytestream_name {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let document_contract = decode_string_literal(&document_declaration.kernel_contract)
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let bytestream_contract = decode_string_literal(&bytestream_declaration.kernel_contract)
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let document = catalogue
        .value_type_by_id(STD_TERMINAL_DOCUMENT_TYPE_ID)
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let bytestream = catalogue
        .value_type_by_id(STD_IO_BYTE_STREAM_TYPE_ID)
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    for (name, contract, definition) in [
        (&document_name, &document_contract, document),
        (&bytestream_name, &bytestream_contract, bytestream),
    ] {
        if definition.name() != name
            || definition.kind() != ValueTypeKind::Opaque
            || definition.mutability() != ValueTypeMutability::Immutable
            || definition.persistence() != ValueTypePersistence::Transient
            || definition.representation_contract() != contract
        {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
    }

    let expected_document_binding_name = QualifiedSemanticName::new(["std", "document"])
        .expect("the fixed standard export is valid");
    let expected_bytestream_binding_name = QualifiedSemanticName::new(["std", "bytestream"])
        .expect("the fixed standard export is valid");
    let document_binding = catalogue
        .type_binding_by_name(&TypeLookupName::qualified(
            expected_document_binding_name.clone(),
        ))
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let bytestream_binding = catalogue
        .type_binding_by_name(&TypeLookupName::qualified(
            expected_bytestream_binding_name.clone(),
        ))
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let document_export_source = unquoted_semantic_name(&document_export.source_type)?;
    let bytestream_export_source = unquoted_semantic_name(&bytestream_export.source_type)?;
    if document_export_source != document_name || bytestream_export_source != bytestream_name {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    for (export, binding, expected_name, expected_target) in [
        (
            document_export,
            document_binding,
            &expected_document_binding_name,
            STD_TERMINAL_DOCUMENT_TYPE_ID,
        ),
        (
            bytestream_export,
            bytestream_binding,
            &expected_bytestream_binding_name,
            STD_IO_BYTE_STREAM_TYPE_ID,
        ),
    ] {
        let TypeExportTarget::Qualified { name } = &export.target else {
            return Err(StandardLibraryCheckError::SourceMismatch);
        };
        if unquoted_semantic_name(name)? != *expected_name
            || !matches!(binding.kind(), TypeBindingKind::Qualified)
            || binding.name() != &TypeLookupName::qualified(expected_name.clone())
            || binding.target() != expected_target
        {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
    }

    let mut origins_by_identity = origin_map(origins)?;
    for (identity, span) in [
        (
            DefinitionIdentity::Schema(STD_TERMINAL_SCHEMA_ID),
            &terminal_declaration.span,
        ),
        (
            DefinitionIdentity::Schema(STD_IO_SCHEMA_ID),
            &io_declaration.span,
        ),
        (
            DefinitionIdentity::ValueType(STD_TERMINAL_DOCUMENT_TYPE_ID),
            &document_declaration.span,
        ),
        (
            DefinitionIdentity::ValueType(STD_IO_BYTE_STREAM_TYPE_ID),
            &bytestream_declaration.span,
        ),
        (
            DefinitionIdentity::TypeBinding(document_binding.id()),
            &document_export.span,
        ),
        (
            DefinitionIdentity::TypeBinding(bytestream_binding.id()),
            &bytestream_export.span,
        ),
    ] {
        take_origin(&mut origins_by_identity, identity, stored_unit.id(), span)?;
    }
    if !origins_by_identity.is_empty() {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    Ok(())
}

/// Reconciles the retained `std/ui.orna` unit against the snapshot catalogue
/// and origins.
///
/// The unit must round-trip exactly and contain nothing besides the
/// `std.ui` schema declaration, the single opaque ui value type declaration
/// (`std.ui.UI` `...19`, with its ADR 0062 kernel contract
/// `orna.std.value.ui@1` and its `IMMUTABLE TRANSIENT` catalogue facts), and
/// the single qualified export (std.UI). Every catalogue definition must sit
/// at the fixed identity and agree with the declaration, and the snapshot
/// origins must cover exactly those three declarations at their exact byte
/// ranges on the retained unit; any extra, missing, or mismatched
/// declaration, identity, contract, binding, or origin fails closed.
fn reconcile_standard_ui_unit(
    stored_unit: &StoredSourceUnit,
    parsed_unit: &ParsedSourceUnit,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
) -> Result<(), StandardLibraryCheckError> {
    if parsed_unit.source_text() != stored_unit.content()
        || parsed_unit.source_text() != parsed_unit.syntax_text()
        || !parsed_unit.parsed().object_types().is_empty()
        || !parsed_unit.parsed().enum_types().is_empty()
        || !parsed_unit.parsed().primitive_value_types().is_empty()
        || !parsed_unit.parsed().record_value_types().is_empty()
        || !parsed_unit.parsed().field_renames().is_empty()
        || !parsed_unit.parsed().server_functions().is_empty()
        || !parsed_unit.parsed().client_functions().is_empty()
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let [ui_declaration] = parsed_unit.parsed().schemas() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let [ui_type_declaration] = parsed_unit.parsed().opaque_value_types() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let [ui_export] = parsed_unit.parsed().type_exports() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };

    let expected_ui_name =
        QualifiedSemanticName::new(["std", "ui"]).expect("the fixed standard schema is valid");
    let ui_name = unquoted_semantic_name(&ui_declaration.name)?;
    if ui_name != expected_ui_name {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let ui_schema = catalogue
        .schema_by_id(STD_UI_SCHEMA_ID)
        .ok_or(StandardLibraryCheckError::MissingSchema)?;
    if ui_schema.name() != &ui_name {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let expected_ui_type_name = QualifiedSemanticName::new(["std", "ui", "ui"])
        .expect("the fixed standard value type is valid");
    let ui_type_name = unquoted_semantic_name(&ui_type_declaration.name)?;
    if ui_type_name != expected_ui_type_name {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let ui_contract = decode_string_literal(&ui_type_declaration.kernel_contract)
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    if ui_contract != STD_UI_CONTRACT {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let ui_definition = catalogue
        .value_type_by_id(STD_UI_TYPE_ID)
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    if ui_definition.name() != &ui_type_name
        || ui_definition.kind() != ValueTypeKind::Opaque
        || ui_definition.mutability() != ValueTypeMutability::Immutable
        || ui_definition.persistence() != ValueTypePersistence::Transient
        || ui_definition.representation_contract() != ui_contract
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let expected_ui_binding_name =
        QualifiedSemanticName::new(["std", "ui"]).expect("the fixed standard export is valid");
    let ui_binding = catalogue
        .type_binding_by_name(&TypeLookupName::qualified(expected_ui_binding_name.clone()))
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let ui_export_source = unquoted_semantic_name(&ui_export.source_type)?;
    if ui_export_source != ui_type_name {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let TypeExportTarget::Qualified { name } = &ui_export.target else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    if unquoted_semantic_name(name)? != expected_ui_binding_name
        || !matches!(ui_binding.kind(), TypeBindingKind::Qualified)
        || ui_binding.name() != &TypeLookupName::qualified(expected_ui_binding_name.clone())
        || ui_binding.target() != STD_UI_TYPE_ID
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let mut origins_by_identity = origin_map(origins)?;
    for (identity, span) in [
        (
            DefinitionIdentity::Schema(STD_UI_SCHEMA_ID),
            &ui_declaration.span,
        ),
        (
            DefinitionIdentity::ValueType(STD_UI_TYPE_ID),
            &ui_type_declaration.span,
        ),
        (
            DefinitionIdentity::TypeBinding(ui_binding.id()),
            &ui_export.span,
        ),
    ] {
        take_origin(&mut origins_by_identity, identity, stored_unit.id(), span)?;
    }
    if !origins_by_identity.is_empty() {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    Ok(())
}
/// Reconciles the retained `std/json.orna` unit against the V5 catalogue and
/// origins. The unit contains the JSON schema, opaque value type, export, and
/// the existing closed `std.json.encode` presenter.
fn reconcile_standard_json_unit(
    stored_unit: &StoredSourceUnit,
    parsed_unit: &ParsedSourceUnit,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
) -> Result<(), StandardLibraryCheckError> {
    if parsed_unit.source_text() != stored_unit.content()
        || parsed_unit.source_text() != parsed_unit.syntax_text()
        || !parsed_unit.parsed().object_types().is_empty()
        || !parsed_unit.parsed().enum_types().is_empty()
        || !parsed_unit.parsed().primitive_value_types().is_empty()
        || !parsed_unit.parsed().record_value_types().is_empty()
        || !parsed_unit.parsed().field_renames().is_empty()
        || !parsed_unit.parsed().client_functions().is_empty()
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let [json_declaration] = parsed_unit.parsed().schemas() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let [json_type_declaration] = parsed_unit.parsed().opaque_value_types() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let [json_export] = parsed_unit.parsed().type_exports() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let [json_function] = parsed_unit.parsed().server_functions() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };

    let expected_json_name =
        QualifiedSemanticName::new(["std", "json"]).expect("the fixed standard schema is valid");
    let json_name = unquoted_semantic_name(&json_declaration.name)?;
    if json_name != expected_json_name {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let json_schema = catalogue
        .schema_by_id(STD_JSON_SCHEMA_ID)
        .ok_or(StandardLibraryCheckError::MissingSchema)?;
    if json_schema.name() != &json_name {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let expected_json_type_name = QualifiedSemanticName::new(["std", "json", "value"])
        .expect("the fixed standard value type is valid");
    let json_type_name = unquoted_semantic_name(&json_type_declaration.name)?;
    if json_type_name != expected_json_type_name {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let json_contract = decode_string_literal(&json_type_declaration.kernel_contract)
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    if json_contract != STD_JSON_CONTRACT {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let json_definition = catalogue
        .value_type_by_id(STD_JSON_VALUE_TYPE_ID)
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    if json_definition.name() != &json_type_name
        || json_definition.kind() != ValueTypeKind::Opaque
        || json_definition.mutability() != ValueTypeMutability::Immutable
        || json_definition.persistence() != ValueTypePersistence::Transient
        || json_definition.representation_contract() != json_contract
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let expected_json_binding_name = QualifiedSemanticName::new(["std", "jsonvalue"])
        .expect("the fixed standard export is valid");
    let json_binding = catalogue
        .type_binding_by_name(&TypeLookupName::qualified(
            expected_json_binding_name.clone(),
        ))
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let json_export_source = unquoted_semantic_name(&json_export.source_type)?;
    let TypeExportTarget::Qualified { name } = &json_export.target else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    if json_export_source != json_type_name
        || unquoted_semantic_name(name)? != expected_json_binding_name
        || !matches!(json_binding.kind(), TypeBindingKind::Qualified)
        || json_binding.name() != &TypeLookupName::qualified(expected_json_binding_name.clone())
        || json_binding.target() != STD_JSON_VALUE_TYPE_ID
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    check_standard_json_encode(json_function, catalogue, origins, STD_JSON_VALUE_TYPE_ID)?;

    let mut origins_by_identity = HashMap::with_capacity(origins.len());
    for origin in origins {
        if !matches!(
            origin.identity(),
            DefinitionIdentity::Schema(_)
                | DefinitionIdentity::ValueType(_)
                | DefinitionIdentity::TypeBinding(_)
                | DefinitionIdentity::Function(_)
                | DefinitionIdentity::Parameter { .. }
        ) {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
        if origins_by_identity
            .insert(origin.identity(), origin.source())
            .is_some()
        {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
    }
    for (identity, span) in [
        (
            DefinitionIdentity::Schema(STD_JSON_SCHEMA_ID),
            &json_declaration.span,
        ),
        (
            DefinitionIdentity::ValueType(STD_JSON_VALUE_TYPE_ID),
            &json_type_declaration.span,
        ),
        (
            DefinitionIdentity::TypeBinding(json_binding.id()),
            &json_export.span,
        ),
        (
            DefinitionIdentity::Function(STD_JSON_ENCODE_FUNCTION_ID),
            &json_function.span,
        ),
    ] {
        take_origin(&mut origins_by_identity, identity, stored_unit.id(), span)?;
    }
    let parameter = json_function
        .parameters
        .first()
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    take_origin(
        &mut origins_by_identity,
        DefinitionIdentity::Parameter {
            owner: STD_JSON_ENCODE_FUNCTION_ID,
            parameter: STD_JSON_ENCODE_PARAMETER_ID,
        },
        stored_unit.id(),
        &parameter.span,
    )?;
    if !origins_by_identity.is_empty() {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    Ok(())
}

/// Reconciles the retained `std/action.orna` unit against the V6 catalogue.
fn reconcile_standard_action_unit(
    stored_unit: &StoredSourceUnit,
    parsed_unit: &ParsedSourceUnit,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
) -> Result<StandardSourceFamilies, StandardLibraryCheckError> {
    if parsed_unit.source_text() != stored_unit.content()
        || parsed_unit.source_text() != parsed_unit.syntax_text()
        || !parsed_unit.parsed().object_types().is_empty()
        || !parsed_unit.parsed().enum_types().is_empty()
        || !parsed_unit.parsed().primitive_value_types().is_empty()
        || !parsed_unit.parsed().record_value_types().is_empty()
        || !parsed_unit.parsed().field_renames().is_empty()
        || !parsed_unit.parsed().server_functions().is_empty()
        || !parsed_unit.parsed().client_functions().is_empty()
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let [action_schema_declaration] = parsed_unit.parsed().schemas() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let [action_type_declaration] = parsed_unit.parsed().opaque_value_types() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let [action_export] = parsed_unit.parsed().type_exports() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };

    let expected_schema_name =
        QualifiedSemanticName::new(["std", "action"]).expect("the fixed action schema is valid");
    let schema_name = unquoted_semantic_name(&action_schema_declaration.name)?;
    if schema_name != expected_schema_name
        || catalogue
            .schema_by_id(STD_ACTION_SCHEMA_ID)
            .ok_or(StandardLibraryCheckError::MissingSchema)?
            .name()
            != &schema_name
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let expected_type_name = QualifiedSemanticName::new(["std", "action", "action"])
        .expect("the fixed action value type is valid");
    let type_name = unquoted_semantic_name(&action_type_declaration.name)?;
    let contract = decode_string_literal(&action_type_declaration.kernel_contract)
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let action_type = catalogue
        .value_type_by_id(STD_ACTION_TYPE_ID)
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    if type_name != expected_type_name
        || action_type.name() != &type_name
        || action_type.kind() != ValueTypeKind::Opaque
        || action_type.mutability() != ValueTypeMutability::Immutable
        || action_type.persistence() != ValueTypePersistence::Transient
        || contract != STD_ACTION_CONTRACT
        || action_type.representation_contract() != contract
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let expected_binding_name =
        QualifiedSemanticName::new(["std", "action"]).expect("the fixed action export is valid");
    let binding = catalogue
        .type_binding_by_name(&TypeLookupName::qualified(expected_binding_name.clone()))
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let TypeExportTarget::Qualified { name: target_name } = &action_export.target else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    if unquoted_semantic_name(&action_export.source_type)? != type_name
        || unquoted_semantic_name(target_name)? != expected_binding_name
        || !matches!(binding.kind(), TypeBindingKind::Qualified)
        || binding.name() != &TypeLookupName::qualified(expected_binding_name)
        || binding.target() != STD_ACTION_TYPE_ID
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let mut origins_by_identity = origin_map(origins)?;

    let schema_origin = take_origin(
        &mut origins_by_identity,
        DefinitionIdentity::Schema(STD_ACTION_SCHEMA_ID),
        stored_unit.id(),
        &action_schema_declaration.span,
    )?;
    let value_type_origin = take_origin(
        &mut origins_by_identity,
        DefinitionIdentity::ValueType(STD_ACTION_TYPE_ID),
        stored_unit.id(),
        &action_type_declaration.span,
    )?;
    let binding_origin = take_origin(
        &mut origins_by_identity,
        DefinitionIdentity::TypeBinding(binding.id()),
        stored_unit.id(),
        &action_export.span,
    )?;
    if !origins_by_identity.is_empty() {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    Ok(StandardSourceFamilies {
        schemas: vec![CheckedStandardSchema {
            id: STD_ACTION_SCHEMA_ID,
            name: schema_name,
            origin: schema_origin,
        }],
        value_types: vec![CheckedStandardValueType {
            id: STD_ACTION_TYPE_ID,
            name: type_name,
            kind: action_type.kind(),
            mutability: action_type.mutability(),
            persistence: action_type.persistence(),
            representation_contract: action_type.representation_contract().to_owned(),
            origin: value_type_origin,
        }],
        type_bindings: vec![CheckedStandardTypeBinding {
            id: binding.id(),
            kind: binding.kind(),
            name: binding.name().clone(),
            target: binding.target(),
            origin: binding_origin,
        }],
    })
}

/// Splits the snapshot origins into the `std/types.orna` origins (schemas,
/// value types, and bindings) and the `std/invoke.orna` origins (the
/// `std.invoke` schema, the `std.invoke.echo` function, and its parameter).
///
/// Every origin must belong to one of the two retained V2 units; any other
/// source unit fails closed.
fn partition_standard_origins(
    origins: &[DefinitionOrigin],
) -> Result<(Vec<DefinitionOrigin>, Vec<DefinitionOrigin>), StandardLibraryCheckError> {
    let mut types_origins = Vec::new();
    let mut invoke_origins = Vec::new();
    for origin in origins {
        let source_unit = origin.source().source_unit();
        if source_unit == STD_TYPES_SOURCE_UNIT_ID {
            types_origins.push(origin.clone());
        } else if source_unit == STD_INVOKE_SOURCE_UNIT_ID {
            invoke_origins.push(origin.clone());
        } else {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
    }
    Ok((types_origins, invoke_origins))
}

/// The three ordered origin partitions of a V3 standard bundle: the types,
/// invoke, and output unit origins.
type StandardV3OriginPartitions = (
    Vec<DefinitionOrigin>,
    Vec<DefinitionOrigin>,
    Vec<DefinitionOrigin>,
);

/// Splits the snapshot origins into the three retained V3 units: the
/// `std/types.orna` origins, the `std/invoke.orna` origins, and the
/// `std/output.orna` origins (the two output schemas, the two opaque output
/// value types, and their two exports).
///
/// Every origin must belong to one of the three retained V3 units; any other
/// source unit fails closed.
fn partition_standard_v3_origins(
    origins: &[DefinitionOrigin],
) -> Result<StandardV3OriginPartitions, StandardLibraryCheckError> {
    let mut types_origins = Vec::new();
    let mut invoke_origins = Vec::new();
    let mut output_origins = Vec::new();
    for origin in origins {
        let source_unit = origin.source().source_unit();
        if source_unit == STD_TYPES_SOURCE_UNIT_ID {
            types_origins.push(origin.clone());
        } else if source_unit == STD_INVOKE_SOURCE_UNIT_ID {
            invoke_origins.push(origin.clone());
        } else if source_unit == STD_OUTPUT_SOURCE_UNIT_ID {
            output_origins.push(origin.clone());
        } else {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
    }
    Ok((types_origins, invoke_origins, output_origins))
}

/// The four ordered origin partitions of a V4 standard bundle: the types,
/// invoke, output, and ui unit origins.
type StandardV4OriginPartitions = (
    Vec<DefinitionOrigin>,
    Vec<DefinitionOrigin>,
    Vec<DefinitionOrigin>,
    Vec<DefinitionOrigin>,
);

/// Splits the snapshot origins into the four retained V4 units: the
/// `std/types.orna` origins, the `std/invoke.orna` origins, the
/// `std/output.orna` origins, and the `std/ui.orna` origins (the ui schema,
/// the opaque ui value type, and its export).
///
/// Every origin must belong to one of the four retained V4 units; any other
/// source unit fails closed.
fn partition_standard_v4_origins(
    origins: &[DefinitionOrigin],
) -> Result<StandardV4OriginPartitions, StandardLibraryCheckError> {
    let mut types_origins = Vec::new();
    let mut invoke_origins = Vec::new();
    let mut output_origins = Vec::new();
    let mut ui_origins = Vec::new();
    for origin in origins {
        let source_unit = origin.source().source_unit();
        if source_unit == STD_TYPES_SOURCE_UNIT_ID {
            types_origins.push(origin.clone());
        } else if source_unit == STD_INVOKE_SOURCE_UNIT_ID {
            invoke_origins.push(origin.clone());
        } else if source_unit == STD_OUTPUT_SOURCE_UNIT_ID {
            output_origins.push(origin.clone());
        } else if source_unit == STD_UI_SOURCE_UNIT_ID {
            ui_origins.push(origin.clone());
        } else {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
    }
    Ok((types_origins, invoke_origins, output_origins, ui_origins))
}

/// Checks one parsed declaration against the closed ADR 0055 standard
/// parameter-echo source shape.
///
/// The checker accepts ONLY the exact `std.invoke.echo` shape: a SERVER
/// function named `std.invoke.echo` with exactly one required non-null
/// `p_value INTEGER` parameter (no default expression; the grammar has no
/// nullable parameter spelling, so required non-null is the only form), one
/// single `INTEGER` result (never `ROWS`), `SECURITY INVOKER`,
/// `TRANSACTION READ ONLY`, `VOLATILITY STABLE`, zero capability clauses, and
/// the closed no-input `SELECT p_value` body. It rejects every other name,
/// parameter count or name, default, type, result shape, security,
/// transaction, volatility, capability, and body variation before any
/// artifact is constructed.
///
/// The supplied catalogue must contain the fixed identities: the `std.invoke`
/// schema, the `std.invoke.echo` function, and its `p_value` parameter. Both
/// written `INTEGER` spellings must resolve through the catalogue to
/// `integer_type_id`, which therefore must hold a value type at that identity.
/// The supplied origins must contain the fixed function and parameter
/// declaration origins; the reference source origins reuse the retained
/// source unit from the function origin and the exact byte ranges of the
/// `INTEGER`, `INTEGER`, and `p_value` tokens in the declaration.
///
/// Step 6 (`feat(compiler): reconcile executable standard source`) wires this
/// checker into the standard source checker and consumes the returned facts
/// to build the `StandardExecutable` record: the fixed function identity, the
/// version-1 revision identity, the 44-byte `orna.server-parameter-echo`
/// artifact, and the three ordered durable references.
pub fn check_standard_parameter_echo(
    declaration: &ServerFunctionDeclaration,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    integer_type_id: TypeId,
) -> Result<CheckedStandardParameterEcho, StandardLibraryCheckError> {
    let expected_name = QualifiedSemanticName::new(["std", "invoke", "echo"])
        .expect("the fixed standard function name is valid");
    let name = semantic_name(&declaration.name);
    if name != expected_name {
        return Err(StandardLibraryCheckError::UnexpectedName { actual: name });
    }

    if declaration.parameters.len() != 1 {
        return Err(StandardLibraryCheckError::UnexpectedParameterCount {
            actual: declaration.parameters.len(),
        });
    }
    let parameter = &declaration.parameters[0];
    let parameter_name = semantic_part(&parameter.name);
    if parameter_name != "p_value" {
        return Err(StandardLibraryCheckError::UnexpectedParameterName {
            actual: parameter_name,
        });
    }
    if parameter.default_expression.is_some() {
        return Err(StandardLibraryCheckError::ParameterDefault);
    }
    if resolved_standard_type_id(&parameter.type_specification, catalogue) != Some(integer_type_id)
    {
        return Err(StandardLibraryCheckError::UnexpectedParameterType);
    }
    let parameter_type_span = parameter.type_specification.span();

    let FunctionReturnType::Single(result_specification) = &declaration.return_type else {
        return Err(StandardLibraryCheckError::UnexpectedResultShape);
    };
    if resolved_standard_type_id(result_specification, catalogue) != Some(integer_type_id) {
        return Err(StandardLibraryCheckError::UnexpectedResultType);
    }
    let result_type_span = result_specification.span();

    let security = declaration
        .security
        .ok_or(StandardLibraryCheckError::MissingSecurity)?;
    if security != SyntaxFunctionSecurity::Invoker {
        return Err(StandardLibraryCheckError::UnexpectedSecurity { actual: security });
    }
    let transaction = declaration
        .transaction
        .ok_or(StandardLibraryCheckError::MissingTransaction)?;
    if transaction != SyntaxFunctionTransaction::ReadOnly {
        return Err(StandardLibraryCheckError::UnexpectedTransaction {
            actual: transaction,
        });
    }
    let volatility = declaration
        .volatility
        .ok_or(StandardLibraryCheckError::MissingVolatility)?;
    if volatility != SyntaxFunctionVolatility::Stable {
        return Err(StandardLibraryCheckError::UnexpectedVolatility { actual: volatility });
    }
    if !declaration.capabilities.is_empty() {
        return Err(StandardLibraryCheckError::CapabilityClause);
    }

    let body = declaration
        .body
        .as_no_input_parameter_select()
        .ok_or(StandardLibraryCheckError::UnexpectedBody)?;
    let body_identifier = semantic_part(&body.parameter);
    if body_identifier != "p_value" {
        return Err(StandardLibraryCheckError::UnexpectedBodyIdentifier {
            actual: body_identifier,
        });
    }
    let body_identifier_span = &body.parameter.span;

    let expected_schema_name =
        QualifiedSemanticName::new(["std", "invoke"]).expect("the fixed standard schema is valid");
    let schema = catalogue
        .schema_by_id(STD_INVOKE_SCHEMA_ID)
        .ok_or(StandardLibraryCheckError::MissingSchema)?;
    if schema.name() != &expected_schema_name {
        return Err(StandardLibraryCheckError::SchemaNameMismatch {
            actual: schema.name().clone(),
        });
    }
    let function = catalogue
        .function_by_id(STD_INVOKE_ECHO_FUNCTION_ID)
        .ok_or(StandardLibraryCheckError::MissingFunction)?;
    if function.name() != &expected_name {
        return Err(StandardLibraryCheckError::FunctionNameMismatch {
            actual: function.name().clone(),
        });
    }
    let parameter_definition = function
        .parameter_by_id(STD_INVOKE_ECHO_PARAMETER_ID)
        .ok_or(StandardLibraryCheckError::MissingParameter)?;
    if parameter_definition.name() != "p_value" {
        return Err(StandardLibraryCheckError::ParameterNameMismatch {
            actual: parameter_definition.name().to_owned(),
        });
    }

    let function_origin = origins
        .iter()
        .find(|origin| {
            origin.identity() == DefinitionIdentity::Function(STD_INVOKE_ECHO_FUNCTION_ID)
        })
        .ok_or(StandardLibraryCheckError::MissingFunctionOrigin)?;
    let parameter_origin = origins
        .iter()
        .find(|origin| {
            origin.identity()
                == DefinitionIdentity::Parameter {
                    owner: STD_INVOKE_ECHO_FUNCTION_ID,
                    parameter: STD_INVOKE_ECHO_PARAMETER_ID,
                }
        })
        .ok_or(StandardLibraryCheckError::MissingParameterOrigin)?;
    if function_origin.source().source_unit() != parameter_origin.source().source_unit() {
        return Err(StandardLibraryCheckError::OriginSourceUnitMismatch);
    }
    let source_unit = function_origin.source().source_unit();

    let payload = ServerParameterEcho::new(STD_INVOKE_ECHO_PARAMETER_ID, integer_type_id)
        .map_err(|source| StandardLibraryCheckError::Artifact { source })?
        .encode()
        .map_err(|source| StandardLibraryCheckError::Artifact { source })?;
    let content_hash = artifact_payload_digest(&payload)
        .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        server_parameter_echo::FORMAT_IDENTITY,
        server_parameter_echo::FORMAT_VERSION,
        payload,
        content_hash,
    )
    .map_err(|source| StandardLibraryCheckError::Revision { source })?;

    let reference_origin = |span: &SourceSpan| -> Result<SourceOrigin, StandardLibraryCheckError> {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
        let end = u32::try_from(span.end).map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
        SourceOrigin::new(source_unit, start, end)
            .map_err(|source| StandardLibraryCheckError::Revision { source })
    };
    let references = vec![
        DefinitionReference::new(
            STD_INVOKE_ECHO_FUNCTION_ID,
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            0,
            DefinitionReferenceTarget::ValueType(integer_type_id),
            DefinitionReferenceKind::NamedType,
            reference_origin(parameter_type_span)?,
        ),
        DefinitionReference::new(
            STD_INVOKE_ECHO_FUNCTION_ID,
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            1,
            DefinitionReferenceTarget::ValueType(integer_type_id),
            DefinitionReferenceKind::NamedType,
            reference_origin(result_type_span)?,
        ),
        DefinitionReference::new(
            STD_INVOKE_ECHO_FUNCTION_ID,
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            2,
            DefinitionReferenceTarget::Parameter {
                owner: STD_INVOKE_ECHO_FUNCTION_ID,
                parameter: STD_INVOKE_ECHO_PARAMETER_ID,
            },
            DefinitionReferenceKind::ParameterRead,
            reference_origin(body_identifier_span)?,
        ),
    ];

    Ok(CheckedStandardParameterEcho {
        function_id: STD_INVOKE_ECHO_FUNCTION_ID,
        parameter_id: STD_INVOKE_ECHO_PARAMETER_ID,
        revision_id: STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        artifact,
        references,
    })
}

/// The closed expected shape of one ADR 0057 standard presenter declaration.
///
/// Both presenters share one exact-shape contract: a SERVER function with the
/// fixed qualified name in the fixed schema, exactly one required non-null
/// parameter with the fixed name and value-type identity, one single result
/// with the fixed value-type identity, `SECURITY INVOKER`, `TRANSACTION READ
/// ONLY`, `VOLATILITY STABLE`, zero capability clauses, and the closed
/// parameter-select body naming the fixed parameter. The two checkers differ
/// only in these fixed facts.
struct PresenterShape {
    /// The exact expected presenter function name.
    function_name: QualifiedSemanticName,
    /// The exact expected presenter schema name.
    schema_name: QualifiedSemanticName,
    /// The exact expected presenter parameter name.
    parameter_name: &'static str,
    /// The fixed presenter function identity.
    function_id: FunctionId,
    /// The fixed presenter parameter identity.
    parameter_id: ParameterId,
    /// The fixed version-1 function-revision identity.
    revision_id: FunctionRevisionId,
    /// The fixed presenter schema identity.
    schema_id: SchemaId,
    /// The fixed parameter value-type identity.
    parameter_type_id: TypeId,
    /// The fixed result value-type identity.
    result_type_id: TypeId,
}

/// The checked declaration facts shared by the two presenter checkers.
#[derive(Clone, Debug, Eq, PartialEq)]
struct CheckedStandardPresenter {
    function_id: FunctionId,
    parameter_id: ParameterId,
    revision_id: FunctionRevisionId,
}

/// Checks one parsed declaration against one closed ADR 0057 standard
/// presenter shape.
///
/// The checker accepts ONLY the exact presenter shape carried by [`PresenterShape`]:
/// a SERVER function with the fixed qualified name, exactly one required
/// non-null parameter with the fixed name (no default expression; the grammar
/// has no nullable parameter spelling, so required non-null is the only
/// form), one single result with the fixed value-type identity (never
/// `ROWS`), `SECURITY INVOKER`, `TRANSACTION READ ONLY`, `VOLATILITY STABLE`,
/// zero capability clauses, and the closed `SELECT <parameter>` body naming
/// the fixed parameter. It rejects every other name, parameter count or name,
/// default, type, result shape, security, transaction, volatility, capability,
/// and body variation before any artifact is constructed.
///
/// The supplied catalogue must contain the fixed identities: the presenter
/// schema, the presenter function, and its parameter, and the function must
/// be a SERVER function. Both written type spellings must resolve through the
/// catalogue to the fixed parameter and result value-type identities, which
/// therefore must hold value types at those identities. The supplied origins
/// must contain the fixed function and parameter declaration origins on the
/// same source unit.
///
/// ADR 0057 step 4 (`feat(artifact): encode terminal and json presenter
/// plans`) consumes the returned facts to construct the closed server
/// artifacts and their ordered durable references.
fn check_standard_presenter_declaration(
    declaration: &ServerFunctionDeclaration,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    shape: &PresenterShape,
) -> Result<CheckedStandardPresenter, StandardLibraryCheckError> {
    let name = semantic_name(&declaration.name);
    if name != shape.function_name {
        return Err(StandardLibraryCheckError::PresenterUnexpectedName {
            expected: shape.function_name.clone(),
            actual: name,
        });
    }

    if declaration.parameters.len() != 1 {
        return Err(
            StandardLibraryCheckError::PresenterUnexpectedParameterCount {
                actual: declaration.parameters.len(),
            },
        );
    }
    let parameter = &declaration.parameters[0];
    let parameter_name = semantic_part(&parameter.name);
    if parameter_name != shape.parameter_name {
        return Err(
            StandardLibraryCheckError::PresenterUnexpectedParameterName {
                expected: shape.parameter_name.to_owned(),
                actual: parameter_name,
            },
        );
    }
    if parameter.default_expression.is_some() {
        return Err(StandardLibraryCheckError::PresenterParameterDefault);
    }
    if resolved_standard_type_id(&parameter.type_specification, catalogue)
        != Some(shape.parameter_type_id)
    {
        return Err(
            StandardLibraryCheckError::PresenterUnexpectedParameterType {
                expected: shape.parameter_type_id,
            },
        );
    }

    let FunctionReturnType::Single(result_specification) = &declaration.return_type else {
        return Err(StandardLibraryCheckError::PresenterUnexpectedResultShape);
    };
    if resolved_standard_type_id(result_specification, catalogue) != Some(shape.result_type_id) {
        return Err(StandardLibraryCheckError::PresenterUnexpectedResultType {
            expected: shape.result_type_id,
        });
    }

    let security = declaration
        .security
        .ok_or(StandardLibraryCheckError::PresenterMissingSecurity)?;
    if security != SyntaxFunctionSecurity::Invoker {
        return Err(StandardLibraryCheckError::PresenterUnexpectedSecurity { actual: security });
    }
    let transaction = declaration
        .transaction
        .ok_or(StandardLibraryCheckError::PresenterMissingTransaction)?;
    if transaction != SyntaxFunctionTransaction::ReadOnly {
        return Err(StandardLibraryCheckError::PresenterUnexpectedTransaction {
            actual: transaction,
        });
    }
    let volatility = declaration
        .volatility
        .ok_or(StandardLibraryCheckError::PresenterMissingVolatility)?;
    if volatility != SyntaxFunctionVolatility::Stable {
        return Err(StandardLibraryCheckError::PresenterUnexpectedVolatility {
            actual: volatility,
        });
    }
    if !declaration.capabilities.is_empty() {
        return Err(StandardLibraryCheckError::PresenterCapabilityClause);
    }

    let body = declaration
        .body
        .as_no_input_parameter_select()
        .ok_or(StandardLibraryCheckError::PresenterUnexpectedBody)?;
    let body_identifier = semantic_part(&body.parameter);
    if body_identifier != shape.parameter_name {
        return Err(
            StandardLibraryCheckError::PresenterUnexpectedBodyIdentifier {
                expected: shape.parameter_name.to_owned(),
                actual: body_identifier,
            },
        );
    }

    let schema = catalogue
        .schema_by_id(shape.schema_id)
        .ok_or(StandardLibraryCheckError::PresenterMissingSchema)?;
    if schema.name() != &shape.schema_name {
        return Err(StandardLibraryCheckError::PresenterSchemaNameMismatch {
            expected: shape.schema_name.clone(),
            actual: schema.name().clone(),
        });
    }
    let function = catalogue
        .function_by_id(shape.function_id)
        .ok_or(StandardLibraryCheckError::PresenterMissingFunction)?;
    if function.name() != &shape.function_name {
        return Err(StandardLibraryCheckError::PresenterFunctionNameMismatch {
            expected: shape.function_name.clone(),
            actual: function.name().clone(),
        });
    }
    if function.domain() != FunctionDomain::Server {
        return Err(StandardLibraryCheckError::PresenterUnexpectedDomain {
            actual: function.domain(),
        });
    }
    let parameter_definition = function
        .parameter_by_id(shape.parameter_id)
        .ok_or(StandardLibraryCheckError::PresenterMissingParameter)?;
    if parameter_definition.name() != shape.parameter_name {
        return Err(StandardLibraryCheckError::PresenterParameterNameMismatch {
            expected: shape.parameter_name.to_owned(),
            actual: parameter_definition.name().to_owned(),
        });
    }

    let function_origin = origins
        .iter()
        .find(|origin| origin.identity() == DefinitionIdentity::Function(shape.function_id))
        .ok_or(StandardLibraryCheckError::PresenterMissingFunctionOrigin)?;
    let parameter_origin = origins
        .iter()
        .find(|origin| {
            origin.identity()
                == DefinitionIdentity::Parameter {
                    owner: shape.function_id,
                    parameter: shape.parameter_id,
                }
        })
        .ok_or(StandardLibraryCheckError::PresenterMissingParameterOrigin)?;
    if function_origin.source().source_unit() != parameter_origin.source().source_unit() {
        return Err(StandardLibraryCheckError::OriginSourceUnitMismatch);
    }

    Ok(CheckedStandardPresenter {
        function_id: shape.function_id,
        parameter_id: shape.parameter_id,
        revision_id: shape.revision_id,
    })
}

/// Checks one parsed declaration against the closed ADR 0057 `std.json.encode`
/// presenter shape.
///
/// The checker accepts ONLY the exact `std.json.encode` shape: a SERVER
/// function named `std.json.encode` with exactly one required non-null
/// `p_value` parameter that resolves through the catalogue to
/// `json_value_type_id`, one single result that resolves to the fixed
/// `std.io.ByteStream` value type (`...16`, ADR 0058), `SECURITY INVOKER`,
/// `TRANSACTION READ ONLY`, `VOLATILITY STABLE`, zero capability clauses, and
/// the closed `SELECT p_value` body. It rejects every other name, parameter
/// count or name, default, type, result shape, security, transaction,
/// volatility, capability, and body variation before any artifact is
/// constructed.
///
/// The supplied catalogue must contain the fixed identities: the `std.json`
/// schema, the `std.json.encode` function, and its `p_value` parameter, and
/// the function must be a SERVER function. Both written type spellings must
/// resolve through the catalogue to `json_value_type_id` and the fixed
/// `std.io.ByteStream` value type, which therefore must hold value types at
/// those identities. The supplied origins must contain the fixed function and
/// parameter declaration origins on the same source unit.
///
/// `std.json.Value` is not yet registered in `orna.std/3` (work ADR 0058
/// registered only `std.terminal.Document` and `std.io.ByteStream`), so its
/// identity is supplied by the caller exactly as ADR 0055 step 4 supplied the
/// INTEGER identity to [`check_standard_parameter_echo`].
///
/// ADR 0057 step 4 (`feat(artifact): encode terminal and json presenter
/// plans`) consumes the returned facts to construct the
/// `orna.server-json-encode` artifact and its ordered durable references.
pub fn check_standard_json_encode(
    declaration: &ServerFunctionDeclaration,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    json_value_type_id: TypeId,
) -> Result<CheckedStandardJsonEncode, StandardLibraryCheckError> {
    let shape = PresenterShape {
        function_name: QualifiedSemanticName::new(["std", "json", "encode"])
            .expect("the fixed standard function name is valid"),
        schema_name: QualifiedSemanticName::new(["std", "json"])
            .expect("the fixed standard schema is valid"),
        parameter_name: "p_value",
        function_id: STD_JSON_ENCODE_FUNCTION_ID,
        parameter_id: STD_JSON_ENCODE_PARAMETER_ID,
        revision_id: STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        schema_id: STD_JSON_SCHEMA_ID,
        parameter_type_id: json_value_type_id,
        result_type_id: STD_IO_BYTE_STREAM_TYPE_ID,
    };
    let checked = check_standard_presenter_declaration(declaration, catalogue, origins, &shape)?;
    Ok(CheckedStandardJsonEncode {
        function_id: checked.function_id,
        parameter_id: checked.parameter_id,
        revision_id: checked.revision_id,
    })
}

/// Checks one parsed declaration against the closed ADR 0057
/// `std.terminal.present_table` presenter shape.
///
/// The checker accepts ONLY the exact `std.terminal.present_table` shape: a
/// SERVER function named `std.terminal.present_table` with exactly one
/// required non-null `p_rows` parameter that resolves through the catalogue
/// to `rows_type_id`, one single result that resolves to the fixed
/// `std.terminal.Document` value type (`...15`, ADR 0058), `SECURITY
/// INVOKER`, `TRANSACTION READ ONLY`, `VOLATILITY STABLE`, zero capability
/// clauses, and the closed `SELECT p_rows` body. It rejects every other name,
/// parameter count or name, default, type, result shape, security,
/// transaction, volatility, capability, and body variation before any
/// artifact is constructed.
///
/// The supplied catalogue must contain the fixed identities: the `std.terminal`
/// schema, the `std.terminal.present_table` function, and its `p_rows`
/// parameter, and the function must be a SERVER function. Both written type
/// spellings must resolve through the catalogue to `rows_type_id` and the
/// fixed `std.terminal.Document` value type, which therefore must hold value
/// types at those identities. The supplied origins must contain the fixed
/// function and parameter declaration origins on the same source unit.
///
/// `std.data.Rows` is not yet registered in `orna.std/3` (work ADR 0058
/// registered only `std.terminal.Document` and `std.io.ByteStream`), so its
/// identity is supplied by the caller exactly as ADR 0055 step 4 supplied the
/// INTEGER identity to [`check_standard_parameter_echo`].
///
/// ADR 0057 step 4 (`feat(artifact): encode terminal and json presenter
/// plans`) consumes the returned facts to construct the
/// `orna.server-terminal-table` artifact and its ordered durable references.
pub fn check_standard_terminal_present_table(
    declaration: &ServerFunctionDeclaration,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    rows_type_id: TypeId,
) -> Result<CheckedStandardTerminalPresentTable, StandardLibraryCheckError> {
    let shape = PresenterShape {
        function_name: QualifiedSemanticName::new(["std", "terminal", "present_table"])
            .expect("the fixed standard function name is valid"),
        schema_name: QualifiedSemanticName::new(["std", "terminal"])
            .expect("the fixed standard schema is valid"),
        parameter_name: "p_rows",
        function_id: STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        parameter_id: STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
        revision_id: STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
        schema_id: STD_TERMINAL_SCHEMA_ID,
        parameter_type_id: rows_type_id,
        result_type_id: STD_TERMINAL_DOCUMENT_TYPE_ID,
    };
    let checked = check_standard_presenter_declaration(declaration, catalogue, origins, &shape)?;
    Ok(CheckedStandardTerminalPresentTable {
        function_id: checked.function_id,
        parameter_id: checked.parameter_id,
        revision_id: checked.revision_id,
    })
}

#[derive(Clone, Copy)]
struct StandardUiConstructorSpec {
    function_id: FunctionId,
    revision_id: FunctionRevisionId,
    runtime_contract: &'static str,
    parameter_ids: &'static [ParameterId],
    parameter_names: &'static [&'static str],
    parameter_types: &'static [TypeId],
}

fn standard_ui_constructor_spec(name: &QualifiedSemanticName) -> Option<StandardUiConstructorSpec> {
    match name
        .parts()
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["std", "ui", "text"] => Some(StandardUiConstructorSpec {
            function_id: STD_UI_TEXT_FUNCTION_ID,
            revision_id: STD_UI_TEXT_FUNCTION_REVISION_ID,
            runtime_contract: STD_UI_TEXT_RUNTIME_CONTRACT,
            parameter_ids: &[STD_UI_TEXT_PARAMETER_ID],
            parameter_names: &["text"],
            parameter_types: &[STD_CHARACTER_LARGE_OBJECT_TYPE_ID],
        }),
        ["std", "ui", "button"] => Some(StandardUiConstructorSpec {
            function_id: STD_UI_BUTTON_FUNCTION_ID,
            revision_id: STD_UI_BUTTON_FUNCTION_REVISION_ID,
            runtime_contract: STD_UI_BUTTON_RUNTIME_CONTRACT,
            parameter_ids: &[
                STD_UI_BUTTON_LABEL_PARAMETER_ID,
                STD_UI_BUTTON_ENABLED_PARAMETER_ID,
            ],
            parameter_names: &["label", "enabled"],
            parameter_types: &[STD_CHARACTER_LARGE_OBJECT_TYPE_ID, STD_BOOLEAN_TYPE_ID],
        }),
        ["std", "ui", "panel"] => Some(StandardUiConstructorSpec {
            function_id: STD_UI_PANEL_FUNCTION_ID,
            revision_id: STD_UI_PANEL_FUNCTION_REVISION_ID,
            runtime_contract: STD_UI_PANEL_RUNTIME_CONTRACT,
            parameter_ids: &[STD_UI_PANEL_CONTENT_PARAMETER_ID],
            parameter_names: &["content"],
            parameter_types: &[STD_UI_TYPE_ID],
        }),
        ["std", "ui", "row"] => Some(StandardUiConstructorSpec {
            function_id: STD_UI_ROW_FUNCTION_ID,
            revision_id: STD_UI_ROW_FUNCTION_REVISION_ID,
            runtime_contract: STD_UI_ROW_RUNTIME_CONTRACT,
            parameter_ids: &[STD_UI_ROW_CONTENT_PARAMETER_ID],
            parameter_names: &["content"],
            parameter_types: &[STD_UI_TYPE_ID],
        }),
        ["std", "ui", "column"] => Some(StandardUiConstructorSpec {
            function_id: STD_UI_COLUMN_FUNCTION_ID,
            revision_id: STD_UI_COLUMN_FUNCTION_REVISION_ID,
            runtime_contract: STD_UI_COLUMN_RUNTIME_CONTRACT,
            parameter_ids: &[STD_UI_COLUMN_CONTENT_PARAMETER_ID],
            parameter_names: &["content"],
            parameter_types: &[STD_UI_TYPE_ID],
        }),
        ["std", "ui", "text_input"] => Some(StandardUiConstructorSpec {
            function_id: STD_UI_TEXT_INPUT_FUNCTION_ID,
            revision_id: STD_UI_TEXT_INPUT_FUNCTION_REVISION_ID,
            runtime_contract: STD_UI_TEXT_INPUT_RUNTIME_CONTRACT,
            parameter_ids: &[
                STD_UI_TEXT_INPUT_TEXT_PARAMETER_ID,
                STD_UI_TEXT_INPUT_PLACEHOLDER_PARAMETER_ID,
                STD_UI_TEXT_INPUT_ENABLED_PARAMETER_ID,
            ],
            parameter_names: &["text", "placeholder", "enabled"],
            parameter_types: &[
                STD_CHARACTER_LARGE_OBJECT_TYPE_ID,
                STD_CHARACTER_LARGE_OBJECT_TYPE_ID,
                STD_BOOLEAN_TYPE_ID,
            ],
        }),
        ["std", "ui", "tabs"] => Some(StandardUiConstructorSpec {
            function_id: STD_UI_TABS_FUNCTION_ID,
            revision_id: STD_UI_TABS_FUNCTION_REVISION_ID,
            runtime_contract: STD_UI_TABS_RUNTIME_CONTRACT,
            parameter_ids: &[STD_UI_TABS_CONTENT_PARAMETER_ID],
            parameter_names: &["content"],
            parameter_types: &[STD_UI_TYPE_ID],
        }),
        _ => None,
    }
}

/// Checks one parsed declaration against the closed Work ADR 0088
/// `std.ui.*` external CLIENT constructor shape.
///
/// The declaration must be one of the seven fixed constructor functions, with
/// the exact ordered parameter identities and types, one `std.ui.UI` result,
/// matching runtime/body contract identities, no defaults, and no
/// capabilities. The supplied origins must contain exactly the function and
/// parameter declarations in `std/ui_constructors.orna`.
pub fn check_standard_ui_constructor(
    declaration: &ClientFunctionDeclaration,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
) -> Result<CheckedStandardUiConstructor, StandardLibraryCheckError> {
    let expected_name = unquoted_semantic_name(&declaration.name)?;
    let Some(spec) = standard_ui_constructor_spec(&expected_name) else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    if !declaration.external
        || !declaration.capabilities.is_empty()
        || declaration.parameters.len() != spec.parameter_ids.len()
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let Some(runtime_contract) = declaration.runtime_contract.as_ref() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    if decode_string_literal(runtime_contract).as_deref() != Some(spec.runtime_contract) {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let Some(body_contract) = declaration.body.as_external_contract() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    if client_contract_identity(body_contract).as_deref() != Some(spec.runtime_contract) {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    for (ordinal, ((parameter, _), (expected_name, expected_type))) in declaration
        .parameters
        .iter()
        .zip(spec.parameter_ids)
        .zip(spec.parameter_names.iter().zip(spec.parameter_types))
        .enumerate()
    {
        if parameter.order != ordinal
            || semantic_part(&parameter.name) != *expected_name
            || parameter.name.text.starts_with('"')
            || parameter.default_expression.is_some()
            || resolved_standard_type_id(&parameter.type_specification, catalogue)
                != Some(*expected_type)
        {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
    }

    let FunctionReturnType::Single(result_type) = &declaration.return_type else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let expected_ui_name =
        QualifiedSemanticName::new(["std", "ui", "ui"]).expect("fixed UI type name is valid");
    if !matches!(
        result_type,
        TypeSpecification::Named(result_name)
            if matches_qualified_name(result_name, &expected_ui_name)
                && resolved_standard_type_id(result_type, catalogue) == Some(STD_UI_TYPE_ID)
    ) {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let function = catalogue
        .function_by_id(spec.function_id)
        .ok_or(StandardLibraryCheckError::MissingFunction)?;
    if function.name() != &expected_name
        || function.domain() != FunctionDomain::Client
        || function.security() != CatalogueFunctionSecurity::Invoker
        || function.transaction().is_some()
        || function.volatility() != CatalogueFunctionVolatility::Immutable
        || function.current_revision() != spec.revision_id
        || function.parameters().len() != spec.parameter_ids.len()
        || function.return_type() != &FunctionReturn::Single(ResolvedType::value(STD_UI_TYPE_ID))
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    for (ordinal, ((parameter, expected_id), (expected_name, expected_type))) in function
        .parameters()
        .iter()
        .zip(spec.parameter_ids)
        .zip(spec.parameter_names.iter().zip(spec.parameter_types))
        .enumerate()
    {
        if parameter.id() != *expected_id
            || parameter.ordinal() != ordinal as u32
            || parameter.name() != *expected_name
            || parameter.resolved_type() != ResolvedType::value(*expected_type)
            || parameter.default_expression().is_some()
        {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
    }

    let mut origins_by_identity = HashMap::with_capacity(origins.len());
    for origin in origins {
        if !matches!(
            origin.identity(),
            DefinitionIdentity::Function(_) | DefinitionIdentity::Parameter { .. }
        ) || origins_by_identity
            .insert(origin.identity(), origin.source())
            .is_some()
        {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
    }
    take_origin(
        &mut origins_by_identity,
        DefinitionIdentity::Function(spec.function_id),
        STD_UI_CONSTRUCTORS_SOURCE_UNIT_ID,
        &declaration.span,
    )?;
    for (parameter, expected_id) in declaration.parameters.iter().zip(spec.parameter_ids) {
        take_origin(
            &mut origins_by_identity,
            DefinitionIdentity::Parameter {
                owner: spec.function_id,
                parameter: *expected_id,
            },
            STD_UI_CONSTRUCTORS_SOURCE_UNIT_ID,
            &parameter.span,
        )?;
    }
    if !origins_by_identity.is_empty() {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    Ok(CheckedStandardUiConstructor {
        function_id: spec.function_id,
        parameter_ids: spec.parameter_ids.to_vec(),
        revision_id: spec.revision_id,
        runtime_contract: spec.runtime_contract,
    })
}

/// Reconciles the retained `std/invoke.orna` unit against the snapshot
/// catalogue, origins, and verified executable evidence.
///
/// The unit must round-trip exactly and contain nothing besides the
/// `CREATE SCHEMA std.invoke;` declaration and the one `std.invoke.echo`
/// server function. The function is checked closed by
/// [`check_standard_parameter_echo`], the three invoke origins must cover the
/// exact schema, function, and parameter declaration ranges, and the stored
/// `StandardExecutable` must agree with every checked fact.
/// Checks one parsed declaration against the closed ADR 0019
/// `std.ui.window` external CLIENT function shape.
///
/// The declaration must be external, carry exactly the ordered `title TEXT`
/// and `content std.ui.UI` parameters, return one `std.ui.UI` value, and carry
/// exactly the `std.ui.window@1` runtime contract. CLIENT functions use the
/// existing invoker/immutable catalogue shape: no transaction or volatility
/// clause is written in source, and no capability requirements are accepted.
pub fn check_standard_ui_window(
    declaration: &ClientFunctionDeclaration,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
) -> Result<CheckedStandardUiWindow, StandardLibraryCheckError> {
    let expected_name =
        QualifiedSemanticName::new(["std", "ui", "window"]).expect("fixed function name is valid");
    if !declaration.external
        || semantic_name(&declaration.name) != expected_name
        || declaration.parameters.len() != 2
        || !declaration.capabilities.is_empty()
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let Some(runtime_contract) = declaration.runtime_contract.as_ref() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    if decode_string_literal(runtime_contract).as_deref() != Some(STD_UI_WINDOW_RUNTIME_CONTRACT) {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let Some(body_contract) = declaration.body.as_external_contract() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    if client_contract_identity(body_contract).as_deref() != Some(STD_UI_WINDOW_RUNTIME_CONTRACT) {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let [title, content] = declaration.parameters.as_slice() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    if title.order != 0
        || semantic_part(&title.name) != "title"
        || title.name.text.starts_with('"')
        || title.default_expression.is_some()
        || resolved_standard_type_id(&title.type_specification, catalogue)
            != Some(STD_CHARACTER_LARGE_OBJECT_TYPE_ID)
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let expected_content_name =
        QualifiedSemanticName::new(["std", "ui", "ui"]).expect("fixed UI type name is valid");
    let TypeSpecification::Named(content_type) = &content.type_specification else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    if content.order != 1
        || semantic_part(&content.name) != "content"
        || content.name.text.starts_with('"')
        || content.default_expression.is_some()
        || !matches_qualified_name(content_type, &expected_content_name)
        || resolved_standard_type_id(&content.type_specification, catalogue) != Some(STD_UI_TYPE_ID)
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let FunctionReturnType::Single(result_type) = &declaration.return_type else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    if !matches!(
        result_type,
        TypeSpecification::Named(result_name)
            if matches_qualified_name(result_name, &expected_content_name)
                && resolved_standard_type_id(result_type, catalogue) == Some(STD_UI_TYPE_ID)
    ) {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let schema_name =
        QualifiedSemanticName::new(["std", "ui"]).expect("fixed UI schema name is valid");
    let schema = catalogue
        .schema_by_id(STD_UI_SCHEMA_ID)
        .ok_or(StandardLibraryCheckError::MissingSchema)?;
    if schema.name() != &schema_name {
        return Err(StandardLibraryCheckError::SchemaNameMismatch {
            actual: schema.name().clone(),
        });
    }
    let function = catalogue
        .function_by_id(STD_UI_WINDOW_FUNCTION_ID)
        .ok_or(StandardLibraryCheckError::MissingFunction)?;
    if function.name() != &expected_name
        || function.domain() != FunctionDomain::Client
        || function.security() != CatalogueFunctionSecurity::Invoker
        || function.transaction().is_some()
        || function.volatility() != CatalogueFunctionVolatility::Immutable
        || function.current_revision() != STD_UI_WINDOW_FUNCTION_REVISION_ID
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let title_definition = function
        .parameter_by_id(STD_UI_WINDOW_TITLE_PARAMETER_ID)
        .ok_or(StandardLibraryCheckError::MissingParameter)?;
    let content_definition = function
        .parameter_by_id(STD_UI_WINDOW_CONTENT_PARAMETER_ID)
        .ok_or(StandardLibraryCheckError::MissingParameter)?;
    if title_definition.name() != "title"
        || title_definition.ordinal() != 0
        || title_definition.resolved_type()
            != ResolvedType::value(STD_CHARACTER_LARGE_OBJECT_TYPE_ID)
        || content_definition.name() != "content"
        || content_definition.ordinal() != 1
        || content_definition.resolved_type() != ResolvedType::value(STD_UI_TYPE_ID)
        || function.parameters().len() != 2
        || function.return_type() != &FunctionReturn::Single(ResolvedType::value(STD_UI_TYPE_ID))
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let mut origins_by_identity = HashMap::with_capacity(origins.len());
    for origin in origins {
        if !matches!(
            origin.identity(),
            DefinitionIdentity::Function(_) | DefinitionIdentity::Parameter { .. }
        ) || origins_by_identity
            .insert(origin.identity(), origin.source())
            .is_some()
        {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
    }
    let function_origin = take_origin(
        &mut origins_by_identity,
        DefinitionIdentity::Function(STD_UI_WINDOW_FUNCTION_ID),
        STD_WINDOW_SOURCE_UNIT_ID,
        &declaration.span,
    )?;
    let title_origin = take_origin(
        &mut origins_by_identity,
        DefinitionIdentity::Parameter {
            owner: STD_UI_WINDOW_FUNCTION_ID,
            parameter: STD_UI_WINDOW_TITLE_PARAMETER_ID,
        },
        STD_WINDOW_SOURCE_UNIT_ID,
        &title.span,
    )?;
    let content_origin = take_origin(
        &mut origins_by_identity,
        DefinitionIdentity::Parameter {
            owner: STD_UI_WINDOW_FUNCTION_ID,
            parameter: STD_UI_WINDOW_CONTENT_PARAMETER_ID,
        },
        STD_WINDOW_SOURCE_UNIT_ID,
        &content.span,
    )?;
    if !origins_by_identity.is_empty()
        || function_origin.source_unit() != title_origin.source_unit()
        || function_origin.source_unit() != content_origin.source_unit()
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    Ok(CheckedStandardUiWindow {
        function_id: STD_UI_WINDOW_FUNCTION_ID,
        title_parameter_id: STD_UI_WINDOW_TITLE_PARAMETER_ID,
        content_parameter_id: STD_UI_WINDOW_CONTENT_PARAMETER_ID,
        revision_id: STD_UI_WINDOW_FUNCTION_REVISION_ID,
    })
}

fn reconcile_standard_invoke_executable(
    catalogue: &CatalogueSnapshot,
    invoke_origins: &[DefinitionOrigin],
    executables: &[StandardExecutable],
    stored_unit: &StoredSourceUnit,
    parsed_unit: &ParsedSourceUnit,
) -> Result<CheckedStandardExecutable, StandardLibraryCheckError> {
    if parsed_unit.source_text() != stored_unit.content()
        || parsed_unit.source_text() != parsed_unit.syntax_text()
        || !parsed_unit.parsed().object_types().is_empty()
        || !parsed_unit.parsed().enum_types().is_empty()
        || !parsed_unit.parsed().primitive_value_types().is_empty()
        || !parsed_unit.parsed().opaque_value_types().is_empty()
        || !parsed_unit.parsed().record_value_types().is_empty()
        || !parsed_unit.parsed().field_renames().is_empty()
        || !parsed_unit.parsed().type_exports().is_empty()
        || !parsed_unit.parsed().client_functions().is_empty()
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let [schema_declaration] = parsed_unit.parsed().schemas() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let [function_declaration] = parsed_unit.parsed().server_functions() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let expected_schema_name =
        QualifiedSemanticName::new(["std", "invoke"]).expect("the fixed standard schema is valid");
    let schema_name = unquoted_semantic_name(&schema_declaration.name)?;
    if schema_name != expected_schema_name {
        return Err(StandardLibraryCheckError::SchemaNameMismatch {
            actual: schema_name,
        });
    }

    let checked = check_standard_parameter_echo(
        function_declaration,
        catalogue,
        invoke_origins,
        STD_INTEGER_TYPE_ID,
    )?;

    let source_origin = |span: &SourceSpan| -> Result<SourceOrigin, StandardLibraryCheckError> {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
        let end = u32::try_from(span.end).map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
        SourceOrigin::new(stored_unit.id(), start, end)
            .map_err(|_| StandardLibraryCheckError::SourceMismatch)
    };
    let expected_schema_origin = source_origin(&schema_declaration.span)?;
    let expected_function_origin = source_origin(&function_declaration.span)?;
    let expected_parameter_origin = source_origin(&function_declaration.parameters[0].span)?;

    let schema_origin = expect_invoke_origin(
        invoke_origins,
        DefinitionIdentity::Schema(STD_INVOKE_SCHEMA_ID),
        expected_schema_origin,
        StandardLibraryCheckError::MissingSchemaOrigin,
    )?;
    let function_origin = expect_invoke_origin(
        invoke_origins,
        DefinitionIdentity::Function(STD_INVOKE_ECHO_FUNCTION_ID),
        expected_function_origin,
        StandardLibraryCheckError::MissingFunctionOrigin,
    )?;
    let parameter_origin = expect_invoke_origin(
        invoke_origins,
        DefinitionIdentity::Parameter {
            owner: STD_INVOKE_ECHO_FUNCTION_ID,
            parameter: STD_INVOKE_ECHO_PARAMETER_ID,
        },
        expected_parameter_origin,
        StandardLibraryCheckError::MissingParameterOrigin,
    )?;
    if invoke_origins.len() != 3 {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let declaration_bytes = &stored_unit.content().as_bytes()[expected_function_origin.byte_start()
        as usize
        ..expected_function_origin.byte_end() as usize];
    let declaration_content_hash = function_declaration_digest(declaration_bytes)
        .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let function = catalogue
        .function_by_id(STD_INVOKE_ECHO_FUNCTION_ID)
        .ok_or(StandardLibraryCheckError::MissingFunction)?;
    let semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        function,
        server_parameter_echo::LANGUAGE_VERSION_IDENTITY,
        checked.artifact(),
        &[],
        checked.references(),
    )
    .map_err(|source| StandardLibraryCheckError::Digest { source })?;

    let checked_executable = CheckedStandardExecutable {
        function_id: checked.function_id(),
        parameter_ids: vec![checked.parameter_id()],
        revision_id: checked.revision_id(),
        revision_number: STD_INVOKE_ECHO_REVISION_NUMBER,
        declaration_origin: expected_function_origin,
        declaration_content_hash,
        semantic_hash,
        semantic_hash_version: FunctionSemanticHashVersion::Version2,
        language_version: server_parameter_echo::LANGUAGE_VERSION_IDENTITY.to_owned(),
        artifact: checked.artifact().clone(),
        references: checked.references().to_vec(),
        schema_origin,
        function_origin,
        parameter_origins: vec![parameter_origin],
    };

    let [stored_executable] = executables else {
        return Err(StandardLibraryCheckError::ExecutableCount {
            actual: executables.len(),
        });
    };
    reconcile_standard_executable(stored_executable, &checked_executable)?;
    Ok(checked_executable)
}

/// Cross-checks one stored standard executable against the checked source
/// facts. Every stored fact must agree exactly, or the snapshot fails closed.
fn reconcile_standard_executable(
    stored: &StandardExecutable,
    checked: &CheckedStandardExecutable,
) -> Result<(), StandardLibraryCheckError> {
    if stored.function() != checked.function_id()
        || stored.revision().id() != checked.revision_id()
        || stored.revision().revision_number() != checked.revision_number()
        || stored.revision().semantic_hash_version() != checked.semantic_hash_version()
        || stored.revision().language_version() != checked.language_version()
        || stored.revision().declaration_origin() != checked.declaration_origin()
        || stored.revision().declaration_content_hash() != checked.declaration_content_hash()
        || stored.revision().semantic_hash() != checked.semantic_hash()
        || stored.revision().artifact() != checked.artifact()
        || stored.references() != checked.references()
    {
        return Err(StandardLibraryCheckError::ExecutableMismatch);
    }
    Ok(())
}

/// Requires exactly one origin with the fixed identity and the exact expected
/// range. A missing, duplicated, or range-mismatched origin fails closed.
fn expect_invoke_origin(
    origins: &[DefinitionOrigin],
    identity: DefinitionIdentity,
    expected: SourceOrigin,
    missing: StandardLibraryCheckError,
) -> Result<SourceOrigin, StandardLibraryCheckError> {
    let mut matches = 0;
    for origin in origins {
        if origin.identity() == identity {
            matches += 1;
            if origin.source() != expected {
                return Err(StandardLibraryCheckError::SourceMismatch);
            }
        }
    }
    if matches == 1 {
        Ok(expected)
    } else {
        Err(missing)
    }
}

/// Resolves one written type specification to its durable type identity in
/// the supplied catalogue, mirroring the standard prelude and qualified
/// lookup rules used by application type resolution.
fn resolved_standard_type_id(
    specification: &TypeSpecification,
    catalogue: &CatalogueSnapshot,
) -> Option<TypeId> {
    let TypeSpecification::Named(name) = specification else {
        return None;
    };
    if name.parts.len() == 1 && !name.parts[0].text.starts_with('"') {
        let prelude = PreludeTypeName::new([semantic_part(&name.parts[0])]).ok()?;
        catalogue.type_id_by_name(&TypeLookupName::prelude(prelude))
    } else {
        catalogue.type_id_by_name(&TypeLookupName::qualified(semantic_name(name)))
    }
}

#[cfg(test)]
pub(crate) fn checked_standard_library_with_contract_overrides_for_test(
    snapshot: &VerifiedStandardLibrarySnapshot,
    overrides: &[(usize, &str)],
) -> Result<CheckedStandardLibrary, StandardLibraryCheckError> {
    let mut checked = check_standard_library_source(snapshot)?;
    for (index, contract) in overrides {
        let Some(value_type) = checked.value_types.get_mut(*index) else {
            return Err(StandardLibraryCheckError::SourceMismatch);
        };
        value_type.representation_contract = (*contract).to_owned();
    }
    Ok(checked)
}

#[derive(Debug, Eq, PartialEq)]
struct StandardSourceFamilies {
    schemas: Vec<CheckedStandardSchema>,
    value_types: Vec<CheckedStandardValueType>,
    type_bindings: Vec<CheckedStandardTypeBinding>,
}

#[derive(Debug, Eq, PartialEq)]
struct PendingStandardSourceFacts {
    schemas: Vec<PendingStandardSchema>,
    value_types: Vec<PendingStandardValueType>,
    type_bindings: Vec<PendingStandardTypeBinding>,
}

#[derive(Debug, Eq, PartialEq)]
struct PendingStandardSchema {
    id: orna_core::SchemaId,
    name: QualifiedSemanticName,
    span: SourceSpan,
}

#[derive(Debug, Eq, PartialEq)]
struct PendingStandardValueType {
    id: orna_core::TypeId,
    name: QualifiedSemanticName,
    kind: ValueTypeKind,
    mutability: ValueTypeMutability,
    persistence: ValueTypePersistence,
    representation_contract: String,
    span: SourceSpan,
}

#[derive(Debug, Eq, PartialEq)]
struct PendingStandardTypeBinding {
    id: orna_core::TypeBindingId,
    kind: TypeBindingKind,
    name: TypeLookupName,
    target: orna_core::TypeId,
    span: SourceSpan,
}

fn reconcile_standard_source(
    stored_unit: &StoredSourceUnit,
    parsed_unit: &ParsedSourceUnit,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
) -> Result<StandardSourceFamilies, StandardLibraryCheckError> {
    validate_standard_source_shape(stored_unit, parsed_unit, catalogue)?;
    let pending = match_standard_source_facts(parsed_unit, catalogue)?;
    validate_standard_source_origins(stored_unit, origins, pending)
}

fn validate_standard_source_shape(
    stored_unit: &StoredSourceUnit,
    parsed_unit: &ParsedSourceUnit,
    catalogue: &CatalogueSnapshot,
) -> Result<(), StandardLibraryCheckError> {
    if parsed_unit.source_text() != stored_unit.content()
        || parsed_unit.source_text() != parsed_unit.syntax_text()
        || !catalogue.object_types().is_empty()
        || !catalogue.enum_types().is_empty()
        || !catalogue.record_value_types().is_empty()
        || !catalogue.functions().is_empty()
        || !parsed_unit.parsed().object_types().is_empty()
        || !parsed_unit.parsed().enum_types().is_empty()
        || !parsed_unit.parsed().record_value_types().is_empty()
        || !parsed_unit.parsed().field_renames().is_empty()
        || !parsed_unit.parsed().server_functions().is_empty()
        || !parsed_unit.parsed().client_functions().is_empty()
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let (qualified_binding_count, prelude_binding_count) =
        catalogue_binding_category_counts(catalogue)?;
    let (qualified_export_count, prelude_export_count) =
        source_export_category_counts(parsed_unit)?;
    if parsed_unit.parsed().schemas().len() != catalogue.schemas().len()
        || parsed_unit.parsed().primitive_value_types().len()
            + parsed_unit.parsed().opaque_value_types().len()
            != catalogue.value_types().len()
        || qualified_export_count != qualified_binding_count
        || prelude_export_count != prelude_binding_count
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    Ok(())
}

fn match_standard_source_facts(
    parsed_unit: &ParsedSourceUnit,
    catalogue: &CatalogueSnapshot,
) -> Result<PendingStandardSourceFacts, StandardLibraryCheckError> {
    let mut consumed_schema_ids = HashSet::with_capacity(catalogue.schemas().len());
    let mut consumed_type_ids = HashSet::with_capacity(catalogue.value_types().len());
    let mut consumed_binding_ids = HashSet::with_capacity(catalogue.type_bindings().len());

    let mut schemas = Vec::with_capacity(parsed_unit.parsed().schemas().len());
    for declaration in parsed_unit.parsed().schemas() {
        let name = unquoted_semantic_name(&declaration.name)?;
        let definition = catalogue
            .schema_by_name(&name)
            .ok_or(StandardLibraryCheckError::SourceMismatch)?;
        if !consumed_schema_ids.insert(definition.id()) {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
        schemas.push(PendingStandardSchema {
            id: definition.id(),
            name,
            span: declaration.span.clone(),
        });
    }

    let mut primary_type_ids = HashMap::with_capacity(catalogue.value_types().len());
    let mut value_types = Vec::with_capacity(catalogue.value_types().len());
    let mut match_value_type = |name: QualifiedSemanticName,
                                kind: ValueTypeKind,
                                persistence: ValueTypePersistence,
                                contract: String,
                                span: SourceSpan|
     -> Result<PendingStandardValueType, StandardLibraryCheckError> {
        let definition = catalogue
            .value_type_by_name(&name)
            .ok_or(StandardLibraryCheckError::SourceMismatch)?;
        if definition.kind() != kind
            || definition.mutability() != ValueTypeMutability::Immutable
            || definition.persistence() != persistence
            || definition.representation_contract() != contract
            || !consumed_type_ids.insert(definition.id())
            || primary_type_ids
                .insert(name.clone(), definition.id())
                .is_some()
        {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
        Ok(PendingStandardValueType {
            id: definition.id(),
            name,
            kind: definition.kind(),
            mutability: definition.mutability(),
            persistence: definition.persistence(),
            representation_contract: definition.representation_contract().to_owned(),
            span,
        })
    };
    for declaration in parsed_unit.parsed().primitive_value_types() {
        let name = unquoted_semantic_name(&declaration.name)?;
        let contract = decode_string_literal(&declaration.kernel_contract)
            .ok_or(StandardLibraryCheckError::SourceMismatch)?;
        let persistence = value_type_persistence(declaration.persistence);
        value_types.push(match_value_type(
            name,
            ValueTypeKind::Primitive,
            persistence,
            contract,
            declaration.span.clone(),
        )?);
    }
    for declaration in parsed_unit.parsed().opaque_value_types() {
        let name = unquoted_semantic_name(&declaration.name)?;
        let contract = decode_string_literal(&declaration.kernel_contract)
            .filter(|contract| opaque_contract_is_valid(contract))
            .ok_or(StandardLibraryCheckError::SourceMismatch)?;
        value_types.push(match_value_type(
            name,
            ValueTypeKind::Opaque,
            ValueTypePersistence::Transient,
            contract,
            declaration.span.clone(),
        )?);
    }
    value_types.sort_by_key(|value_type| value_type.span.start);

    let type_exports = parsed_unit.parsed().type_exports();
    let mut qualified_bindings = (0..type_exports.len()).map(|_| None).collect::<Vec<_>>();
    let mut qualified_targets = HashMap::with_capacity(catalogue.type_bindings().len());
    for (index, declaration) in type_exports.iter().enumerate() {
        let TypeExportTarget::Qualified { name } = &declaration.target else {
            continue;
        };
        let source_name = unquoted_semantic_name(&declaration.source_type)?;
        let target_name = unquoted_semantic_name(name)?;
        let target = primary_type_ids
            .get(&source_name)
            .copied()
            .ok_or(StandardLibraryCheckError::SourceMismatch)?;
        let lookup_name = TypeLookupName::qualified(target_name.clone());
        let binding = catalogue
            .type_binding_by_name(&lookup_name)
            .ok_or(StandardLibraryCheckError::SourceMismatch)?;
        if !matches!(binding.kind(), TypeBindingKind::Qualified)
            || binding.target() != target
            || !consumed_binding_ids.insert(binding.id())
            || qualified_targets.insert(target_name, target).is_some()
        {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
        qualified_bindings[index] = Some(PendingStandardTypeBinding {
            id: binding.id(),
            kind: binding.kind(),
            name: binding.name().clone(),
            target: binding.target(),
            span: declaration.span.clone(),
        });
    }

    let mut type_bindings = Vec::with_capacity(type_exports.len());
    for (index, declaration) in type_exports.iter().enumerate() {
        match &declaration.target {
            TypeExportTarget::Qualified { .. } => {
                let binding = qualified_bindings[index]
                    .take()
                    .ok_or(StandardLibraryCheckError::SourceMismatch)?;
                type_bindings.push(binding);
            }
            TypeExportTarget::Prelude { words, .. } => {
                let source_name = unquoted_semantic_name(&declaration.source_type)?;
                let target = qualified_targets
                    .get(&source_name)
                    .copied()
                    .ok_or(StandardLibraryCheckError::SourceMismatch)?;
                let prelude_name = unquoted_prelude_name(words)?;
                let lookup_name = TypeLookupName::prelude(prelude_name);
                let binding = catalogue
                    .type_binding_by_name(&lookup_name)
                    .ok_or(StandardLibraryCheckError::SourceMismatch)?;
                if !matches!(binding.kind(), TypeBindingKind::Prelude)
                    || binding.target() != target
                    || !consumed_binding_ids.insert(binding.id())
                {
                    return Err(StandardLibraryCheckError::SourceMismatch);
                }
                type_bindings.push(PendingStandardTypeBinding {
                    id: binding.id(),
                    kind: binding.kind(),
                    name: binding.name().clone(),
                    target: binding.target(),
                    span: declaration.span.clone(),
                });
            }
        }
    }

    if consumed_schema_ids.len() != catalogue.schemas().len()
        || consumed_type_ids.len() != catalogue.value_types().len()
        || consumed_binding_ids.len() != catalogue.type_bindings().len()
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    Ok(PendingStandardSourceFacts {
        schemas,
        value_types,
        type_bindings,
    })
}

fn validate_standard_source_origins(
    stored_unit: &StoredSourceUnit,
    origins: &[DefinitionOrigin],
    pending: PendingStandardSourceFacts,
) -> Result<StandardSourceFamilies, StandardLibraryCheckError> {
    let mut origins_by_identity = origin_map(origins)?;
    let schemas = pending
        .schemas
        .into_iter()
        .map(|fact| {
            let origin = take_origin(
                &mut origins_by_identity,
                DefinitionIdentity::Schema(fact.id),
                stored_unit.id(),
                &fact.span,
            )?;
            Ok(CheckedStandardSchema {
                id: fact.id,
                name: fact.name,
                origin,
            })
        })
        .collect::<Result<Vec<_>, StandardLibraryCheckError>>()?;
    let value_types = pending
        .value_types
        .into_iter()
        .map(|fact| {
            let origin = take_origin(
                &mut origins_by_identity,
                DefinitionIdentity::ValueType(fact.id),
                stored_unit.id(),
                &fact.span,
            )?;
            Ok(CheckedStandardValueType {
                id: fact.id,
                name: fact.name,
                kind: fact.kind,
                mutability: fact.mutability,
                persistence: fact.persistence,
                representation_contract: fact.representation_contract,
                origin,
            })
        })
        .collect::<Result<Vec<_>, StandardLibraryCheckError>>()?;
    let type_bindings = pending
        .type_bindings
        .into_iter()
        .map(|fact| {
            let origin = take_origin(
                &mut origins_by_identity,
                DefinitionIdentity::TypeBinding(fact.id),
                stored_unit.id(),
                &fact.span,
            )?;
            Ok(CheckedStandardTypeBinding {
                id: fact.id,
                kind: fact.kind,
                name: fact.name,
                target: fact.target,
                origin,
            })
        })
        .collect::<Result<Vec<_>, StandardLibraryCheckError>>()?;
    if !origins_by_identity.is_empty() {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    Ok(StandardSourceFamilies {
        schemas,
        value_types,
        type_bindings,
    })
}

fn catalogue_binding_category_counts(
    catalogue: &CatalogueSnapshot,
) -> Result<(usize, usize), StandardLibraryCheckError> {
    let mut qualified = 0;
    let mut prelude = 0;
    for binding in catalogue.type_bindings() {
        match binding.kind() {
            TypeBindingKind::Qualified => qualified += 1,
            TypeBindingKind::Prelude => prelude += 1,
            _ => return Err(StandardLibraryCheckError::SourceMismatch),
        }
    }
    Ok((qualified, prelude))
}

fn source_export_category_counts(
    parsed_unit: &ParsedSourceUnit,
) -> Result<(usize, usize), StandardLibraryCheckError> {
    let mut qualified = 0;
    let mut prelude = 0;
    for declaration in parsed_unit.parsed().type_exports() {
        match &declaration.target {
            TypeExportTarget::Qualified { .. } => qualified += 1,
            TypeExportTarget::Prelude { .. } => prelude += 1,
        }
    }
    Ok((qualified, prelude))
}

fn origin_map(
    origins: &[DefinitionOrigin],
) -> Result<HashMap<DefinitionIdentity, SourceOrigin>, StandardLibraryCheckError> {
    let mut by_identity = HashMap::with_capacity(origins.len());
    for origin in origins {
        match origin.identity() {
            DefinitionIdentity::Schema(_)
            | DefinitionIdentity::ValueType(_)
            | DefinitionIdentity::TypeBinding(_) => {}
            _ => return Err(StandardLibraryCheckError::SourceMismatch),
        }
        if by_identity
            .insert(origin.identity(), origin.source())
            .is_some()
        {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
    }
    Ok(by_identity)
}

fn take_origin(
    origins: &mut HashMap<DefinitionIdentity, SourceOrigin>,
    identity: DefinitionIdentity,
    source_unit: orna_core::SourceUnitId,
    span: &SourceSpan,
) -> Result<SourceOrigin, StandardLibraryCheckError> {
    let byte_start =
        u32::try_from(span.start).map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let byte_end =
        u32::try_from(span.end).map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let expected = SourceOrigin::new(source_unit, byte_start, byte_end)
        .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let actual = origins
        .remove(&identity)
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    if actual != expected {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    Ok(actual)
}

fn unquoted_semantic_name(
    name: &QualifiedName,
) -> Result<QualifiedSemanticName, StandardLibraryCheckError> {
    if name.parts.iter().any(|part| part.text.starts_with('"')) {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    QualifiedSemanticName::new(name.parts.iter().map(semantic_part))
        .map_err(|_| StandardLibraryCheckError::SourceMismatch)
}
fn matches_qualified_name(name: &QualifiedName, expected: &QualifiedSemanticName) -> bool {
    unquoted_semantic_name(name)
        .ok()
        .is_some_and(|actual| actual == *expected)
}

fn unquoted_prelude_name(
    words: &[orna_syntax::NamePart],
) -> Result<PreludeTypeName, StandardLibraryCheckError> {
    if words.iter().any(|word| word.text.starts_with('"')) {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    PreludeTypeName::new(words.iter().map(semantic_part))
        .map_err(|_| StandardLibraryCheckError::SourceMismatch)
}

fn value_type_persistence(persistence: PrimitiveValueTypePersistence) -> ValueTypePersistence {
    match persistence {
        PrimitiveValueTypePersistence::Persistable => ValueTypePersistence::Persistable,
        PrimitiveValueTypePersistence::Transient => ValueTypePersistence::Transient,
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
    let result = check_application_parsed(parse_report, base, None, false);
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
    allow_protected_source: bool,
) -> ApplicationCheckResult {
    let mut diagnostics = parse_report.diagnostics().to_vec();
    if !diagnostics.is_empty() {
        return application_failed(parse_report, diagnostics);
    }

    if !allow_protected_source {
        diagnostics.extend(check_protected_source(&parse_report));
        if !diagnostics.is_empty() {
            return application_failed(parse_report, diagnostics);
        }
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

fn resolve_client_function_headers<'a>(
    parse_report: &'a ParseReport,
    function_ids: &HashMap<QualifiedSemanticName, CheckedFunctionId>,
) -> Vec<ClientFunctionHeader<'a>> {
    let mut headers = Vec::new();
    for unit in parse_report.units() {
        for declaration in unit.parsed().client_functions() {
            let name = semantic_name(&declaration.name);
            if let Some(&id) = function_ids.get(&name) {
                headers.push(ClientFunctionHeader {
                    declaration,
                    logical_path: unit.logical_path(),
                    id,
                });
            }
        }
    }
    headers
}

fn resolve_client_function_inputs<'a>(
    headers: &[ClientFunctionHeader<'a>],
    submitted_ids: &HashMap<QualifiedSemanticName, SubmittedType>,
    base: &CatalogueSnapshot,
    assignments: &mut CheckAssignments,
    diagnostics: &mut Vec<CompilerDiagnostic>,
    standard: Option<&CheckedStandardLibrary>,
    uses: &mut Vec<CheckedApplicationTypeUse>,
) -> Vec<ResolvedClientFunctionInput<'a>> {
    let mut inputs = Vec::with_capacity(headers.len());
    for header in headers {
        let declaration = header.declaration;
        let diagnostics_before = diagnostics.len();
        let name = semantic_name(&declaration.name);
        let base_function = base.function_by_name(&name);
        let expression_body = declaration.body.as_expression().is_some()
            || declaration.body.as_external_contract().is_some()
            || declaration.body.as_state_block().is_some();
        if !expression_body && !declaration.parameters.is_empty() {
            diagnostics.push(diagnostic(
                DiagnosticCode::DomainIncompatible,
                "this CLIENT function cannot declare parameters yet",
                header.logical_path,
                &declaration.parameter_list_span,
            ));
        }
        if !expression_body
            && let (
                Some(standard),
                FunctionReturnType::Single(specification),
                Some((_, body_source)),
            ) = (
                standard,
                &declaration.return_type,
                declaration.body.as_boolean_literal(),
            )
            && is_standard_client_boolean_return(specification)
            && matches!(
                intrinsic_boolean_type(Some(standard)),
                IntrinsicBooleanType::Missing
            )
        {
            diagnostics.push(diagnostic(
                DiagnosticCode::DomainIncompatible,
                "the checked standard library does not provide a Boolean value type",
                header.logical_path,
                &body_source.span,
            ));
        }
        let mut parameter_names = HashSet::new();
        let mut parameters = Vec::with_capacity(declaration.parameters.len());

        for parameter in &declaration.parameters {
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
            if let Some(default) = &parameter.default_expression {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    "CLIENT function parameters do not yet support default values",
                    header.logical_path,
                    &default.span,
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

        let return_type = match &declaration.return_type {
            FunctionReturnType::Single(specification) if !expression_body && standard.is_none() => {
                if is_closed_client_boolean_return(specification) {
                    Some(ResolvedApplicationType {
                        semantic_type: SemanticType::scalar(StandardScalar::Boolean),
                        standard_value_type: None,
                    })
                } else {
                    diagnostics.push(diagnostic(
                        DiagnosticCode::TypeMismatch,
                        "this CLIENT function must return BOOLEAN",
                        header.logical_path,
                        specification.span(),
                    ));
                    None
                }
            }
            FunctionReturnType::Single(specification) => {
                resolve_application_type_with_named_standard(
                    specification,
                    submitted_ids,
                    header.logical_path,
                    diagnostics,
                    standard,
                    true,
                )
            }
            FunctionReturnType::Stream { element, .. } if expression_body => {
                resolve_application_type_with_named_standard(
                    element,
                    submitted_ids,
                    header.logical_path,
                    diagnostics,
                    standard,
                    true,
                )
            }
            FunctionReturnType::Rows { span, .. } | FunctionReturnType::Stream { span, .. } => {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    if expression_body {
                        "this CLIENT function must return one value"
                    } else {
                        "this CLIENT function must return BOOLEAN"
                    },
                    header.logical_path,
                    span,
                ));
                None
            }
        };

        if diagnostics.len() != diagnostics_before {
            continue;
        }
        let Some(return_type) = return_type else {
            continue;
        };
        let result_shape = match &declaration.return_type {
            FunctionReturnType::Stream { .. } => ClientExpressionResultShape::OptionalList,
            FunctionReturnType::Single(_) | FunctionReturnType::Rows { .. } => {
                ClientExpressionResultShape::Value
            }
        };
        let return_shape = match result_shape {
            ClientExpressionResultShape::Value => CheckedClientReturnShape::Single,
            ClientExpressionResultShape::OptionalList => CheckedClientReturnShape::Stream,
        };
        if !expression_body
            && return_type.semantic_type != SemanticType::scalar(StandardScalar::Boolean)
        {
            let span = match &declaration.return_type {
                FunctionReturnType::Single(specification) => specification.span(),
                FunctionReturnType::Rows { span, .. } | FunctionReturnType::Stream { span, .. } => {
                    span
                }
            };
            diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                "this CLIENT function must return BOOLEAN",
                header.logical_path,
                span,
            ));
            continue;
        }
        if expression_body
            && !client_expression_type_is_evaluable(
                ClientExpressionType {
                    semantic_type: return_type.semantic_type,
                    standard_value_type: return_type.standard_value_type,
                    result_shape,
                },
                base,
                standard,
            )
        {
            let span = match &declaration.return_type {
                FunctionReturnType::Single(specification) => specification.span(),
                FunctionReturnType::Rows { span, .. } | FunctionReturnType::Stream { span, .. } => {
                    span
                }
            };
            diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                "this CLIENT function return type is not supported by the local evaluator",
                header.logical_path,
                span,
            ));
            continue;
        }
        if let FunctionReturnType::Single(specification)
        | FunctionReturnType::Stream {
            element: specification,
            ..
        } = &declaration.return_type
        {
            record_standard_type_use(
                uses,
                standard,
                CheckedTypeUseKind::Return {
                    owner: header.id,
                    ordinal: 0,
                },
                return_type,
                type_use_location(specification, header.logical_path),
            );
        }
        inputs.push(ResolvedClientFunctionInput {
            id: header.id,
            name,
            capabilities: &declaration.capabilities,
            parameters,
            return_type: return_type.semantic_type,
            standard_value_type: return_type.standard_value_type,
            result_shape,
            return_shape,
            body: &declaration.body,
            location: location(header.logical_path, &declaration.span),
            declaration_span: declaration.span.clone(),
            logical_path: header.logical_path,
            control_flow_required: client_body_requires_control_flow(&declaration.body),
        });
    }
    inputs
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClientCapabilityArgumentKind {
    PathScope,
    HostScope,
    SecretId,
}

impl ClientCapabilityArgumentKind {
    const fn label(self) -> &'static str {
        match self {
            Self::PathScope => "path-scope",
            Self::HostScope => "host-scope",
            Self::SecretId => "secret-id",
        }
    }
}

struct ClientCapabilityVocabularyEntry {
    parts: &'static [&'static str],
    argument_count: usize,
    argument_kind: ClientCapabilityArgumentKind,
}

const CLIENT_CAPABILITY_VOCABULARY: &[ClientCapabilityVocabularyEntry] = &[
    ClientCapabilityVocabularyEntry {
        parts: &["std", "fs", "read"],
        argument_count: 1,
        argument_kind: ClientCapabilityArgumentKind::PathScope,
    },
    ClientCapabilityVocabularyEntry {
        parts: &["std", "fs", "write"],
        argument_count: 1,
        argument_kind: ClientCapabilityArgumentKind::PathScope,
    },
    ClientCapabilityVocabularyEntry {
        parts: &["std", "net", "connect"],
        argument_count: 1,
        argument_kind: ClientCapabilityArgumentKind::HostScope,
    },
    ClientCapabilityVocabularyEntry {
        parts: &["std", "secret", "use"],
        argument_count: 1,
        argument_kind: ClientCapabilityArgumentKind::SecretId,
    },
];

#[derive(Clone, Debug, Eq, PartialEq)]
enum ClientCapabilityArgument {
    TextLiteral,
    Parameter(String),
}

fn client_capability_entry(
    name: &QualifiedSemanticName,
) -> Option<&'static ClientCapabilityVocabularyEntry> {
    CLIENT_CAPABILITY_VOCABULARY.iter().find(|entry| {
        name.parts()
            .iter()
            .map(String::as_str)
            .eq(entry.parts.iter().copied())
    })
}

fn client_capability_argument_count(arguments: Option<&SourceSlice>) -> usize {
    let Some(arguments) = arguments else {
        return 0;
    };
    let text = arguments.text.trim();
    if text.is_empty() {
        return 0;
    }

    let mut count = 1;
    let mut parentheses = 0usize;
    let mut quote = None;
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if let Some(quote_character) = quote {
            if character == quote_character {
                if characters.peek() == Some(&quote_character) {
                    characters.next();
                } else {
                    quote = None;
                }
            }
            continue;
        }
        match character {
            '\'' | '"' => quote = Some(character),
            '(' => parentheses += 1,
            ')' => parentheses = parentheses.saturating_sub(1),
            ',' if parentheses == 0 => count += 1,
            _ => {}
        }
    }
    count
}

fn parse_client_capability_argument(text: &str) -> Option<ClientCapabilityArgument> {
    let text = text.trim();
    if is_client_text_literal(text) {
        return Some(ClientCapabilityArgument::TextLiteral);
    }
    normalise_client_parameter_name(text).map(ClientCapabilityArgument::Parameter)
}

/// Records one validated capability requirement in the checked CLIENT model.
///
/// The checked name is the closed qualified vocabulary name and the argument
/// source is the declaration's literal scope value or parameter reference.
/// Validation has already run, so a non-vocabulary name, wrong argument
/// shape, or undeclared parameter cannot reach this conversion; unknown
/// forms map to `None` and are skipped.
fn checked_client_capability(
    capability: &CapabilitySpecification,
) -> Option<CheckedClientCapability> {
    let name = semantic_name(&capability.name);
    client_capability_entry(&name)?;
    let arguments = capability.arguments.as_ref()?;
    let argument = parse_client_capability_argument(&arguments.text)?;
    let argument = match argument {
        ClientCapabilityArgument::TextLiteral => {
            CheckedClientCapabilityArgument::Text(unquote_client_text_literal(&arguments.text)?)
        }
        ClientCapabilityArgument::Parameter(parameter) => {
            CheckedClientCapabilityArgument::Parameter(parameter)
        }
    };
    Some(CheckedClientCapability::new(name.to_string(), argument))
}

/// Unquotes one validated single-quoted CLIENT text literal.
///
/// A doubled quote inside the literal is a single literal quote, mirroring
/// `normalise_client_parameter_name`'s handling of quoted parameter names.
fn unquote_client_text_literal(text: &str) -> Option<String> {
    let text = text.trim();
    if !is_client_text_literal(text) {
        return None;
    }
    let inner = &text[1..text.len() - 1];
    let mut value = String::with_capacity(inner.len());
    let mut characters = inner.chars().peekable();
    while let Some(character) = characters.next() {
        value.push(character);
        if character == '\'' && characters.peek() == Some(&'\'') {
            characters.next();
        }
    }
    Some(value)
}

fn is_client_text_literal(text: &str) -> bool {
    let mut characters = text.chars();
    if characters.next() != Some('\'') || !text.ends_with('\'') {
        return false;
    }

    let mut characters = text[1..].chars().peekable();
    while let Some(character) = characters.next() {
        if character != '\'' {
            continue;
        }
        if characters.peek() == Some(&'\'') {
            characters.next();
        } else {
            return characters.peek().is_none();
        }
    }
    false
}

fn normalise_client_parameter_name(text: &str) -> Option<String> {
    if text.starts_with('"') {
        if !text.ends_with('"') || text.len() < 2 {
            return None;
        }
        let inner = &text[1..text.len() - 1];
        let mut characters = inner.chars().peekable();
        while let Some(character) = characters.next() {
            if character == '"' && characters.peek() == Some(&'"') {
                characters.next();
            } else if character == '"' {
                return None;
            }
        }
        if inner.is_empty() {
            return None;
        }
        return Some(inner.replace("\"\"", "\""));
    }

    let mut characters = text.chars();
    let first = characters.next()?;
    if first != '_' && !first.is_alphabetic() {
        return None;
    }
    if characters.any(|character| character != '_' && !character.is_alphanumeric()) {
        return None;
    }
    Some(text.to_lowercase())
}

fn validate_client_capability<'a>(
    capability: &CapabilitySpecification,
    declared_parameters: impl IntoIterator<Item = &'a str>,
    logical_path: &str,
    declaration_span: &SourceSpan,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) {
    let name = semantic_name(&capability.name);
    let Some(entry) = client_capability_entry(&name) else {
        diagnostics.push(diagnostic(
            DiagnosticCode::CapabilityRequirement,
            format!("unknown CLIENT capability {name}"),
            logical_path,
            declaration_span,
        ));
        return;
    };

    let argument_count = client_capability_argument_count(capability.arguments.as_ref());
    if argument_count != entry.argument_count {
        diagnostics.push(diagnostic(
            DiagnosticCode::CapabilityRequirement,
            format!(
                "CLIENT capability {name} requires exactly {} {} argument",
                entry.argument_count,
                entry.argument_kind.label()
            ),
            logical_path,
            declaration_span,
        ));
        return;
    }

    let Some(arguments) = capability.arguments.as_ref() else {
        diagnostics.push(diagnostic(
            DiagnosticCode::CapabilityRequirement,
            format!(
                "CLIENT capability {name} requires one {} argument",
                entry.argument_kind.label()
            ),
            logical_path,
            declaration_span,
        ));
        return;
    };
    let Some(argument) = parse_client_capability_argument(&arguments.text) else {
        diagnostics.push(diagnostic(
            DiagnosticCode::CapabilityRequirement,
            format!(
                "CLIENT capability {name} argument must be a text literal or declared parameter"
            ),
            logical_path,
            declaration_span,
        ));
        return;
    };
    if let ClientCapabilityArgument::Parameter(parameter) = argument
        && !declared_parameters
            .into_iter()
            .any(|declared| declared == parameter)
    {
        diagnostics.push(diagnostic(
            DiagnosticCode::CapabilityRequirement,
            format!(
                "CLIENT capability {name} argument references undeclared parameter {parameter}"
            ),
            logical_path,
            declaration_span,
        ));
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClientExpressionResultShape {
    Value,
    OptionalList,
}

#[derive(Clone, Copy)]
struct ClientExpressionType {
    semantic_type: SemanticType<CheckedTypeId>,
    standard_value_type: Option<orna_core::TypeId>,
    result_shape: ClientExpressionResultShape,
}

#[derive(Clone)]
struct ClientLocalBinding {
    checked: CheckedClientExpression,
    expression_type: ClientExpressionType,
    // Procedural locals are read by ordinal. Legacy state-block locals keep
    // their old substitution behaviour and therefore have no ordinal.
    ordinal: Option<u32>,
    kind: CheckedClientLocalKind,
}

type ClientLocalEnvironment = HashMap<String, ClientLocalBinding>;

#[derive(Clone)]
struct ClientExpressionParameter {
    id: CheckedParameterId,
    name: String,
    expression_type: ClientExpressionType,
}

#[derive(Clone)]
struct ClientExpressionTarget {
    id: CheckedFunctionId,
    parameters: Vec<ClientExpressionParameter>,
    return_type: ClientExpressionType,
}

#[derive(Clone)]
struct ClientActionTarget {
    domain: orna_artifact::client_plan::ActionTargetDomain,
    id: CheckedFunctionId,
    parameters: Vec<ClientExpressionParameter>,
    return_type: ClientExpressionType,
}

fn action_result_type_is_durable(
    result_type: ClientExpressionType,
    standard: Option<&CheckedStandardLibrary>,
) -> bool {
    matches!(
        result_type.semantic_type,
        SemanticType::Scalar(
            StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::Float
                | StandardScalar::CharacterLargeObject
                | StandardScalar::BinaryLargeObject,
        ) if result_type.standard_value_type.is_some()
    ) || matches!(result_type.semantic_type, SemanticType::Reference { .. })
        || matches!(
        result_type.semantic_type,
        SemanticType::Named(CheckedTypeId::Existing(type_id))
            if standard.is_some_and(|standard| {
                standard
                    .verified_snapshot()
                    .catalogue()
                    .value_type_by_id(type_id)
                    .is_some_and(|value_type| {
                        value_type.persistence() == ValueTypePersistence::Persistable
                            || type_id == STD_ACTION_TYPE_ID
                    })
            })
        )
}

fn client_action_result_type(
    result_type: ClientExpressionType,
    standard: Option<&CheckedStandardLibrary>,
) -> ClientExpressionType {
    if result_type.standard_value_type.is_some() {
        return result_type;
    }
    let SemanticType::Named(CheckedTypeId::Existing(type_id)) = result_type.semantic_type else {
        return result_type;
    };
    if standard.is_some_and(|standard| {
        standard
            .verified_snapshot()
            .catalogue()
            .value_type_by_id(type_id)
            .is_some()
    }) {
        ClientExpressionType {
            standard_value_type: Some(type_id),
            ..result_type
        }
    } else {
        result_type
    }
}

fn action_argument_type_is_orv3_encodable(
    expression_type: ClientExpressionType,
    base: &CatalogueSnapshot,
    standard: Option<&CheckedStandardLibrary>,
) -> bool {
    matches!(
        expression_type.semantic_type,
        SemanticType::Scalar(
            StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::Float
                | StandardScalar::CharacterLargeObject
                | StandardScalar::BinaryLargeObject
        ) if expression_type.standard_value_type.is_some()
    ) || matches!(
        expression_type.semantic_type,
        SemanticType::Reference { .. }
    ) || matches!(
        expression_type.semantic_type,
        SemanticType::Named(type_id)
            if match type_id {
                CheckedTypeId::Provisional(_) => true,
                CheckedTypeId::Existing(type_id) => {
                    base.enum_type_by_id(type_id).is_some()
                        || base.record_value_type_by_id(type_id).is_some()
                        || standard.is_some_and(|standard| {
                            let catalogue = standard.verified_snapshot().catalogue();
                            catalogue.enum_type_by_id(type_id).is_some()
                                || catalogue.record_value_type_by_id(type_id).is_some()
                        })
                }
            }
    )
}

fn client_expression_contains_await_or_resource(
    expression: &CheckedClientExpression,
    locals: &ClientLocalEnvironment,
) -> bool {
    match expression {
        CheckedClientExpression::Await { .. } | CheckedClientExpression::Resource { .. } => true,
        CheckedClientExpression::Call { arguments, .. } => arguments
            .iter()
            .any(|(_, argument)| client_expression_contains_await_or_resource(argument, locals)),
        CheckedClientExpression::Action { operation } => operation
            .arguments()
            .iter()
            .any(|(_, argument)| client_expression_contains_await_or_resource(argument, locals)),
        CheckedClientExpression::Inspect { operation } => match operation {
            CheckedInspectOperation::Snapshot {
                target, options, ..
            } => {
                client_expression_contains_await_or_resource(target, locals)
                    || options.as_deref().is_some_and(|options| {
                        client_expression_contains_await_or_resource(options, locals)
                    })
            }
            CheckedInspectOperation::Projection { snapshot, .. } => {
                client_expression_contains_await_or_resource(snapshot, locals)
            }
        },
        CheckedClientExpression::Concat { left, right, .. }
        | CheckedClientExpression::Binary { left, right, .. } => {
            client_expression_contains_await_or_resource(left, locals)
                || client_expression_contains_await_or_resource(right, locals)
        }
        CheckedClientExpression::Unary { expression, .. }
        | CheckedClientExpression::Parenthesized { expression, .. } => {
            client_expression_contains_await_or_resource(expression, locals)
        }
        CheckedClientExpression::LocalRead { local, .. } => locals.values().any(|binding| {
            binding.ordinal == Some(*local)
                && matches!(binding.kind, CheckedClientLocalKind::Resource(_))
        }),
        CheckedClientExpression::SourceIntrospection { .. }
        | CheckedClientExpression::Input { .. }
        | CheckedClientExpression::Evaluate { .. }
        | CheckedClientExpression::String { .. }
        | CheckedClientExpression::Integer { .. }
        | CheckedClientExpression::Boolean { .. }
        | CheckedClientExpression::ParameterRead { .. }
        | CheckedClientExpression::FieldPath { .. } => false,
    }
}
fn client_expression_contains_inspect(expression: &CheckedClientExpression) -> bool {
    match expression {
        CheckedClientExpression::Inspect { .. } => true,
        CheckedClientExpression::Await { expression, .. } => {
            client_expression_contains_inspect(expression)
        }
        CheckedClientExpression::Call { arguments, .. } => arguments
            .iter()
            .any(|(_, argument)| client_expression_contains_inspect(argument)),
        CheckedClientExpression::Resource { operation } => operation
            .arguments()
            .iter()
            .any(|(_, argument)| client_expression_contains_inspect(argument)),
        CheckedClientExpression::Action { operation } => operation
            .arguments()
            .iter()
            .any(|(_, argument)| client_expression_contains_inspect(argument)),

        CheckedClientExpression::Concat { left, right, .. }
        | CheckedClientExpression::Binary { left, right, .. } => {
            client_expression_contains_inspect(left) || client_expression_contains_inspect(right)
        }
        CheckedClientExpression::Unary { expression, .. }
        | CheckedClientExpression::Parenthesized { expression, .. } => {
            client_expression_contains_inspect(expression)
        }
        CheckedClientExpression::SourceIntrospection { .. }
        | CheckedClientExpression::Input { .. }
        | CheckedClientExpression::Evaluate { .. }
        | CheckedClientExpression::String { .. }
        | CheckedClientExpression::Integer { .. }
        | CheckedClientExpression::Boolean { .. }
        | CheckedClientExpression::ParameterRead { .. }
        | CheckedClientExpression::LocalRead { .. }
        | CheckedClientExpression::FieldPath { .. } => false,
    }
}

fn client_expression_contains_action(expression: &ClientExpression) -> bool {
    let action_name =
        QualifiedSemanticName::new(["std", "action", "call"]).expect("std.action.call is valid");
    match expression {
        ClientExpression::Call {
            callee, arguments, ..
        } => {
            semantic_name(callee) == action_name
                || arguments
                    .iter()
                    .any(|argument| client_expression_contains_action(&argument.value))
        }
        ClientExpression::Await { expression, .. } => client_expression_contains_action(expression),
        ClientExpression::Concat { left, right, .. } => {
            client_expression_contains_action(left) || client_expression_contains_action(right)
        }
        ClientExpression::Binary(binary) => {
            client_expression_contains_action(&binary.left)
                || client_expression_contains_action(&binary.right)
        }
        ClientExpression::Unary(unary) => client_expression_contains_action(&unary.expression),
        ClientExpression::Parenthesized { expression, .. } => {
            client_expression_contains_action(expression)
        }
        ClientExpression::StringLiteral { .. }
        | ClientExpression::IntegerLiteral { .. }
        | ClientExpression::BooleanLiteral { .. }
        | ClientExpression::ParameterRead { .. }
        | ClientExpression::LocalRead { .. }
        | ClientExpression::FieldPath { .. } => false,
    }
}

fn action_target_parameters(
    parameters: &[ResolvedServerFunctionParameter],
) -> Option<Vec<ClientExpressionParameter>> {
    parameters
        .iter()
        .map(|parameter| {
            Some(ClientExpressionParameter {
                id: parameter.id,
                name: parameter.name.clone(),
                expression_type: ClientExpressionType {
                    semantic_type: parameter.semantic_type,
                    standard_value_type: parameter.standard_value_type,
                    result_shape: ClientExpressionResultShape::Value,
                },
            })
        })
        .collect()
}

#[derive(Clone)]
struct ClientResourceTarget {
    kind: ResourceKind,
    id: CheckedFunctionId,
    parameters: Vec<ClientExpressionParameter>,
    result_type: ClientExpressionType,
}

fn standard_value_type_scalar(
    standard: Option<&CheckedStandardLibrary>,
    type_id: orna_core::TypeId,
) -> Option<StandardScalar> {
    standard.and_then(|standard| {
        standard
            .value_types()
            .iter()
            .find(|value_type| value_type.id() == type_id)
            .and_then(|value_type| {
                (value_type.kind() == ValueTypeKind::Primitive)
                    .then(|| compatibility_scalar(value_type.representation_contract()))
                    .flatten()
            })
    })
}

fn standard_scalar_type_id(
    standard: Option<&CheckedStandardLibrary>,
    scalar: StandardScalar,
) -> Option<orna_core::TypeId> {
    standard.and_then(|standard| {
        standard.value_types().iter().find_map(|value_type| {
            (value_type.kind() == ValueTypeKind::Primitive
                && compatibility_scalar(value_type.representation_contract()) == Some(scalar))
            .then_some(value_type.id())
        })
    })
}

fn client_expression_type_from_core(
    resolved_type: ResolvedType,
    standard: Option<&CheckedStandardLibrary>,
) -> Option<ClientExpressionType> {
    client_expression_type_from_core_with_shape(
        resolved_type,
        standard,
        ClientExpressionResultShape::Value,
    )
}

fn client_expression_type_from_core_with_shape(
    resolved_type: ResolvedType,
    standard: Option<&CheckedStandardLibrary>,
    result_shape: ClientExpressionResultShape,
) -> Option<ClientExpressionType> {
    let (semantic_type, standard_value_type) = match resolved_type {
        ResolvedType::Scalar(scalar) => (
            SemanticType::scalar(scalar),
            standard_scalar_type_id(standard, scalar),
        ),
        ResolvedType::Named(type_id) => {
            (SemanticType::Named(CheckedTypeId::Existing(type_id)), None)
        }
        ResolvedType::Reference { target } => (
            SemanticType::reference(CheckedTypeId::Existing(target)),
            None,
        ),
        ResolvedType::Value(type_id) => {
            let scalar = standard_value_type_scalar(standard, type_id);
            (
                scalar.map_or(
                    SemanticType::Named(CheckedTypeId::Existing(type_id)),
                    SemanticType::scalar,
                ),
                scalar.map(|_| type_id),
            )
        }
    };
    Some(ClientExpressionType {
        semantic_type,
        standard_value_type,
        result_shape,
    })
}

fn client_expression_targets(
    inputs: &[ResolvedClientFunctionInput<'_>],
    base: &CatalogueSnapshot,
    standard: Option<&CheckedStandardLibrary>,
) -> HashMap<QualifiedSemanticName, ClientExpressionTarget> {
    let mut targets = HashMap::new();
    for input in inputs {
        targets.insert(
            input.name.clone(),
            ClientExpressionTarget {
                id: input.id,
                parameters: input
                    .parameters
                    .iter()
                    .map(|parameter| ClientExpressionParameter {
                        id: parameter.id,
                        name: parameter.name.clone(),
                        expression_type: ClientExpressionType {
                            semantic_type: parameter.semantic_type,
                            standard_value_type: parameter.standard_value_type,
                            result_shape: ClientExpressionResultShape::Value,
                        },
                    })
                    .collect(),
                return_type: ClientExpressionType {
                    semantic_type: input.return_type,
                    standard_value_type: input.standard_value_type,
                    result_shape: input.result_shape,
                },
            },
        );
    }
    let standard_functions =
        standard.map(|standard| standard.verified_snapshot().catalogue().functions());
    // Application CLIENT declarations and functions take precedence over a
    // same-named standard target; the standard target is only a fallback.
    for functions in [Some(base.functions()), standard_functions] {
        let Some(functions) = functions else {
            continue;
        };
        for function in functions {
            if function.domain() != FunctionDomain::Client || targets.contains_key(function.name())
            {
                continue;
            }
            let Some(return_type) = (match function.return_type() {
                FunctionReturn::Single(resolved_type) => {
                    client_expression_type_from_core(*resolved_type, standard)
                }
                FunctionReturn::Stream(resolved_type) => {
                    client_expression_type_from_core_with_shape(
                        *resolved_type,
                        standard,
                        ClientExpressionResultShape::OptionalList,
                    )
                }
                FunctionReturn::Rows(_) => None,
            }) else {
                continue;
            };
            let Some(parameters) = function
                .parameters()
                .iter()
                .map(|parameter| {
                    client_expression_type_from_core(parameter.resolved_type(), standard).map(
                        |expression_type| ClientExpressionParameter {
                            id: CheckedParameterId::Existing(parameter.id()),
                            name: parameter.name().to_owned(),
                            expression_type,
                        },
                    )
                })
                .collect::<Option<Vec<_>>>()
            else {
                // An unrepresentable parameter must not disappear from the
                // target signature and make an incomplete call look bound.
                continue;
            };
            targets.insert(
                function.name().clone(),
                ClientExpressionTarget {
                    id: CheckedFunctionId::Existing(function.id()),
                    parameters,
                    return_type,
                },
            );
        }
    }
    targets
}

fn client_action_targets(
    client_inputs: &[ResolvedClientFunctionInput<'_>],
    server_inputs: &[ResolvedServerFunctionInput<'_>],
    base: &CatalogueSnapshot,
    standard: Option<&CheckedStandardLibrary>,
) -> HashMap<QualifiedSemanticName, ClientActionTarget> {
    let mut targets = HashMap::new();
    for input in client_inputs {
        // ADR 0079 defers stream actions. Client stream functions are valid
        // expression producers, but they are not action targets until the
        // action protocol has an explicit stream result contract.
        if input.result_shape != ClientExpressionResultShape::Value {
            continue;
        }
        let return_type = client_action_result_type(
            ClientExpressionType {
                semantic_type: input.return_type,
                standard_value_type: input.standard_value_type,
                result_shape: input.result_shape,
            },
            standard,
        );
        if !action_result_type_is_durable(return_type, standard) {
            continue;
        }
        let Some(parameters) = action_target_parameters(&input.parameters) else {
            continue;
        };
        targets.insert(
            input.name.clone(),
            ClientActionTarget {
                domain: orna_artifact::client_plan::ActionTargetDomain::Client,
                id: input.id,
                parameters,
                return_type,
            },
        );
    }
    for input in server_inputs {
        let return_type = client_action_result_type(
            match input.return_type {
                ResolvedServerFunctionReturn::Single {
                    semantic_type,
                    standard_value_type,
                    ..
                } => ClientExpressionType {
                    semantic_type,
                    standard_value_type,
                    result_shape: ClientExpressionResultShape::Value,
                },
                ResolvedServerFunctionReturn::Rows { .. }
                | ResolvedServerFunctionReturn::Stream { .. } => continue,
            },
            standard,
        );
        if !action_result_type_is_durable(return_type, standard) {
            continue;
        }
        let Some(parameters) = action_target_parameters(&input.parameters) else {
            continue;
        };
        targets.insert(
            input.name.clone(),
            ClientActionTarget {
                domain: orna_artifact::client_plan::ActionTargetDomain::Server,
                id: input.id,
                parameters,
                return_type,
            },
        );
    }
    let standard_functions =
        standard.map(|standard| standard.verified_snapshot().catalogue().functions());
    // Keep application precedence so a target name resolves to one catalogue identity.
    for functions in [Some(base.functions()), standard_functions] {
        let Some(functions) = functions else {
            continue;
        };
        for function in functions {
            if targets.contains_key(function.name()) {
                continue;
            }
            let return_type = match function.return_type() {
                FunctionReturn::Single(resolved) => {
                    client_expression_type_from_core(*resolved, standard)
                }
                // Action execution rejects ROWS and STREAM results (ADR 0079),
                // including one-column ROWS that could otherwise look scalar.
                FunctionReturn::Rows(_) | FunctionReturn::Stream(_) => None,
            };
            let Some(return_type) = return_type
                .map(|value| client_action_result_type(value, standard))
                .filter(|value| action_result_type_is_durable(*value, standard))
            else {
                continue;
            };
            let Some(parameters) = function
                .parameters()
                .iter()
                .map(|parameter| {
                    client_expression_type_from_core(parameter.resolved_type(), standard).map(
                        |expression_type| ClientExpressionParameter {
                            id: CheckedParameterId::Existing(parameter.id()),
                            name: parameter.name().to_owned(),
                            expression_type,
                        },
                    )
                })
                .collect::<Option<Vec<_>>>()
            else {
                continue;
            };
            targets.insert(
                function.name().clone(),
                ClientActionTarget {
                    domain: match function.domain() {
                        FunctionDomain::Client => {
                            orna_artifact::client_plan::ActionTargetDomain::Client
                        }
                        FunctionDomain::Server => {
                            orna_artifact::client_plan::ActionTargetDomain::Server
                        }
                    },
                    id: CheckedFunctionId::Existing(function.id()),
                    parameters,
                    return_type,
                },
            );
        }
    }
    targets
}

fn client_resource_call_site_id(
    location: &SourceLocation,
    owner: &QualifiedSemanticName,
) -> CallSiteId {
    let path = location.logical_path().as_bytes();
    let mut payload = Vec::with_capacity(path.len() + 32);
    payload.extend_from_slice(&(path.len() as u64).to_be_bytes());
    payload.extend_from_slice(path);
    payload.extend_from_slice(&(location.span().start() as u64).to_be_bytes());
    payload.extend_from_slice(&(location.span().end() as u64).to_be_bytes());
    // A call-site identifies the compiled source location in its owning
    // CLIENT function. The target and its revision are separate resource
    // identity fields, so retargeting does not change the call-site identity.
    let owner = owner.to_string();
    payload.extend_from_slice(&(owner.len() as u64).to_be_bytes());
    payload.extend_from_slice(owner.as_bytes());
    let digest = artifact_payload_digest(&payload).expect("resource call-site payload is bounded");
    CallSiteId::from_bytes(
        digest.to_bytes()[..16]
            .try_into()
            .expect("digest has 16-byte prefix"),
    )
}

/// Returns whether a STREAM item can be materialised as the runtime
/// canonical `OPTION<LIST<T>>` resource value.
///
/// The client runtime collection representation admits the six legacy scalar
/// values, active enum/record identities, and active object references.
/// Other scalar identities and opaque values may be valid function types but
/// cannot be represented inside the list descriptor used for stream batches.
fn client_resource_stream_type_is_supported(
    expression_type: ClientExpressionType,
    base: &CatalogueSnapshot,
    standard: Option<&CheckedStandardLibrary>,
) -> bool {
    match expression_type.semantic_type {
        SemanticType::Scalar(scalar) => matches!(
            scalar,
            StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::Float
                | StandardScalar::CharacterLargeObject
                | StandardScalar::BinaryLargeObject
        ),
        SemanticType::Named(CheckedTypeId::Provisional(_)) => true,
        SemanticType::Reference {
            target: CheckedTypeId::Provisional(_),
        } => true,
        SemanticType::Named(CheckedTypeId::Existing(type_id)) => {
            base.enum_type_by_id(type_id).is_some()
                || base.record_value_type_by_id(type_id).is_some()
                || standard.is_some_and(|standard| {
                    let catalogue = standard.verified_snapshot().catalogue();
                    catalogue.enum_type_by_id(type_id).is_some()
                        || catalogue.record_value_type_by_id(type_id).is_some()
                })
        }
        SemanticType::Reference {
            target: CheckedTypeId::Existing(type_id),
        } => {
            base.object_type_by_id(type_id).is_some()
                || standard.is_some_and(|standard| {
                    standard
                        .verified_snapshot()
                        .catalogue()
                        .object_type_by_id(type_id)
                        .is_some()
                })
        }
    }
}

fn client_resource_targets(
    inputs: &[ResolvedServerFunctionInput<'_>],
    base: &CatalogueSnapshot,
    standard: Option<&CheckedStandardLibrary>,
) -> HashMap<QualifiedSemanticName, ClientResourceTarget> {
    let mut targets = HashMap::new();
    for input in inputs {
        let (kind, result_type) = match &input.return_type {
            ResolvedServerFunctionReturn::Single {
                semantic_type,
                standard_value_type,
                ..
            } => (
                ResourceKind::Scalar,
                ClientExpressionType {
                    semantic_type: *semantic_type,
                    standard_value_type: *standard_value_type,
                    result_shape: ClientExpressionResultShape::Value,
                },
            ),
            ResolvedServerFunctionReturn::Stream {
                semantic_type,
                standard_value_type,
                ..
            } => (
                ResourceKind::Stream,
                ClientExpressionType {
                    semantic_type: *semantic_type,
                    standard_value_type: *standard_value_type,
                    result_shape: ClientExpressionResultShape::Value,
                },
            ),
            ResolvedServerFunctionReturn::Rows { .. } => continue,
        };
        if kind == ResourceKind::Stream
            && !client_resource_stream_type_is_supported(result_type, base, standard)
        {
            continue;
        }
        targets.insert(
            input.name.clone(),
            ClientResourceTarget {
                kind,
                id: input.id,
                parameters: input
                    .parameters
                    .iter()
                    .map(|parameter| ClientExpressionParameter {
                        id: parameter.id,
                        name: parameter.name.clone(),
                        expression_type: ClientExpressionType {
                            semantic_type: parameter.semantic_type,
                            standard_value_type: parameter.standard_value_type,
                            result_shape: ClientExpressionResultShape::Value,
                        },
                    })
                    .collect(),
                result_type,
            },
        );
    }
    let standard_functions =
        standard.map(|standard| standard.verified_snapshot().catalogue().functions());
    for (is_standard, functions) in [(false, Some(base.functions())), (true, standard_functions)] {
        let Some(functions) = functions else {
            continue;
        };
        for function in functions {
            // Standard resource execution is intentionally closed to the one
            // executor currently implemented by the client resource path.
            // Return shape alone must not admit presenters or future functions.
            if is_standard && function.id() != STD_INVOKE_ECHO_FUNCTION_ID {
                continue;
            }
            if function.domain() != FunctionDomain::Server || targets.contains_key(function.name())
            {
                continue;
            }
            let (kind, result_type) = match function.return_type() {
                FunctionReturn::Single(resolved) => (
                    ResourceKind::Scalar,
                    client_expression_type_from_core(*resolved, standard),
                ),
                FunctionReturn::Stream(resolved) => (
                    ResourceKind::Stream,
                    client_expression_type_from_core(*resolved, standard),
                ),
                FunctionReturn::Rows(_) => continue,
            };
            let Some(result_type) = result_type else {
                continue;
            };
            if kind == ResourceKind::Stream
                && !client_resource_stream_type_is_supported(result_type, base, standard)
            {
                continue;
            }
            let Some(parameters) = function
                .parameters()
                .iter()
                .map(|parameter| {
                    client_expression_type_from_core(parameter.resolved_type(), standard).map(
                        |expression_type| ClientExpressionParameter {
                            id: CheckedParameterId::Existing(parameter.id()),
                            name: parameter.name().to_owned(),
                            expression_type,
                        },
                    )
                })
                .collect::<Option<Vec<_>>>()
            else {
                // An unrepresentable parameter must not disappear from the
                // target signature and make an incomplete call look bound.
                continue;
            };
            targets.insert(
                function.name().clone(),
                ClientResourceTarget {
                    kind,
                    id: CheckedFunctionId::Existing(function.id()),
                    parameters,
                    result_type,
                },
            );
        }
    }
    targets
}

fn client_expression_type_is_evaluable(
    expression_type: ClientExpressionType,
    base: &CatalogueSnapshot,
    standard: Option<&CheckedStandardLibrary>,
) -> bool {
    if expression_type.result_shape == ClientExpressionResultShape::OptionalList {
        return client_resource_stream_type_is_supported(expression_type, base, standard);
    }
    match expression_type.semantic_type {
        SemanticType::Scalar(scalar) => matches!(
            scalar,
            StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::Float
                | StandardScalar::CharacterLargeObject
                | StandardScalar::BinaryLargeObject
        ),
        SemanticType::Named(CheckedTypeId::Existing(type_id))
            if is_sealed_inspect_type_id(type_id)
                || type_id == SYS_SOURCE_FUNCTION_TYPE_ID
                || type_id == STD_UI_TYPE_ID =>
        {
            true
        }
        SemanticType::Named(CheckedTypeId::Existing(type_id)) => standard
            .and_then(|standard| {
                standard
                    .value_types()
                    .iter()
                    .find(|value_type| value_type.id() == type_id)
            })
            .is_some_and(|value_type| {
                value_type.kind() == ValueTypeKind::Opaque
                    || matches!(
                        value_type.representation_contract(),
                        "orna.kernel.value.boolean@1"
                            | "orna.kernel.value.integer@1"
                            | "orna.kernel.value.bigint@1"
                            | "orna.kernel.value.float@1"
                            | "orna.kernel.value.character-large-object@1"
                            | "orna.kernel.value.binary-large-object@1"
                    )
            }),
        SemanticType::Named(CheckedTypeId::Provisional(_)) | SemanticType::Reference { .. } => {
            false
        }
    }
}

fn client_expression_types_compatible(
    actual: ClientExpressionType,
    expected: ClientExpressionType,
) -> bool {
    let named_standard_alias = matches!(
        (
            actual.semantic_type,
            expected.semantic_type,
            actual.standard_value_type,
            expected.standard_value_type,
        ),
        (
            SemanticType::Named(CheckedTypeId::Existing(actual_id)),
            SemanticType::Scalar(_),
            None,
            Some(expected_id),
        ) if actual_id == expected_id
    );
    (actual.semantic_type == expected.semantic_type || named_standard_alias)
        && actual.result_shape == expected.result_shape
        && (expected.standard_value_type.is_none()
            || actual.standard_value_type == expected.standard_value_type
            || named_standard_alias)
}

fn resource_constructor_kind(name: &QualifiedSemanticName) -> Option<ResourceKind> {
    if name == &QualifiedSemanticName::new(["std", "data", "resource"]).ok()? {
        Some(ResourceKind::Scalar)
    } else if name == &QualifiedSemanticName::new(["std", "data", "stream_resource"]).ok()? {
        Some(ResourceKind::Stream)
    } else {
        None
    }
}

#[allow(clippy::too_many_arguments)]
fn check_resource_constructor(
    expression: &ClientExpression,
    input: &ResolvedClientFunctionInput<'_>,
    targets: &HashMap<QualifiedSemanticName, ClientExpressionTarget>,
    action_targets: &HashMap<QualifiedSemanticName, ClientActionTarget>,
    resource_targets: &HashMap<QualifiedSemanticName, ClientResourceTarget>,
    query_catalogue: &ResolutionCatalogue<CheckedTypeId, CheckedFieldId>,
    base: &CatalogueSnapshot,
    server_names: &[QualifiedSemanticName],
    standard: Option<&CheckedStandardLibrary>,
    diagnostics: &mut Vec<CompilerDiagnostic>,
    references: &mut Vec<CheckedDefinitionReference>,
    used_capabilities: &mut HashSet<QualifiedSemanticName>,
    locals: &ClientLocalEnvironment,
) -> Option<(CheckedClientExpression, ClientExpressionType)> {
    if let ClientExpression::LocalRead { local } = expression {
        let name = semantic_part(local);
        let Some(binding) = locals.get(&name) else {
            diagnostics.push(diagnostic(
                DiagnosticCode::UnknownQualifiedName,
                format!("unknown CLIENT local {name}"),
                input.logical_path,
                &local.span,
            ));
            return None;
        };
        if !matches!(binding.kind, CheckedClientLocalKind::Resource(..)) {
            diagnostics.push(diagnostic(
                DiagnosticCode::DomainIncompatible,
                format!("CLIENT local {name} is not a resource"),
                input.logical_path,
                &local.span,
            ));
            return None;
        }
        return Some((
            binding.ordinal.map_or_else(
                || binding.checked.clone(),
                |ordinal| CheckedClientExpression::LocalRead {
                    local: ordinal,
                    location: location(input.logical_path, &local.span),
                },
            ),
            binding.expression_type,
        ));
    }
    let ClientExpression::Call {
        callee,
        arguments,
        span,
    } = expression
    else {
        diagnostics.push(diagnostic(
            DiagnosticCode::DomainIncompatible,
            "AWAIT requires a std.data.resource or std.data.stream_resource constructor",
            input.logical_path,
            expression.span(),
        ));
        return None;
    };
    let constructor_name = semantic_name(callee);
    let Some(kind) = resource_constructor_kind(&constructor_name) else {
        diagnostics.push(diagnostic(
            DiagnosticCode::DomainIncompatible,
            "AWAIT operand must be a resource constructor",
            input.logical_path,
            span,
        ));
        return None;
    };
    if arguments.len() != 2 {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            "resource constructor requires exactly one target and one arguments value",
            input.logical_path,
            span,
        ));
        return None;
    }
    let mut target_expression = None;
    let mut arguments_expression = None;
    for argument in arguments {
        let Some(name) = &argument.name else {
            diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                "resource constructor arguments must be named target and arguments",
                input.logical_path,
                &argument.span,
            ));
            return None;
        };
        match semantic_part(name).as_str() {
            "target" if target_expression.is_none() => target_expression = Some(&argument.value),
            "arguments" if arguments_expression.is_none() => {
                arguments_expression = Some(&argument.value)
            }
            "target" | "arguments" => {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DuplicateDefinition,
                    "duplicate resource constructor argument",
                    input.logical_path,
                    &argument.span,
                ));
                return None;
            }
            _ => {
                diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    "resource constructor accepts only target and arguments",
                    input.logical_path,
                    &argument.span,
                ));
                return None;
            }
        }
    }
    let (Some(target_expression), Some(arguments_expression)) =
        (target_expression, arguments_expression)
    else {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            "resource constructor requires both target and arguments",
            input.logical_path,
            span,
        ));
        return None;
    };
    let ClientExpression::FieldPath { root, members, .. } = target_expression else {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            "resource target must be a qualified SERVER function name",
            input.logical_path,
            target_expression.span(),
        ));
        return None;
    };
    if members.is_empty() {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            "resource target must include a schema and function name",
            input.logical_path,
            target_expression.span(),
        ));
        return None;
    }
    let mut target_parts = Vec::with_capacity(members.len() + 1);
    target_parts.push(semantic_part(root));
    target_parts.extend(members.iter().map(semantic_part));
    let Ok(target_name) = QualifiedSemanticName::new(target_parts) else {
        diagnostics.push(diagnostic(
            DiagnosticCode::UnknownQualifiedName,
            "resource target must be a qualified SERVER function name",
            input.logical_path,
            target_expression.span(),
        ));
        return None;
    };
    let Some(target) = resource_targets.get(&target_name) else {
        let message = if targets.contains_key(&target_name)
            || base
                .function_by_name(&target_name)
                .is_some_and(|function| function.domain() == FunctionDomain::Client)
        {
            format!("resource target {target_name} must be a SERVER function")
        } else {
            format!("unknown SERVER resource target {target_name}")
        };
        diagnostics.push(diagnostic(
            if targets.contains_key(&target_name)
                || base
                    .function_by_name(&target_name)
                    .is_some_and(|function| function.domain() == FunctionDomain::Client)
            {
                DiagnosticCode::DomainIncompatible
            } else {
                DiagnosticCode::UnknownQualifiedName
            },
            message,
            input.logical_path,
            target_expression.span(),
        ));
        return None;
    };
    if target.kind != kind {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            format!("resource constructor kind does not match SERVER target {target_name}"),
            input.logical_path,
            target_expression.span(),
        ));
        return None;
    }
    let ClientExpression::Call {
        callee: args_callee,
        arguments: target_arguments,
        ..
    } = arguments_expression
    else {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            "resource arguments must be a std.call.args value",
            input.logical_path,
            arguments_expression.span(),
        ));
        return None;
    };
    if semantic_name(args_callee)
        != QualifiedSemanticName::new(["std", "call", "args"]).expect("std.call.args is valid")
    {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            "resource arguments must be a std.call.args value",
            input.logical_path,
            arguments_expression.span(),
        ));
        return None;
    }
    let mut bound = vec![false; target.parameters.len()];
    let mut positional = 0usize;
    let mut checked_arguments = Vec::with_capacity(target_arguments.len());
    for argument in target_arguments {
        let parameter_index = if let Some(name) = &argument.name {
            let parameter_name = semantic_part(name);
            let Some(index) = target
                .parameters
                .iter()
                .position(|parameter| parameter.name == parameter_name)
            else {
                diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    format!("unknown SERVER resource parameter {parameter_name}"),
                    input.logical_path,
                    &argument.span,
                ));
                return None;
            };
            index
        } else {
            while positional < bound.len() && bound[positional] {
                positional += 1;
            }
            if positional >= bound.len() {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    format!("too many arguments for SERVER resource target {target_name}"),
                    input.logical_path,
                    &argument.span,
                ));
                return None;
            }
            let index = positional;
            positional += 1;
            index
        };
        if bound[parameter_index] {
            diagnostics.push(diagnostic(
                DiagnosticCode::DuplicateDefinition,
                format!(
                    "duplicate SERVER resource parameter {}",
                    target.parameters[parameter_index].name
                ),
                input.logical_path,
                &argument.span,
            ));
            return None;
        }
        let (checked, expression_type) = check_client_expression(
            &argument.value,
            input,
            targets,
            action_targets,
            resource_targets,
            query_catalogue,
            base,
            server_names,
            standard,
            diagnostics,
            references,
            used_capabilities,
            locals,
        )?;
        let parameter = &target.parameters[parameter_index];
        if !client_expression_types_compatible(expression_type, parameter.expression_type) {
            diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                format!(
                    "resource argument does not match SERVER parameter {}",
                    parameter.name
                ),
                input.logical_path,
                &argument.span,
            ));
            return None;
        }
        bound[parameter_index] = true;
        checked_arguments.push((parameter.id, checked));
    }
    if bound.iter().any(|bound| !bound) {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            format!("missing argument for SERVER resource target {target_name}"),
            input.logical_path,
            span,
        ));
        return None;
    }
    checked_arguments.sort_by_key(|(parameter, _)| *parameter);
    let operation_location = location(input.logical_path, span);
    let call_site = client_resource_call_site_id(&operation_location, &input.name);
    references.push(CheckedDefinitionReference {
        target: CheckedDefinitionReferenceTarget::Function(target.id),
        kind: DefinitionReferenceKind::FunctionCall,
        location: operation_location.clone(),
    });
    let operation = CheckedResourceOperation {
        kind,
        target: target.id,
        call_site,
        arguments: checked_arguments,
        result_type: target.result_type.semantic_type,
        standard_result_type: target.result_type.standard_value_type,
        location: operation_location,
    };
    Some((
        CheckedClientExpression::Resource { operation },
        target.result_type,
    ))
}

#[allow(clippy::too_many_arguments)]
fn check_action_constructor(
    expression: &ClientExpression,
    input: &ResolvedClientFunctionInput<'_>,
    targets: &HashMap<QualifiedSemanticName, ClientExpressionTarget>,
    action_targets: &HashMap<QualifiedSemanticName, ClientActionTarget>,
    resource_targets: &HashMap<QualifiedSemanticName, ClientResourceTarget>,
    query_catalogue: &ResolutionCatalogue<CheckedTypeId, CheckedFieldId>,
    base: &CatalogueSnapshot,
    server_names: &[QualifiedSemanticName],
    standard: Option<&CheckedStandardLibrary>,
    diagnostics: &mut Vec<CompilerDiagnostic>,
    references: &mut Vec<CheckedDefinitionReference>,
    used_capabilities: &mut HashSet<QualifiedSemanticName>,
    locals: &ClientLocalEnvironment,
) -> Option<(CheckedClientExpression, ClientExpressionType)> {
    let action_name =
        QualifiedSemanticName::new(["std", "action", "call"]).expect("std.action.call is valid");
    let Some(action_type) = standard
        .and_then(|standard| {
            standard.value_types().iter().find(|value| {
                value.id() == STD_ACTION_TYPE_ID
                    && value.kind() == ValueTypeKind::Opaque
                    && value.representation_contract() == STD_ACTION_CONTRACT
            })
        })
        .map(|_| ClientExpressionType {
            semantic_type: SemanticType::Named(CheckedTypeId::Existing(STD_ACTION_TYPE_ID)),
            standard_value_type: Some(STD_ACTION_TYPE_ID),
            result_shape: ClientExpressionResultShape::Value,
        })
    else {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            "the checked standard library does not provide std.action.Action",
            input.logical_path,
            expression.span(),
        ));
        return None;
    };
    let ClientExpression::Call {
        callee,
        arguments,
        span,
    } = expression
    else {
        return None;
    };
    if semantic_name(callee) != action_name {
        return None;
    }
    if arguments.len() != 2 {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            "std.action.call requires exactly one target and one arguments value",
            input.logical_path,
            span,
        ));
        return None;
    }
    let mut target_expression = None;
    let mut arguments_expression = None;
    for argument in arguments {
        let Some(name) = &argument.name else {
            diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                "std.action.call arguments must be named target and arguments",
                input.logical_path,
                &argument.span,
            ));
            return None;
        };
        match semantic_part(name).as_str() {
            "target" if target_expression.is_none() => {
                target_expression = Some(&argument.value);
            }
            "arguments" if arguments_expression.is_none() => {
                arguments_expression = Some(&argument.value);
            }
            "target" | "arguments" => {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DuplicateDefinition,
                    "duplicate std.action.call argument",
                    input.logical_path,
                    &argument.span,
                ));
                return None;
            }
            _ => {
                diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    "std.action.call accepts only target and arguments",
                    input.logical_path,
                    &argument.span,
                ));
                return None;
            }
        }
    }
    let (Some(target_expression), Some(arguments_expression)) =
        (target_expression, arguments_expression)
    else {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            "std.action.call requires both target and arguments",
            input.logical_path,
            span,
        ));
        return None;
    };
    let ClientExpression::FieldPath { root, members, .. } = target_expression else {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            "std.action.call target must be a qualified function name",
            input.logical_path,
            target_expression.span(),
        ));
        return None;
    };
    if members.is_empty() {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            "std.action.call target must include a schema and function name",
            input.logical_path,
            target_expression.span(),
        ));
        return None;
    }
    let mut target_parts = Vec::with_capacity(members.len() + 1);
    target_parts.push(semantic_part(root));
    target_parts.extend(members.iter().map(semantic_part));
    let Ok(target_name) = QualifiedSemanticName::new(target_parts) else {
        diagnostics.push(diagnostic(
            DiagnosticCode::UnknownQualifiedName,
            "std.action.call target must be a qualified function name",
            input.logical_path,
            target_expression.span(),
        ));
        return None;
    };
    let Some(target) = action_targets.get(&target_name) else {
        let message = if server_names.contains(&target_name)
            || base.function_by_name(&target_name).is_some()
        {
            format!("std.action.call target {target_name} does not return one durable value")
        } else {
            format!("unknown std.action.call target {target_name}")
        };
        diagnostics.push(diagnostic(
            DiagnosticCode::UnknownQualifiedName,
            message,
            input.logical_path,
            target_expression.span(),
        ));
        return None;
    };
    let ClientExpression::Call {
        callee: args_callee,
        arguments: target_arguments,
        ..
    } = arguments_expression
    else {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            "std.action.call arguments must be a std.call.args value",
            input.logical_path,
            arguments_expression.span(),
        ));
        return None;
    };
    if semantic_name(args_callee)
        != QualifiedSemanticName::new(["std", "call", "args"]).expect("std.call.args is valid")
    {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            "std.action.call arguments must be a std.call.args value",
            input.logical_path,
            arguments_expression.span(),
        ));
        return None;
    }
    if target.parameters.iter().any(|parameter| {
        !action_argument_type_is_orv3_encodable(parameter.expression_type, base, standard)
    }) {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            format!(
                "std.action.call target {target_name} has a parameter that is not ORV3-encodable"
            ),
            input.logical_path,
            target_expression.span(),
        ));
        return None;
    }
    let mut bound = vec![false; target.parameters.len()];
    let mut positional = 0usize;
    let mut checked_arguments = Vec::with_capacity(target_arguments.len());
    for argument in target_arguments {
        let parameter_index = if let Some(name) = &argument.name {
            let parameter_name = semantic_part(name);
            let Some(index) = target
                .parameters
                .iter()
                .position(|parameter| parameter.name == parameter_name)
            else {
                diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    format!("unknown std.action.call parameter {parameter_name}"),
                    input.logical_path,
                    &argument.span,
                ));
                return None;
            };
            index
        } else {
            while positional < bound.len() && bound[positional] {
                positional += 1;
            }
            if positional >= bound.len() {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    format!("too many arguments for std.action.call target {target_name}"),
                    input.logical_path,
                    &argument.span,
                ));
                return None;
            }
            let index = positional;
            positional += 1;
            index
        };
        if bound[parameter_index] {
            diagnostics.push(diagnostic(
                DiagnosticCode::DuplicateDefinition,
                format!(
                    "duplicate std.action.call parameter {}",
                    target.parameters[parameter_index].name
                ),
                input.logical_path,
                &argument.span,
            ));
            return None;
        }
        let (checked, expression_type) = check_client_expression(
            &argument.value,
            input,
            targets,
            action_targets,
            resource_targets,
            query_catalogue,
            base,
            server_names,
            standard,
            diagnostics,
            references,
            used_capabilities,
            locals,
        )?;
        let parameter = &target.parameters[parameter_index];
        if client_expression_contains_await_or_resource(&checked, locals) {
            diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                format!(
                    "std.action.call argument for parameter {} is not ORV3-encodable",
                    parameter.name
                ),
                input.logical_path,
                &argument.span,
            ));
            return None;
        }
        if !client_expression_types_compatible(expression_type, parameter.expression_type) {
            diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                format!(
                    "std.action.call argument does not match parameter {}",
                    parameter.name
                ),
                input.logical_path,
                &argument.span,
            ));
            return None;
        }
        if !action_argument_type_is_orv3_encodable(expression_type, base, standard) {
            diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                format!(
                    "std.action.call argument for parameter {} is not ORV3-encodable",
                    parameter.name
                ),
                input.logical_path,
                &argument.span,
            ));
            return None;
        }
        bound[parameter_index] = true;
        checked_arguments.push((parameter.id, checked));
    }
    if bound.iter().any(|bound| !bound) {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            format!("missing argument for std.action.call target {target_name}"),
            input.logical_path,
            span,
        ));
        return None;
    }
    checked_arguments.sort_by_key(|(parameter, _)| *parameter);
    let operation_location = location(input.logical_path, span);
    references.push(CheckedDefinitionReference {
        target: CheckedDefinitionReferenceTarget::Function(target.id),
        kind: DefinitionReferenceKind::FunctionCall,
        location: operation_location.clone(),
    });
    let operation = CheckedActionOperation {
        target_domain: target.domain,
        target: target.id,
        call_site: client_resource_call_site_id(&operation_location, &input.name),
        arguments: checked_arguments,
        result_type: target.return_type.semantic_type,
        standard_result_type: target.return_type.standard_value_type,
        location: operation_location,
    };
    Some((CheckedClientExpression::Action { operation }, action_type))
}

#[allow(clippy::too_many_arguments)]
fn check_inspect_call(
    expression: &ClientExpression,
    input: &ResolvedClientFunctionInput<'_>,
    targets: &HashMap<QualifiedSemanticName, ClientExpressionTarget>,
    action_targets: &HashMap<QualifiedSemanticName, ClientActionTarget>,
    resource_targets: &HashMap<QualifiedSemanticName, ClientResourceTarget>,
    query_catalogue: &ResolutionCatalogue<CheckedTypeId, CheckedFieldId>,
    base: &CatalogueSnapshot,
    server_names: &[QualifiedSemanticName],
    standard: Option<&CheckedStandardLibrary>,
    diagnostics: &mut Vec<CompilerDiagnostic>,
    references: &mut Vec<CheckedDefinitionReference>,
    used_capabilities: &mut HashSet<QualifiedSemanticName>,
    locals: &ClientLocalEnvironment,
) -> Option<Option<(CheckedClientExpression, ClientExpressionType)>> {
    let ClientExpression::Call {
        callee,
        arguments,
        span,
    } = expression
    else {
        return None;
    };
    let name = semantic_name(callee);
    let system = orna_core::system::system_function_by_name(&name)?;
    let Some(signature) = system.inspect_signature() else {
        return Some(None);
    };

    let projection = match system.id() {
        orna_core::system::SYS_INSPECT_SNAPSHOT_FUNCTION_ID => None,
        orna_core::system::SYS_INSPECT_INVOCATION_NODES_FUNCTION_ID => {
            Some(CheckedInspectProjection::InvocationNodes)
        }
        orna_core::system::SYS_INSPECT_CALLS_FUNCTION_ID => Some(CheckedInspectProjection::Calls),
        orna_core::system::SYS_INSPECT_RESOURCES_FUNCTION_ID => {
            Some(CheckedInspectProjection::Resources)
        }
        orna_core::system::SYS_INSPECT_STATE_CELLS_FUNCTION_ID => {
            Some(CheckedInspectProjection::StateCells)
        }
        orna_core::system::SYS_INSPECT_UI_NODES_FUNCTION_ID => {
            Some(CheckedInspectProjection::UiNodes)
        }
        orna_core::system::SYS_INSPECT_PRESENTATION_CANDIDATES_FUNCTION_ID => {
            Some(CheckedInspectProjection::PresentationCandidates)
        }
        orna_core::system::SYS_INSPECT_RUNTIME_BINDINGS_FUNCTION_ID => {
            Some(CheckedInspectProjection::RuntimeBindings)
        }
        orna_core::system::SYS_INSPECT_SECURITY_DECISIONS_FUNCTION_ID => {
            Some(CheckedInspectProjection::SecurityDecisions)
        }
        _ => {
            diagnostics.push(diagnostic(
                DiagnosticCode::UnknownQualifiedName,
                format!("sealed INSPECT function {name} is not an expression operation"),
                input.logical_path,
                span,
            ));
            return Some(None);
        }
    };

    let (target_argument, options_argument) = if projection.is_none() {
        if arguments.is_empty() || arguments.len() > 2 {
            diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                "sys.inspect.snapshot requires target and optionally p_options",
                input.logical_path,
                span,
            ));
            return Some(None);
        }
        let mut bound = [false; 2];
        let mut target_argument = None;
        let mut options_argument = None;
        for (position, argument) in arguments.iter().enumerate() {
            let index = match argument.name.as_ref().map(semantic_part) {
                Some(argument_name) if argument_name == "p_target" => 0,
                Some(argument_name) if argument_name == "p_options" => 1,
                Some(_) => {
                    diagnostics.push(diagnostic(
                        DiagnosticCode::UnknownQualifiedName,
                        format!("{name} accepts only named arguments p_target and p_options"),
                        input.logical_path,
                        &argument.span,
                    ));
                    return Some(None);
                }
                None => position,
            };
            if bound[index] {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DuplicateDefinition,
                    format!(
                        "duplicate argument for sys.inspect.snapshot parameter {}",
                        if index == 0 { "p_target" } else { "p_options" }
                    ),
                    input.logical_path,
                    &argument.span,
                ));
                return Some(None);
            }
            bound[index] = true;
            if index == 0 {
                target_argument = Some(argument);
            } else {
                options_argument = Some(argument);
            }
        }
        let Some(target_argument) = target_argument else {
            diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                "sys.inspect.snapshot requires p_target",
                input.logical_path,
                span,
            ));
            return Some(None);
        };
        (target_argument, options_argument)
    } else {
        if arguments.len() != 1 {
            diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                "sys.inspect projection requires exactly one snapshot argument",
                input.logical_path,
                span,
            ));
            return Some(None);
        }
        let argument = &arguments[0];
        if let Some(argument_name) = &argument.name
            && semantic_part(argument_name) != "p_snapshot"
        {
            diagnostics.push(diagnostic(
                DiagnosticCode::UnknownQualifiedName,
                format!("{name} accepts only named argument p_snapshot"),
                input.logical_path,
                &argument.span,
            ));
            return Some(None);
        }
        (argument, None)
    };

    let (checked, expression_type) = check_client_expression(
        &target_argument.value,
        input,
        targets,
        action_targets,
        resource_targets,
        query_catalogue,
        base,
        server_names,
        standard,
        diagnostics,
        references,
        used_capabilities,
        locals,
    )?;
    let expected_type = if projection.is_none() {
        SemanticType::reference(CheckedTypeId::Existing(
            orna_core::system::SYS_INSPECT_INVOCATION_TYPE_ID,
        ))
    } else {
        SemanticType::Named(CheckedTypeId::Existing(
            orna_core::system::SYS_INSPECT_SNAPSHOT_TYPE_ID,
        ))
    };
    if expression_type.semantic_type != expected_type
        || expression_type.result_shape != ClientExpressionResultShape::Value
    {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            if projection.is_none() {
                "sys.inspect.snapshot target must be REF sys.inspect.invocation"
            } else {
                "sys.inspect projection argument must be sys.inspect.snapshot"
            },
            input.logical_path,
            target_argument.value.span(),
        ));
        return Some(None);
    }

    if let Some(options_argument) = options_argument {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            "sys.inspect.snapshot options are not supported in Inspector v1",
            input.logical_path,
            options_argument.value.span(),
        ));
        return Some(None);
    }
    let checked_options = None;

    // The registry signature remains authoritative for the sealed operation.
    let valid_signature = if projection.is_none() {
        signature.parameter_count() == 2
            && signature.parameter_type(0)
                == Some(orna_core::system::SYS_INSPECT_INVOCATION_TYPE_ID)
            && signature.parameter_type(1)
                == Some(orna_core::system::SYS_INSPECT_SNAPSHOT_OPTIONS_TYPE_ID)
            && signature.result_type() == Some(orna_core::system::SYS_INSPECT_SNAPSHOT_TYPE_ID)
    } else {
        signature.parameter_count() == 1
            && signature.parameter_type(0) == Some(orna_core::system::SYS_INSPECT_SNAPSHOT_TYPE_ID)
            && signature.result_type().is_some()
    };
    if !valid_signature {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            format!("sealed INSPECT function {name} has an invalid registry signature"),
            input.logical_path,
            span,
        ));
        return Some(None);
    }

    let (operation, result_type) = if let Some(projection) = projection {
        let result_type = match projection {
            CheckedInspectProjection::InvocationNodes => {
                orna_core::system::SYS_INSPECT_INVOCATION_NODES_TYPE_ID
            }
            CheckedInspectProjection::Calls => orna_core::system::SYS_INSPECT_CALLS_TYPE_ID,
            CheckedInspectProjection::Resources => orna_core::system::SYS_INSPECT_RESOURCES_TYPE_ID,
            CheckedInspectProjection::StateCells => {
                orna_core::system::SYS_INSPECT_STATE_CELLS_TYPE_ID
            }
            CheckedInspectProjection::UiNodes => orna_core::system::SYS_INSPECT_UI_NODES_TYPE_ID,
            CheckedInspectProjection::PresentationCandidates => {
                orna_core::system::SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID
            }
            CheckedInspectProjection::RuntimeBindings => {
                orna_core::system::SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID
            }
            CheckedInspectProjection::SecurityDecisions => {
                orna_core::system::SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID
            }
        };
        if signature.result_type() != Some(result_type) {
            diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                format!("sealed INSPECT function {name} has the wrong result carrier"),
                input.logical_path,
                span,
            ));
            return Some(None);
        }
        (
            CheckedInspectOperation::Projection {
                projection,

                snapshot: Box::new(checked),
                location: location(input.logical_path, span),
            },
            result_type,
        )
    } else {
        (
            CheckedInspectOperation::Snapshot {
                target: Box::new(checked),
                options: checked_options,
                location: location(input.logical_path, span),
            },
            orna_core::system::SYS_INSPECT_SNAPSHOT_TYPE_ID,
        )
    };
    Some(Some((
        CheckedClientExpression::Inspect { operation },
        ClientExpressionType {
            semantic_type: SemanticType::Named(CheckedTypeId::Existing(result_type)),
            standard_value_type: None,
            result_shape: ClientExpressionResultShape::Value,
        },
    )))
}
fn checked_client_unary_operator(
    operator: orna_syntax::ClientUnaryOperator,
) -> ControlFlowUnaryOperator {
    match operator {
        orna_syntax::ClientUnaryOperator::Plus => ControlFlowUnaryOperator::Plus,
        orna_syntax::ClientUnaryOperator::Minus => ControlFlowUnaryOperator::Minus,
        orna_syntax::ClientUnaryOperator::Not => ControlFlowUnaryOperator::Not,
    }
}

fn checked_client_binary_operator(
    operator: orna_syntax::ClientBinaryOperator,
) -> ControlFlowBinaryOperator {
    match operator {
        orna_syntax::ClientBinaryOperator::Add => ControlFlowBinaryOperator::Add,
        orna_syntax::ClientBinaryOperator::Subtract => ControlFlowBinaryOperator::Subtract,
        orna_syntax::ClientBinaryOperator::Multiply => ControlFlowBinaryOperator::Multiply,
        orna_syntax::ClientBinaryOperator::Divide => ControlFlowBinaryOperator::Divide,
        orna_syntax::ClientBinaryOperator::Modulo => ControlFlowBinaryOperator::Modulo,
        orna_syntax::ClientBinaryOperator::Equal => ControlFlowBinaryOperator::Equal,
        orna_syntax::ClientBinaryOperator::NotEqual => ControlFlowBinaryOperator::NotEqual,
        orna_syntax::ClientBinaryOperator::LessThan => ControlFlowBinaryOperator::LessThan,
        orna_syntax::ClientBinaryOperator::GreaterThan => ControlFlowBinaryOperator::GreaterThan,
        orna_syntax::ClientBinaryOperator::LessThanOrEqual => {
            ControlFlowBinaryOperator::LessThanOrEqual
        }
        orna_syntax::ClientBinaryOperator::GreaterThanOrEqual => {
            ControlFlowBinaryOperator::GreaterThanOrEqual
        }
        orna_syntax::ClientBinaryOperator::And => ControlFlowBinaryOperator::And,
        orna_syntax::ClientBinaryOperator::Or => ControlFlowBinaryOperator::Or,
    }
}

fn control_flow_supported_scalar(expression_type: ClientExpressionType) -> Option<StandardScalar> {
    if expression_type.result_shape != ClientExpressionResultShape::Value {
        return None;
    }
    match expression_type.semantic_type {
        SemanticType::Scalar(
            scalar @ (StandardScalar::Integer
            | StandardScalar::Boolean
            | StandardScalar::CharacterLargeObject),
        ) => Some(scalar),
        SemanticType::Scalar(_) | SemanticType::Named(_) | SemanticType::Reference { .. } => None,
    }
}

fn control_flow_types_match(left: ClientExpressionType, right: ClientExpressionType) -> bool {
    control_flow_supported_scalar(left).is_some()
        && control_flow_supported_scalar(left) == control_flow_supported_scalar(right)
        && left.standard_value_type == right.standard_value_type
}

#[allow(clippy::too_many_arguments)]
fn check_client_expression(
    expression: &ClientExpression,
    input: &ResolvedClientFunctionInput<'_>,
    targets: &HashMap<QualifiedSemanticName, ClientExpressionTarget>,
    action_targets: &HashMap<QualifiedSemanticName, ClientActionTarget>,
    resource_targets: &HashMap<QualifiedSemanticName, ClientResourceTarget>,
    query_catalogue: &ResolutionCatalogue<CheckedTypeId, CheckedFieldId>,
    base: &CatalogueSnapshot,
    server_names: &[QualifiedSemanticName],
    standard: Option<&CheckedStandardLibrary>,
    diagnostics: &mut Vec<CompilerDiagnostic>,
    references: &mut Vec<CheckedDefinitionReference>,
    used_capabilities: &mut HashSet<QualifiedSemanticName>,
    locals: &ClientLocalEnvironment,
) -> Option<(CheckedClientExpression, ClientExpressionType)> {
    let expression_location = || location(input.logical_path, expression.span());
    match expression {
        ClientExpression::Await { expression, span } => {
            let (checked_resource, result_type) = check_resource_constructor(
                expression,
                input,
                targets,
                action_targets,
                resource_targets,
                query_catalogue,
                base,
                server_names,
                standard,
                diagnostics,
                references,
                used_capabilities,
                locals,
            )?;
            let resource_kind = match &checked_resource {
                CheckedClientExpression::Resource { operation } => Some(operation.kind()),
                CheckedClientExpression::LocalRead { .. } => match expression.as_ref() {
                    ClientExpression::LocalRead { local } => locals
                        .get(&semantic_part(local))
                        .and_then(|binding| match binding.kind {
                            CheckedClientLocalKind::Resource(kind) => Some(kind),
                            CheckedClientLocalKind::Value => None,
                        }),
                    _ => None,
                },
                _ => None,
            };
            let result_shape = if resource_kind == Some(ResourceKind::Stream) {
                ClientExpressionResultShape::OptionalList
            } else {
                ClientExpressionResultShape::Value
            };
            let result_type = ClientExpressionType {
                result_shape,
                ..result_type
            };
            Some((
                CheckedClientExpression::Await {
                    expression: Box::new(checked_resource),
                    location: location(input.logical_path, span),
                },
                result_type,
            ))
        }
        ClientExpression::StringLiteral { value, source } => {
            let expression_type = ClientExpressionType {
                semantic_type: SemanticType::scalar(StandardScalar::CharacterLargeObject),
                standard_value_type: standard_scalar_type_id(
                    standard,
                    StandardScalar::CharacterLargeObject,
                ),
                result_shape: ClientExpressionResultShape::Value,
            };
            Some((
                CheckedClientExpression::String {
                    value: value.clone(),
                    location: location(input.logical_path, &source.span),
                },
                expression_type,
            ))
        }
        ClientExpression::IntegerLiteral { value, source } => {
            if i32::try_from(*value).is_err() {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    "CLIENT integer literal is outside the INTEGER range",
                    input.logical_path,
                    &source.span,
                ));
                return None;
            }
            let expression_type = ClientExpressionType {
                semantic_type: SemanticType::scalar(StandardScalar::Integer),
                standard_value_type: standard_scalar_type_id(standard, StandardScalar::Integer),
                result_shape: ClientExpressionResultShape::Value,
            };
            Some((
                CheckedClientExpression::Integer {
                    value: *value,
                    location: location(input.logical_path, &source.span),
                },
                expression_type,
            ))
        }
        ClientExpression::BooleanLiteral { value, source } => {
            let expression_type = ClientExpressionType {
                semantic_type: SemanticType::scalar(StandardScalar::Boolean),
                standard_value_type: standard_scalar_type_id(standard, StandardScalar::Boolean),
                result_shape: ClientExpressionResultShape::Value,
            };
            Some((
                CheckedClientExpression::Boolean {
                    value: *value,
                    location: location(input.logical_path, &source.span),
                },
                expression_type,
            ))
        }
        ClientExpression::ParameterRead { parameter } => {
            let name = semantic_part(parameter);
            if let Some(binding) = locals.get(&name) {
                return Some((
                    binding.ordinal.map_or_else(
                        || binding.checked.clone(),
                        |ordinal| CheckedClientExpression::LocalRead {
                            local: ordinal,
                            location: expression_location(),
                        },
                    ),
                    binding.expression_type,
                ));
            }
            let Some(parameter) = input
                .parameters
                .iter()
                .find(|parameter| parameter.name == name)
            else {
                diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    format!("unknown CLIENT parameter {name}"),
                    input.logical_path,
                    &parameter.span,
                ));
                return None;
            };
            Some((
                CheckedClientExpression::ParameterRead {
                    parameter: parameter.id,
                    location: expression_location(),
                },
                ClientExpressionType {
                    semantic_type: parameter.semantic_type,
                    standard_value_type: parameter.standard_value_type,
                    result_shape: ClientExpressionResultShape::Value,
                },
            ))
        }
        ClientExpression::LocalRead { local } => {
            let name = semantic_part(local);
            let Some(binding) = locals.get(&name) else {
                diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    format!("unknown CLIENT local {name}"),
                    input.logical_path,
                    &local.span,
                ));
                return None;
            };
            Some((
                binding.ordinal.map_or_else(
                    || binding.checked.clone(),
                    |ordinal| CheckedClientExpression::LocalRead {
                        local: ordinal,
                        location: expression_location(),
                    },
                ),
                binding.expression_type,
            ))
        }
        ClientExpression::FieldPath {
            root,
            members,
            span,
        } => {
            let root_name = semantic_part(root);
            let Some(parameter) = input
                .parameters
                .iter()
                .find(|parameter| parameter.name == root_name)
            else {
                diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    format!("unknown CLIENT parameter {root_name}"),
                    input.logical_path,
                    &root.span,
                ));
                return None;
            };
            let SemanticType::Reference { target } = parameter.semantic_type else {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    "CLIENT field paths require a REF parameter",
                    input.logical_path,
                    span,
                ));
                return None;
            };
            let mut owner = target;
            let mut fields = Vec::with_capacity(members.len());
            let mut expression_type = None;
            for (index, member) in members.iter().enumerate() {
                let field_name = semantic_part(member);
                let Some(field) =
                    QueryCatalogue::field_by_name(query_catalogue, owner, &field_name)
                else {
                    diagnostics.push(diagnostic(
                        DiagnosticCode::UnknownQualifiedName,
                        format!("unknown field {field_name} in CLIENT field path"),
                        input.logical_path,
                        &member.span,
                    ));
                    return None;
                };
                fields.push(field.id());
                expression_type = Some(ClientExpressionType {
                    semantic_type: field.semantic_type(),
                    standard_value_type: field.standard_value_type(),
                    result_shape: ClientExpressionResultShape::Value,
                });
                if let SemanticType::Reference { target: next } = field.semantic_type() {
                    owner = next;
                } else if index + 1 != members.len() {
                    diagnostics.push(diagnostic(
                        DiagnosticCode::TypeMismatch,
                        "CLIENT field path continues through a non-reference field",
                        input.logical_path,
                        &member.span,
                    ));
                    return None;
                }
            }
            let Some(expression_type) = expression_type else {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    "CLIENT field path must select a field",
                    input.logical_path,
                    span,
                ));
                return None;
            };
            Some((
                CheckedClientExpression::FieldPath {
                    root: parameter.id,
                    fields,
                    location: location(input.logical_path, span),
                },
                expression_type,
            ))
        }
        ClientExpression::Unary(unary) => {
            let (checked_expression, expression_type) = check_client_expression(
                &unary.expression,
                input,
                targets,
                action_targets,
                resource_targets,
                query_catalogue,
                base,
                server_names,
                standard,
                diagnostics,
                references,
                used_capabilities,
                locals,
            )?;
            let required = match unary.operator {
                orna_syntax::ClientUnaryOperator::Plus
                | orna_syntax::ClientUnaryOperator::Minus => StandardScalar::Integer,
                orna_syntax::ClientUnaryOperator::Not => StandardScalar::Boolean,
            };
            if control_flow_supported_scalar(expression_type) != Some(required) {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    format!(
                        "CLIENT unary {} requires a {} expression",
                        unary.operator.as_str(),
                        match required {
                            StandardScalar::Integer => "INTEGER",
                            StandardScalar::Boolean => "BOOLEAN",
                            _ => "supported scalar",
                        }
                    ),
                    input.logical_path,
                    &unary.span,
                ));
                return None;
            }
            Some((
                CheckedClientExpression::Unary {
                    operator: checked_client_unary_operator(unary.operator),
                    expression: Box::new(checked_expression),
                    location: location(input.logical_path, &unary.span),
                },
                ClientExpressionType {
                    semantic_type: SemanticType::scalar(required),
                    standard_value_type: expression_type.standard_value_type,
                    result_shape: ClientExpressionResultShape::Value,
                },
            ))
        }
        ClientExpression::Binary(binary) => {
            let (left_checked, left_type) = check_client_expression(
                &binary.left,
                input,
                targets,
                action_targets,
                resource_targets,
                query_catalogue,
                base,
                server_names,
                standard,
                diagnostics,
                references,
                used_capabilities,
                locals,
            )?;
            let (right_checked, right_type) = check_client_expression(
                &binary.right,
                input,
                targets,
                action_targets,
                resource_targets,
                query_catalogue,
                base,
                server_names,
                standard,
                diagnostics,
                references,
                used_capabilities,
                locals,
            )?;
            let operator = binary.operator;
            let valid = match operator {
                orna_syntax::ClientBinaryOperator::Add
                | orna_syntax::ClientBinaryOperator::Subtract
                | orna_syntax::ClientBinaryOperator::Multiply
                | orna_syntax::ClientBinaryOperator::Divide
                | orna_syntax::ClientBinaryOperator::Modulo => {
                    control_flow_supported_scalar(left_type) == Some(StandardScalar::Integer)
                        && control_flow_supported_scalar(right_type)
                            == Some(StandardScalar::Integer)
                }
                orna_syntax::ClientBinaryOperator::And | orna_syntax::ClientBinaryOperator::Or => {
                    control_flow_supported_scalar(left_type) == Some(StandardScalar::Boolean)
                        && control_flow_supported_scalar(right_type)
                            == Some(StandardScalar::Boolean)
                }
                orna_syntax::ClientBinaryOperator::Equal
                | orna_syntax::ClientBinaryOperator::NotEqual
                | orna_syntax::ClientBinaryOperator::LessThan
                | orna_syntax::ClientBinaryOperator::GreaterThan
                | orna_syntax::ClientBinaryOperator::LessThanOrEqual
                | orna_syntax::ClientBinaryOperator::GreaterThanOrEqual => {
                    control_flow_types_match(left_type, right_type)
                }
            };
            if !valid {
                let message = match operator {
                    orna_syntax::ClientBinaryOperator::Add
                    | orna_syntax::ClientBinaryOperator::Subtract
                    | orna_syntax::ClientBinaryOperator::Multiply
                    | orna_syntax::ClientBinaryOperator::Divide
                    | orna_syntax::ClientBinaryOperator::Modulo => {
                        format!(
                            "CLIENT arithmetic operator {} requires INTEGER operands",
                            operator.as_str()
                        )
                    }
                    orna_syntax::ClientBinaryOperator::And
                    | orna_syntax::ClientBinaryOperator::Or => {
                        format!(
                            "CLIENT Boolean operator {} requires BOOLEAN operands",
                            operator.as_str()
                        )
                    }
                    _ => format!(
                        "CLIENT comparison {} requires operands of the same INTEGER, BOOLEAN, or TEXT type",
                        operator.as_str()
                    ),
                };
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    message,
                    input.logical_path,
                    &binary.span,
                ));
                return None;
            }
            let comparison = matches!(
                operator,
                orna_syntax::ClientBinaryOperator::Equal
                    | orna_syntax::ClientBinaryOperator::NotEqual
                    | orna_syntax::ClientBinaryOperator::LessThan
                    | orna_syntax::ClientBinaryOperator::GreaterThan
                    | orna_syntax::ClientBinaryOperator::LessThanOrEqual
                    | orna_syntax::ClientBinaryOperator::GreaterThanOrEqual
            );
            let result_scalar = if comparison
                || matches!(
                    operator,
                    orna_syntax::ClientBinaryOperator::And | orna_syntax::ClientBinaryOperator::Or
                ) {
                StandardScalar::Boolean
            } else {
                StandardScalar::Integer
            };
            Some((
                CheckedClientExpression::Binary {
                    operator: checked_client_binary_operator(operator),
                    left: Box::new(left_checked),
                    right: Box::new(right_checked),
                    location: location(input.logical_path, &binary.span),
                },
                ClientExpressionType {
                    semantic_type: SemanticType::scalar(result_scalar),
                    standard_value_type: standard_scalar_type_id(standard, result_scalar),
                    result_shape: ClientExpressionResultShape::Value,
                },
            ))
        }
        ClientExpression::Parenthesized { expression, span } => {
            let (checked_expression, expression_type) = check_client_expression(
                expression,
                input,
                targets,
                action_targets,
                resource_targets,
                query_catalogue,
                base,
                server_names,
                standard,
                diagnostics,
                references,
                used_capabilities,
                locals,
            )?;
            Some((
                CheckedClientExpression::Parenthesized {
                    expression: Box::new(checked_expression),
                    location: location(input.logical_path, span),
                },
                expression_type,
            ))
        }

        ClientExpression::Concat { left, right, span } => {
            let (left_checked, left_type) = check_client_expression(
                left,
                input,
                targets,
                action_targets,
                resource_targets,
                query_catalogue,
                base,
                server_names,
                standard,
                diagnostics,
                references,
                used_capabilities,
                locals,
            )?;
            let (right_checked, right_type) = check_client_expression(
                right,
                input,
                targets,
                action_targets,
                resource_targets,
                query_catalogue,
                base,
                server_names,
                standard,
                diagnostics,
                references,
                used_capabilities,
                locals,
            )?;
            let text = SemanticType::scalar(StandardScalar::CharacterLargeObject);
            if left_type.semantic_type != text
                || right_type.semantic_type != text
                || left_type.result_shape != ClientExpressionResultShape::Value
                || right_type.result_shape != ClientExpressionResultShape::Value
            {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    "CLIENT concatenation requires TEXT expressions",
                    input.logical_path,
                    span,
                ));
                return None;
            }
            Some((
                CheckedClientExpression::Concat {
                    left: Box::new(left_checked),
                    right: Box::new(right_checked),
                    location: location(input.logical_path, span),
                },
                ClientExpressionType {
                    semantic_type: text,
                    result_shape: ClientExpressionResultShape::Value,
                    standard_value_type: left_type.standard_value_type,
                },
            ))
        }
        ClientExpression::Call {
            callee,
            arguments,
            span,
        } => {
            let name = semantic_name(callee);
            if let Some(system_function) = orna_core::system::system_function_by_name(&name)
                && system_function.kind()
                    == orna_core::system::SystemFunctionKind::SourceIntrospection
            {
                if !arguments.is_empty() {
                    diagnostics.push(diagnostic(
                        DiagnosticCode::TypeMismatch,
                        "sys.source.current takes no arguments",
                        input.logical_path,
                        span,
                    ));
                    return None;
                }
                return Some((
                    CheckedClientExpression::SourceIntrospection {
                        location: location(input.logical_path, span),
                    },
                    ClientExpressionType {
                        semantic_type: SemanticType::Named(CheckedTypeId::Existing(
                            SYS_SOURCE_FUNCTION_TYPE_ID,
                        )),
                        standard_value_type: None,
                        result_shape: ClientExpressionResultShape::Value,
                    },
                ));
            }
            if name
                == QualifiedSemanticName::new(["std", "cli", "input"])
                    .expect("std.cli.input is valid")
            {
                if !arguments.is_empty() {
                    diagnostics.push(diagnostic(
                        DiagnosticCode::TypeMismatch,
                        "std.cli.input takes no arguments",
                        input.logical_path,
                        span,
                    ));
                    return None;
                }
                let text = SemanticType::scalar(StandardScalar::CharacterLargeObject);
                return Some((
                    CheckedClientExpression::Input {
                        location: location(input.logical_path, span),
                    },
                    ClientExpressionType {
                        semantic_type: text,
                        standard_value_type: standard_scalar_type_id(
                            standard,
                            StandardScalar::CharacterLargeObject,
                        ),
                        result_shape: ClientExpressionResultShape::Value,
                    },
                ));
            }
            if name
                == QualifiedSemanticName::new(["std", "cli", "evaluate"])
                    .expect("std.cli.evaluate is valid")
            {
                if arguments.len() != 1 {
                    diagnostics.push(diagnostic(
                        DiagnosticCode::TypeMismatch,
                        "std.cli.evaluate requires one command expression",
                        input.logical_path,
                        span,
                    ));
                    return None;
                }
                let (command, command_type) = check_client_expression(
                    &arguments[0].value,
                    input,
                    targets,
                    action_targets,
                    resource_targets,
                    query_catalogue,
                    base,
                    server_names,
                    standard,
                    diagnostics,
                    references,
                    used_capabilities,
                    locals,
                )?;
                let text = SemanticType::scalar(StandardScalar::CharacterLargeObject);
                if command_type.semantic_type != text
                    || command_type.result_shape != ClientExpressionResultShape::Value
                {
                    diagnostics.push(diagnostic(
                        DiagnosticCode::TypeMismatch,
                        "std.cli.evaluate requires a TEXT command expression",
                        input.logical_path,
                        arguments[0].value.span(),
                    ));
                    return None;
                }
                let ui_type =
                    client_expression_type_from_core(ResolvedType::value(STD_UI_TYPE_ID), standard)
                        .expect("std.ui.UI is representable as a CLIENT result");
                return Some((
                    CheckedClientExpression::Evaluate {
                        expression: Box::new(command),
                        location: location(input.logical_path, span),
                    },
                    ui_type,
                ));
            }
            if let Some(inspect) = check_inspect_call(
                expression,
                input,
                targets,
                action_targets,
                resource_targets,
                query_catalogue,
                base,
                server_names,
                standard,
                diagnostics,
                references,
                used_capabilities,
                locals,
            ) {
                return inspect;
            }
            if name
                == QualifiedSemanticName::new(["std", "action", "call"])
                    .expect("std.action.call is valid")
            {
                return check_action_constructor(
                    expression,
                    input,
                    targets,
                    action_targets,
                    resource_targets,
                    query_catalogue,
                    base,
                    server_names,
                    standard,
                    diagnostics,
                    references,
                    used_capabilities,
                    locals,
                );
            }
            if name
                == QualifiedSemanticName::new(["std", "action", "sequence"])
                    .expect("std.action.sequence is valid")
                || name
                    == QualifiedSemanticName::new(["std", "action", "parallel"])
                        .expect("std.action.parallel is valid")
            {
                diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    format!("unknown CLIENT function {name}"),
                    input.logical_path,
                    span,
                ));
                return None;
            }
            if resource_constructor_kind(&name).is_some() {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DomainIncompatible,
                    "resource constructors are only valid as an AWAIT operand",
                    input.logical_path,
                    span,
                ));
                return None;
            }
            let Some(target) = targets.get(&name) else {
                let message = if server_names.contains(&name)
                    || base
                        .function_by_name(&name)
                        .is_some_and(|function| function.domain() == FunctionDomain::Server)
                {
                    format!("CLIENT expression cannot call SERVER function {name}")
                } else {
                    format!("unknown CLIENT function {name}")
                };
                diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    message,
                    input.logical_path,
                    span,
                ));
                return None;
            };
            if target.return_type.result_shape == ClientExpressionResultShape::OptionalList {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    format!(
                        "CLIENT STREAM function {name} cannot be used as an expression operand"
                    ),
                    input.logical_path,
                    span,
                ));
                return None;
            }
            used_capabilities.insert(name.clone());
            let mut bound = vec![false; target.parameters.len()];
            let mut positional = 0usize;
            let mut checked_argument_slots = vec![None; target.parameters.len()];
            for argument in arguments {
                let parameter_index = if let Some(name) = &argument.name {
                    let parameter_name = semantic_part(name);
                    let Some(index) = target
                        .parameters
                        .iter()
                        .position(|parameter| parameter.name == parameter_name)
                    else {
                        diagnostics.push(diagnostic(
                            DiagnosticCode::UnknownQualifiedName,
                            format!("unknown CLIENT argument {parameter_name}"),
                            input.logical_path,
                            &input.declaration_span,
                        ));
                        return None;
                    };
                    index
                } else {
                    while positional < bound.len() && bound[positional] {
                        positional += 1;
                    }
                    if positional >= bound.len() {
                        diagnostics.push(diagnostic(
                            DiagnosticCode::TypeMismatch,
                            format!("too many arguments for CLIENT function {name}"),
                            input.logical_path,
                            &input.declaration_span,
                        ));
                        return None;
                    }
                    let index = positional;
                    positional += 1;
                    index
                };
                if bound[parameter_index] {
                    diagnostics.push(diagnostic(
                        DiagnosticCode::DuplicateDefinition,
                        format!(
                            "duplicate argument for CLIENT parameter {}",
                            target.parameters[parameter_index].name
                        ),
                        input.logical_path,
                        &input.declaration_span,
                    ));
                    return None;
                }
                let (checked, expression_type) = check_client_expression(
                    &argument.value,
                    input,
                    targets,
                    action_targets,
                    resource_targets,
                    query_catalogue,
                    base,
                    server_names,
                    standard,
                    diagnostics,
                    references,
                    used_capabilities,
                    locals,
                )?;
                let parameter = &target.parameters[parameter_index];
                if !client_expression_types_compatible(expression_type, parameter.expression_type) {
                    diagnostics.push(diagnostic(
                        DiagnosticCode::TypeMismatch,
                        format!(
                            "argument does not match CLIENT parameter {}",
                            parameter.name
                        ),
                        input.logical_path,
                        &input.declaration_span,
                    ));
                    return None;
                }
                bound[parameter_index] = true;
                checked_argument_slots[parameter_index] = Some((parameter.id, checked));
            }
            if bound.iter().any(|bound| !bound) {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    format!("missing argument for CLIENT function {name}"),
                    input.logical_path,
                    &input.declaration_span,
                ));
                return None;
            }
            let checked_arguments = checked_argument_slots
                .into_iter()
                .map(|argument| argument.expect("checked CLIENT argument slot is bound"))
                .collect::<Vec<_>>();
            references.push(CheckedDefinitionReference {
                target: CheckedDefinitionReferenceTarget::Function(target.id),
                kind: DefinitionReferenceKind::FunctionCall,
                location: location(input.logical_path, span),
            });
            Some((
                CheckedClientExpression::Call {
                    function: target.id,
                    arguments: checked_arguments,
                    location: location(input.logical_path, span),
                },
                target.return_type,
            ))
        }
    }
}

fn client_local_resource_family(source: &SourceSlice) -> Option<ResourceKind> {
    let mut parser = ClientResourceTypeParser::new(&source.text, source.span.start);
    let outer = parser.parse_qualified_name_parts()?;
    if outer.len() != 3
        || !outer[0].text.eq_ignore_ascii_case("std")
        || !outer[1].text.eq_ignore_ascii_case("data")
    {
        return None;
    }
    match outer[2].text.to_ascii_lowercase().as_str() {
        "resource" => Some(ResourceKind::Scalar),
        "streamresource" => Some(ResourceKind::Stream),
        _ => None,
    }
}

/// Parses a CLIENT resource declaration and returns its family plus inner descriptor.
///
/// The descriptor is resolved later against submitted and standard types; the SERVER
/// target remains authoritative for the resulting expression type.
fn client_local_resource_type(
    source: &SourceSlice,
) -> Option<(ResourceKind, Option<TypeSpecification>)> {
    let mut parser = ClientResourceTypeParser::new(&source.text, source.span.start);
    let outer = parser.parse_qualified_name_parts()?;
    if outer.len() != 3
        || !outer[0].text.eq_ignore_ascii_case("std")
        || !outer[1].text.eq_ignore_ascii_case("data")
    {
        return None;
    }
    let kind = match outer[2].text.to_ascii_lowercase().as_str() {
        "resource" => ResourceKind::Scalar,
        "streamresource" => ResourceKind::Stream,
        _ => return None,
    };
    if !parser.consume(b'<') {
        return None;
    }
    let descriptor = if parser.consume_keyword("TABLE") || parser.consume_keyword("RECORD") {
        parser.parse_inline_record_shape(0)?;
        None
    } else {
        Some(parser.parse_type_specification(0)?)
    };
    if !parser.consume(b'>') || !parser.is_end() {
        return None;
    }
    Some((kind, descriptor))
}

fn reject_deferred_client_resource_descriptor(
    descriptor: Option<&TypeSpecification>,
    local_name: &str,
    input: &ResolvedClientFunctionInput<'_>,
    source: &SourceSlice,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) -> bool {
    // A successful parse with no typed descriptor is the deferred inline row shape.
    if descriptor.is_some() {
        return false;
    }
    diagnostics.push(diagnostic(
        DiagnosticCode::TypeMismatch,
        format!(
            "CLIENT local {local_name} uses an inline TABLE/RECORD resource descriptor; row-resource transport is deferred"
        ),
        input.logical_path,
        &source.span,
    ));
    true
}

struct ClientResourceTypeParser<'a> {
    text: &'a str,
    base: usize,
    offset: usize,
    invalid_trivia: bool,
}

impl<'a> ClientResourceTypeParser<'a> {
    const MAX_TYPE_DEPTH: usize = 32;

    fn new(text: &'a str, base: usize) -> Self {
        Self {
            text,
            base,
            offset: 0,
            invalid_trivia: false,
        }
    }

    fn span(&self, start: usize, end: usize) -> SourceSpan {
        SourceSpan {
            start: self.base + start,
            end: self.base + end,
        }
    }

    fn source_slice(&self, start: usize, end: usize) -> SourceSlice {
        SourceSlice {
            text: self.text[start..end].to_owned(),
            span: self.span(start, end),
        }
    }

    fn is_end(&mut self) -> bool {
        self.skip_trivia();
        !self.invalid_trivia && self.offset == self.text.len()
    }

    fn skip_trivia(&mut self) {
        loop {
            while self
                .text
                .get(self.offset..)
                .and_then(|text| text.chars().next())
                .is_some_and(char::is_whitespace)
            {
                self.offset += self.text[self.offset..]
                    .chars()
                    .next()
                    .expect("character exists")
                    .len_utf8();
            }
            let Some(remaining) = self.text.get(self.offset..) else {
                return;
            };
            if remaining.starts_with("--") {
                self.offset += 2;
                while let Some(character) = self.text[self.offset..].chars().next() {
                    self.offset += character.len_utf8();
                    if character == '\n' {
                        break;
                    }
                }
                continue;
            }
            if let Some(comment) = remaining.strip_prefix("/*") {
                let Some(end) = comment.find("*/") else {
                    self.invalid_trivia = true;
                    self.offset = self.text.len();
                    return;
                };
                self.offset += end + 4;
                continue;
            }
            return;
        }
    }

    fn consume(&mut self, expected: u8) -> bool {
        self.skip_trivia();
        if self.text.as_bytes().get(self.offset) == Some(&expected) {
            self.offset += 1;
            true
        } else {
            false
        }
    }

    fn parse_identifier_part(&mut self) -> Option<NamePart> {
        self.skip_trivia();
        let start = self.offset;
        if self.text.as_bytes().get(self.offset) == Some(&b'"') {
            self.offset += 1;
            while let Some(character) = self.text[self.offset..].chars().next() {
                self.offset += character.len_utf8();
                if character == '"' {
                    if self.text.as_bytes().get(self.offset) == Some(&b'"') {
                        self.offset += 1;
                    } else {
                        return Some(NamePart {
                            text: self.text[start..self.offset].to_owned(),
                            span: self.span(start, self.offset),
                        });
                    }
                }
            }
            return None;
        }
        let first = self.text[self.offset..].chars().next()?;
        if first != '_' && !first.is_alphabetic() {
            return None;
        }
        self.offset += first.len_utf8();
        while let Some(character) = self.text[self.offset..].chars().next() {
            if character != '_' && !character.is_alphabetic() && !character.is_numeric() {
                break;
            }
            self.offset += character.len_utf8();
        }
        Some(NamePart {
            text: self.text[start..self.offset].to_owned(),
            span: self.span(start, self.offset),
        })
    }

    fn parse_identifier(&mut self) -> Option<String> {
        self.parse_identifier_part().map(|part| part.text)
    }

    fn parse_qualified_name_parts(&mut self) -> Option<Vec<NamePart>> {
        let mut parts = vec![self.parse_identifier_part()?];
        while self.consume(b'.') {
            parts.push(self.parse_identifier_part()?);
        }
        Some(parts)
    }

    fn consume_keyword(&mut self, keyword: &str) -> bool {
        self.skip_trivia();
        let saved = self.offset;
        if self.text.as_bytes().get(saved) == Some(&b'"') {
            return false;
        }
        let Some(identifier) = self.parse_identifier() else {
            return false;
        };
        if identifier.eq_ignore_ascii_case(keyword) {
            true
        } else {
            self.offset = saved;
            false
        }
    }

    fn parse_type_specification(&mut self, depth: usize) -> Option<TypeSpecification> {
        if depth > Self::MAX_TYPE_DEPTH {
            return None;
        }
        self.skip_trivia();
        let saved = self.offset;
        if self.consume_keyword("REF") {
            let target = self.parse_type_specification(depth + 1)?;
            let spec = TypeSpecification::Reference {
                span: self.span(saved, target.span().end - self.base),
                target: Box::new(target),
            };
            return self.parse_postfix_options(spec, depth);
        }
        for keyword in ["LIST", "SET", "MAP", "OPTION", "STREAM"] {
            self.offset = saved;
            if !self.consume_keyword(keyword) {
                continue;
            }
            if !self.consume(b'<') {
                return None;
            }
            let first = self.parse_type_specification(depth + 1)?;
            let second = if keyword == "MAP" {
                if !self.consume(b',') {
                    return None;
                }
                Some(self.parse_type_specification(depth + 1)?)
            } else {
                None
            };
            if !self.consume(b'>') {
                return None;
            }
            let spec = match keyword {
                "LIST" => TypeSpecification::List {
                    span: self.span(saved, self.offset),
                    element: Box::new(first),
                },
                "SET" => TypeSpecification::Set {
                    span: self.span(saved, self.offset),
                    element: Box::new(first),
                },
                "MAP" => TypeSpecification::Map {
                    span: self.span(saved, self.offset),
                    key: Box::new(first),
                    value: Box::new(second.expect("MAP value exists")),
                },
                "OPTION" => TypeSpecification::Option {
                    span: self.span(saved, self.offset),
                    value: Box::new(first),
                    spelling: OptionTypeSpelling::Prefix,
                },
                "STREAM" => TypeSpecification::Stream {
                    span: self.span(saved, self.offset),
                    element: Box::new(first),
                },
                _ => unreachable!(),
            };
            return self.parse_postfix_options(spec, depth);
        }
        self.offset = saved;
        if let Some(spec) = self.parse_standard_large_object_specification() {
            return self.parse_postfix_options(spec, depth);
        }
        self.offset = saved;
        let parts = self.parse_qualified_name_parts()?;
        let start = parts.first().expect("nonempty").span.start - self.base;
        let end = parts.last().expect("nonempty").span.end - self.base;
        self.parse_postfix_options(
            TypeSpecification::Named(QualifiedName {
                parts,
                span: self.span(start, end),
            }),
            depth,
        )
    }

    fn parse_inline_record_shape(&mut self, depth: usize) -> Option<()> {
        if depth > Self::MAX_TYPE_DEPTH || !self.consume(b'(') {
            return None;
        }
        if self.consume(b')') {
            return Some(());
        }
        loop {
            self.parse_identifier_part()?;
            self.parse_type_specification(depth + 1)?;
            if self.consume(b')') {
                return Some(());
            }
            if !self.consume(b',') {
                return None;
            }
        }
    }

    fn parse_standard_large_object_specification(&mut self) -> Option<TypeSpecification> {
        self.skip_trivia();
        let start = self.offset;
        let kind = if self.consume_keyword("CHARACTER") {
            StandardLargeObjectKind::Character
        } else {
            self.offset = start;
            if self.consume_keyword("BINARY") {
                StandardLargeObjectKind::Binary
            } else {
                self.offset = start;
                return None;
            }
        };
        if !self.consume_keyword("LARGE") || !self.consume_keyword("OBJECT") {
            self.offset = start;
            return None;
        }
        Some(TypeSpecification::StandardLargeObject {
            kind,
            source: self.source_slice(start, self.offset),
        })
    }

    fn parse_postfix_options(
        &mut self,
        mut spec: TypeSpecification,
        depth: usize,
    ) -> Option<TypeSpecification> {
        let mut option_depth = depth;
        loop {
            self.skip_trivia();
            if self.text.as_bytes().get(self.offset) != Some(&b'?') {
                return Some(spec);
            }
            if option_depth >= Self::MAX_TYPE_DEPTH {
                return None;
            }
            self.offset += 1;
            option_depth += 1;
            let start = spec.span().start - self.base;
            spec = TypeSpecification::Option {
                value: Box::new(spec),
                spelling: OptionTypeSpelling::Postfix,
                span: self.span(start, self.offset),
            };
        }
    }
}

fn client_type_specification_from_source(source: &SourceSlice) -> Option<TypeSpecification> {
    let text = source.text.trim();
    let normalized: String = text
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect();
    let large_object = match normalized.to_ascii_uppercase().as_str() {
        "CHARACTERLARGEOBJECT" => Some(StandardLargeObjectKind::Character),
        "BINARYLARGEOBJECT" => Some(StandardLargeObjectKind::Binary),
        _ => None,
    };
    if let Some(kind) = large_object {
        return Some(TypeSpecification::StandardLargeObject {
            kind,
            source: source.clone(),
        });
    }
    if text.is_empty()
        || text.split('.').any(|part| {
            part.is_empty()
                || part.chars().any(|character| {
                    !(character.is_ascii_alphanumeric() || character == '_' || character == '"')
                })
        })
    {
        return None;
    }
    let parts = text
        .split('.')
        .map(|part| orna_syntax::NamePart {
            text: part.to_owned(),
            span: source.span.clone(),
        })
        .collect::<Vec<_>>();
    Some(TypeSpecification::Named(QualifiedName {
        parts,
        span: source.span.clone(),
    }))
}
fn client_contract_identity(source: &SourceSlice) -> Option<String> {
    let identity = decode_string_literal(source)?;
    let (name, version) = identity.rsplit_once('@')?;
    if version.is_empty()
        || version
            .parse::<u64>()
            .ok()
            .is_none_or(|version| version == 0)
        || name.contains('@')
    {
        return None;
    }
    let parts = name.split('.').collect::<Vec<_>>();
    if parts
        .iter()
        .any(|part| normalise_client_parameter_name(part).is_none())
        || QualifiedSemanticName::new(parts).is_err()
    {
        return None;
    }
    Some(identity)
}
fn is_inspect_render_identity(identity: &str) -> bool {
    identity == "devtools.inspector_shell@1"
        || identity == INSPECT_RENDER_CONTRACT
        || identity.starts_with("std.inspect.render@")
}

#[allow(clippy::too_many_arguments)]
fn validate_registered_client_external_contract(
    _name: &QualifiedSemanticName,
    identity: &str,
    parameters: &[ResolvedServerFunctionParameter],
    return_type: ResolvedApplicationType,
    result_shape: ClientExpressionResultShape,
    logical_path: &str,
    declaration_span: &SourceSpan,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) -> bool {
    if !is_inspect_render_identity(identity) {
        return true;
    }
    if identity != INSPECT_RENDER_CONTRACT {
        diagnostics.push(diagnostic(
            DiagnosticCode::UnknownQualifiedName,
            format!("unregistered CLIENT external contract {identity}"),
            logical_path,
            declaration_span,
        ));
        return false;
    }

    if parameters.len() != INSPECT_RENDER_CARRIER_SIGNATURE.len() {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            format!("{INSPECT_RENDER_CONTRACT} requires exactly nine ordered carrier parameters"),
            logical_path,
            declaration_span,
        ));
        return false;
    }
    for (parameter, (expected_name, expected_id, _)) in
        parameters.iter().zip(INSPECT_RENDER_CARRIER_SIGNATURE)
    {
        if parameter.name != expected_name
            || parameter.semantic_type != SemanticType::Named(CheckedTypeId::Existing(expected_id))
        {
            diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                format!(
                    "{INSPECT_RENDER_CONTRACT} parameter {expected_name} must be {}",
                    expected_name.trim_start_matches("p_")
                ),
                logical_path,
                &parameter.name_span,
            ));
            return false;
        }
    }
    if result_shape != ClientExpressionResultShape::Value
        || return_type.semantic_type != SemanticType::Named(CheckedTypeId::Existing(STD_UI_TYPE_ID))
    {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            format!("{INSPECT_RENDER_CONTRACT} must return std.ui.UI"),
            logical_path,
            declaration_span,
        ));
        return false;
    }
    true
}

impl CheckedClientStateSlot {
    pub(crate) fn location(&self) -> &SourceLocation {
        &self.location
    }
}

fn checked_state_slot_id(function: CheckedFunctionId, name: &str) -> CheckedStateSlotId {
    let mut payload = function.to_string().into_bytes();
    payload.push(0);
    payload.extend_from_slice(&(name.len() as u32).to_be_bytes());
    payload.extend_from_slice(name.as_bytes());
    let digest = artifact_payload_digest(&payload).expect("state-slot identity payload is bounded");
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest.to_bytes()[..16]);
    let id = StateSlotId::from_bytes(bytes);
    if function.existing().is_some() {
        CheckedStateSlotId::Existing(id)
    } else {
        CheckedStateSlotId::Provisional(id)
    }
}
pub(crate) fn durable_state_slot_id(function: FunctionId, name: &str) -> StateSlotId {
    let mut payload = function.to_string().into_bytes();
    payload.push(0);
    payload.extend_from_slice(&(name.len() as u32).to_be_bytes());

    payload.extend_from_slice(name.as_bytes());
    let digest = artifact_payload_digest(&payload).expect("state-slot identity payload is bounded");
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest.to_bytes()[..16]);
    StateSlotId::from_bytes(bytes)
}
fn client_body_requires_control_flow(body: &orna_syntax::ClientFunctionBody) -> bool {
    match body {
        orna_syntax::ClientFunctionBody::BooleanLiteral { .. }
        | orna_syntax::ClientFunctionBody::ExternalContract { .. } => false,
        orna_syntax::ClientFunctionBody::Expression { expression }
        | orna_syntax::ClientFunctionBody::ReturnExpression { expression } => {
            client_expression_requires_control_flow(expression)
        }
        orna_syntax::ClientFunctionBody::StateBlock(block) => {
            block
                .locals
                .iter()
                .any(|local| client_expression_requires_control_flow(&local.expression))
                || block
                    .return_expression
                    .as_ref()
                    .is_some_and(client_expression_requires_control_flow)
                || block
                    .statements
                    .iter()
                    .any(client_statement_requires_control_flow)
        }
        _ => false,
    }
}

fn client_statement_requires_control_flow(
    statement: &orna_syntax::ClientProceduralStatement,
) -> bool {
    match statement {
        orna_syntax::ClientProceduralStatement::Let(statement) => {
            client_expression_requires_control_flow(&statement.expression)
        }
        orna_syntax::ClientProceduralStatement::Assignment(statement) => {
            client_expression_requires_control_flow(&statement.expression)
        }
        orna_syntax::ClientProceduralStatement::Return(_) => true,
        orna_syntax::ClientProceduralStatement::If(statement) => {
            client_expression_requires_control_flow(&statement.condition)
                || statement
                    .then_statements
                    .iter()
                    .any(client_statement_requires_control_flow)
                || statement.elsif_branches.iter().any(|branch| {
                    client_expression_requires_control_flow(&branch.condition)
                        || branch
                            .statements
                            .iter()
                            .any(client_statement_requires_control_flow)
                })
                || statement
                    .else_statements
                    .as_ref()
                    .is_some_and(|statements| {
                        statements
                            .iter()
                            .any(client_statement_requires_control_flow)
                    })
        }
        orna_syntax::ClientProceduralStatement::While(statement) => {
            client_expression_requires_control_flow(&statement.condition)
                || statement
                    .body
                    .iter()
                    .any(client_statement_requires_control_flow)
        }
    }
}

fn client_expression_requires_control_flow(expression: &ClientExpression) -> bool {
    match expression {
        ClientExpression::Unary(_) | ClientExpression::Binary(_) => true,
        ClientExpression::Parenthesized { expression, .. }
        | ClientExpression::Await { expression, .. } => {
            client_expression_requires_control_flow(expression)
        }
        ClientExpression::Call { arguments, .. } => arguments
            .iter()
            .any(|argument| client_expression_requires_control_flow(&argument.value)),
        ClientExpression::Concat { left, right, .. } => {
            client_expression_requires_control_flow(left)
                || client_expression_requires_control_flow(right)
        }
        ClientExpression::StringLiteral { .. }
        | ClientExpression::IntegerLiteral { .. }
        | ClientExpression::BooleanLiteral { .. }
        | ClientExpression::ParameterRead { .. }
        | ClientExpression::LocalRead { .. }
        | ClientExpression::FieldPath { .. } => false,
    }
}

fn validate_client_await_positions(
    expression: &ClientExpression,
    allow_await: bool,
    input: &ResolvedClientFunctionInput<'_>,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) {
    match expression {
        ClientExpression::Await { expression, span } => {
            if !allow_await {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DomainIncompatible,
                    "AWAIT is only valid as the CLIENT body return expression",
                    input.logical_path,
                    span,
                ));
            }
            // The resource constructor is a non-blocking value operation; an
            // AWAIT operand cannot itself contain another suspension.
            validate_client_await_positions(expression, false, input, diagnostics);
        }
        ClientExpression::Call { arguments, .. } => {
            for argument in arguments {
                validate_client_await_positions(&argument.value, false, input, diagnostics);
            }
        }
        ClientExpression::Concat { left, right, .. } => {
            validate_client_await_positions(left, false, input, diagnostics);
            validate_client_await_positions(right, false, input, diagnostics);
        }
        ClientExpression::Binary(binary) => {
            validate_client_await_positions(&binary.left, false, input, diagnostics);
            validate_client_await_positions(&binary.right, false, input, diagnostics);
        }
        ClientExpression::Unary(unary) => {
            validate_client_await_positions(&unary.expression, false, input, diagnostics);
        }
        ClientExpression::Parenthesized { expression, .. } => {
            validate_client_await_positions(expression, false, input, diagnostics);
        }
        ClientExpression::StringLiteral { .. }
        | ClientExpression::IntegerLiteral { .. }
        | ClientExpression::BooleanLiteral { .. }
        | ClientExpression::ParameterRead { .. }
        | ClientExpression::LocalRead { .. }
        | ClientExpression::FieldPath { .. } => {}
    }
}

fn unsupported_client_state_reference(
    expression: &ClientExpression,
    input: &ResolvedClientFunctionInput<'_>,
    state_names: &HashSet<String>,
) -> Option<SourceSpan> {
    let parameter_name = |name: &orna_syntax::NamePart| semantic_part(name);
    let is_state = |name: &orna_syntax::NamePart| {
        let name = parameter_name(name);
        state_names.contains(&name)
            && !input
                .parameters
                .iter()
                .any(|parameter| parameter.name == name)
    };
    match expression {
        ClientExpression::ParameterRead { parameter } if is_state(parameter) => {
            Some(parameter.span.clone())
        }
        ClientExpression::FieldPath { root, .. } if is_state(root) => Some(root.span.clone()),

        ClientExpression::Await { expression, .. } => {
            unsupported_client_state_reference(expression, input, state_names)
        }
        ClientExpression::Call { arguments, .. } => arguments.iter().find_map(|argument| {
            unsupported_client_state_reference(&argument.value, input, state_names)
        }),

        ClientExpression::Concat { left, right, .. } => {
            unsupported_client_state_reference(left, input, state_names)
                .or_else(|| unsupported_client_state_reference(right, input, state_names))
        }
        ClientExpression::Binary(binary) => {
            unsupported_client_state_reference(&binary.left, input, state_names)
                .or_else(|| unsupported_client_state_reference(&binary.right, input, state_names))
        }
        ClientExpression::Unary(unary) => {
            unsupported_client_state_reference(&unary.expression, input, state_names)
        }
        ClientExpression::Parenthesized { expression, .. } => {
            unsupported_client_state_reference(expression, input, state_names)
        }
        ClientExpression::StringLiteral { .. }
        | ClientExpression::IntegerLiteral { .. }
        | ClientExpression::BooleanLiteral { .. }
        | ClientExpression::ParameterRead { .. }
        | ClientExpression::LocalRead { .. }
        | ClientExpression::FieldPath { .. } => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn check_client_functions(
    inputs: &[ResolvedClientFunctionInput<'_>],
    server_inputs: &[ResolvedServerFunctionInput<'_>],
    submitted_ids: &HashMap<QualifiedSemanticName, SubmittedType>,
    query_catalogue: &ResolutionCatalogue<CheckedTypeId, CheckedFieldId>,
    server_names: &[QualifiedSemanticName],
    resource_targets: &HashMap<QualifiedSemanticName, ClientResourceTarget>,
    base: &CatalogueSnapshot,
    diagnostics: &mut Vec<CompilerDiagnostic>,
    standard: Option<&CheckedStandardLibrary>,
    uses: &mut Vec<CheckedApplicationTypeUse>,
) -> Vec<CheckedClientFunction> {
    let targets = client_expression_targets(inputs, base, standard);
    let action_targets = client_action_targets(inputs, server_inputs, base, standard);
    inputs
        .iter()
        .filter_map(|input| {
            for capability in input.capabilities {
                validate_client_capability(
                    capability,
                    input
                        .parameters
                        .iter()
                        .map(|parameter| parameter.name.as_str()),
                    input.logical_path,
                    &input.declaration_span,
                    diagnostics,
                );
            }
            let (body, body_type, body_location, mut references) =
                if input.control_flow_required {
                    check_client_control_flow_body(
                        input,
                        submitted_ids,
                        &targets,
                        &action_targets,
                        resource_targets,
                        query_catalogue,
                        base,
                        server_names,
                        standard,
                        diagnostics,
                    )?
                } else if let Some((value, body_source)) = input.body.as_boolean_literal() {
                    if !input.capabilities.is_empty() {
                        diagnostics.push(diagnostic(
                            DiagnosticCode::CapabilityRequirement,
                            "accepted CLIENT function bodies must not declare capabilities",
                            input.logical_path,
                            &input.declaration_span,
                        ));
                        return None;
                    }
                    (
                        CheckedClientFunctionBody::BooleanLiteral {
                            value,
                            location: location(input.logical_path, &body_source.span),
                        },
                        ClientExpressionType {
                            semantic_type: input.return_type,
                            standard_value_type: input.standard_value_type,
                            result_shape: input.result_shape,
                        },
                        location(input.logical_path, &body_source.span),
                        Vec::new(),
                    )
                } else if let Some(expression) = input.body.as_expression().or_else(|| {
                    input
                        .body
                        .as_state_block()
                        .filter(|block| {
                            block.states.is_empty()
                                && block.locals.is_empty()
                                && block.statements.is_empty()
                        })
                        .and_then(|block| block.return_expression.as_ref())
                }) {
                    if matches!(
                        input.body,
                        orna_syntax::ClientFunctionBody::Expression { .. }
                    ) && input.return_type
                        == SemanticType::Named(CheckedTypeId::Existing(STD_UI_TYPE_ID))
                    {
                        diagnostics.push(diagnostic(
                            DiagnosticCode::DomainIncompatible,
                            "CLIENT UI functions must use explicit RETURN instead of AS expression",
                            input.logical_path,
                            expression.span(),
                        ));
                        return None;
                    }
                    let diagnostics_before = diagnostics.len();
                    validate_client_await_positions(
                        expression,
                        !matches!(input.body, orna_syntax::ClientFunctionBody::Expression { .. }),
                        input,
                        diagnostics,
                    );
                    if diagnostics.len() != diagnostics_before {
                        return None;
                    }
                    let mut references = Vec::new();
                    let mut used_capabilities = HashSet::new();
                    let locals = ClientLocalEnvironment::new();
                    let (checked, expression_type) = check_client_expression(
                        expression,
                        input,
                        &targets,
                        &action_targets,
                        resource_targets,
                        query_catalogue,
                        base,
                        server_names,
                        standard,
                        diagnostics,
                        &mut references,
                        &mut used_capabilities,
                        &locals,
                    )?;
                    if !client_expression_types_compatible(
                        expression_type,
                        ClientExpressionType {
                            semantic_type: input.return_type,
                            standard_value_type: input.standard_value_type,
                            result_shape: input.result_shape,
                        },
                    ) {
                        diagnostics.push(diagnostic(
                            DiagnosticCode::TypeMismatch,
                            "this CLIENT function must return the declared value type",
                            input.logical_path,
                            expression.span(),
                        ));
                        return None;
                    }
                    for capability in input.capabilities {
                        let capability_name = semantic_name(&capability.name);
                        if !used_capabilities.contains(&capability_name) {
                            diagnostics.push(diagnostic(
                                DiagnosticCode::CapabilityRequirement,
                                format!(
                                    "declared CLIENT capability {capability_name} is not exercised"
                                ),
                                input.logical_path,
                                &input.declaration_span,
                            ));
                            return None;
                        }
                    }
                    (
                        CheckedClientFunctionBody::Expression {
                            expression: checked,
                        },
                        expression_type,
                        location(input.logical_path, expression.span()),
                        references,
                    )
                } else if let Some(block) = input.body.as_state_block().filter(|block| block.states.is_empty()) {
    let mut references = Vec::new();
    let mut used_capabilities = HashSet::new();
    let mut locals = ClientLocalEnvironment::new();
    let mut checked_locals = Vec::new();
    let mut statements = Vec::new();
    let mut next_ordinal = 0_u32;

    for local in &block.locals {
        let local_name = semantic_part(&local.name);
        if locals.contains_key(&local_name) {
            diagnostics.push(diagnostic(DiagnosticCode::DuplicateDefinition, format!("duplicate CLIENT local definition {local_name} in {}", input.name), input.logical_path, &local.name.span));
            return None;
        }
        let diagnostics_before = diagnostics.len();
        validate_client_await_positions(&local.expression, true, input, diagnostics);
        if diagnostics.len() != diagnostics_before { return None; }
        let direct_resource = matches!(
            &local.expression,
            ClientExpression::Call { callee, .. }
                if resource_constructor_kind(&semantic_name(callee)).is_some()
        );
        let (checked, expression_type, kind) =
            if client_local_resource_family(&local.type_source).is_some() || direct_resource {
                let Some((kind, descriptor)) = client_local_resource_type(&local.type_source) else {
                    diagnostics.push(diagnostic(
                        DiagnosticCode::TypeMismatch,
                        format!(
                            "CLIENT local {local_name} must declare std.data.Resource<T> or std.data.StreamResource<T>"
                        ),
                        input.logical_path,
                        &local.type_source.span,
                    ));
                    return None;
                };
                if reject_deferred_client_resource_descriptor(
                    descriptor.as_ref(),
                    &local_name,
                    input,
                    &local.type_source,
                    diagnostics,
                ) {
                    return None;
                }
                let expected_type = match descriptor.as_ref() {
                    Some(descriptor) => {
                        let resolved = resolve_application_type_with_named_standard(
                            descriptor,
                            submitted_ids,
                            input.logical_path,
                            diagnostics,
                            standard,
                            true,
                        )?;
                        Some(ClientExpressionType {
                            semantic_type: resolved.semantic_type,
                            standard_value_type: resolved.standard_value_type,
                            result_shape: ClientExpressionResultShape::Value,
                        })
                    }
                    None => None,
                };
                let (checked, expression_type) = check_resource_constructor(
                    &local.expression,
                    input,
                    &targets,
                    &action_targets,
                    resource_targets,
                    query_catalogue,
                    base,
                    server_names,
                    standard,
                    diagnostics,
                    &mut references,
                    &mut used_capabilities,
                    &locals,
                )?;
                let actual_kind = match &checked {
                    CheckedClientExpression::Resource { operation } => operation.kind,
                    _ => unreachable!("resource constructor checker returns a resource"),
                };
                if actual_kind != kind {
                    diagnostics.push(diagnostic(
                        DiagnosticCode::TypeMismatch,
                        format!("CLIENT local {local_name} type does not match its resource constructor"),
                        input.logical_path,
                        &local.type_source.span,
                    ));
                    return None;
                }
                if let Some(expected_type) = expected_type
                    && !client_expression_types_compatible(expression_type, expected_type)
                {
                    diagnostics.push(diagnostic(
                        DiagnosticCode::TypeMismatch,
                        format!(
                            "CLIENT local {local_name} descriptor does not match its SERVER resource result"
                        ),
                        input.logical_path,
                        &local.type_source.span,
                    ));
                    return None;
                }
                (checked, expression_type, CheckedClientLocalKind::Resource(kind))
            } else {
            let (checked, expression_type) = check_client_expression(&local.expression, input, &targets, &action_targets, resource_targets, query_catalogue, base, server_names, standard, diagnostics, &mut references, &mut used_capabilities, &locals)?;
            if !client_expression_type_is_evaluable(expression_type, base, standard) {
                diagnostics.push(diagnostic(DiagnosticCode::TypeMismatch, "this CLIENT local type is not supported by the local evaluator", input.logical_path, &local.span));
                return None;
            }
            let Some(specification) = client_type_specification_from_source(&local.type_source) else {
                diagnostics.push(diagnostic(DiagnosticCode::TypeMismatch, format!("unsupported CLIENT local type for {local_name}"), input.logical_path, &local.type_source.span));
                return None;
            };
            let resolved = resolve_application_type_with_named_standard(&specification, submitted_ids, input.logical_path, diagnostics, standard, true)?;
            let expected = ClientExpressionType { semantic_type: resolved.semantic_type, standard_value_type: resolved.standard_value_type, result_shape: ClientExpressionResultShape::Value };
            if !client_expression_types_compatible(expression_type, expected) {
                diagnostics.push(diagnostic(DiagnosticCode::TypeMismatch, format!("CLIENT local {local_name} initializer does not match its declared type"), input.logical_path, &local.span));
                return None;
            }
            (checked, expression_type, CheckedClientLocalKind::Value)
        };
        let ordinal = next_ordinal; next_ordinal += 1;
        checked_locals.push(CheckedClientLocal { ordinal, name: local_name.clone(), semantic_type: expression_type.semantic_type, standard_value_type: expression_type.standard_value_type, kind, location: location(input.logical_path, &local.span) });
        locals.insert(local_name, ClientLocalBinding { checked: checked.clone(), expression_type, ordinal: Some(ordinal), kind });
        statements.push(CheckedClientStatement::Let { local: ordinal, expression: checked });
    }

    for statement in &block.statements {
        match statement {
            orna_syntax::ClientProceduralStatement::Let(statement) => {
                let local_name = semantic_part(&statement.name);
                if locals.contains_key(&local_name) {
                    diagnostics.push(diagnostic(DiagnosticCode::DuplicateDefinition, format!("duplicate CLIENT local definition {local_name} in {}", input.name), input.logical_path, &statement.name.span));
                    return None;
                }
                let diagnostics_before = diagnostics.len();
                validate_client_await_positions(&statement.expression, true, input, diagnostics);
                if diagnostics.len() != diagnostics_before { return None; }
                let declared_resource_family = statement
                    .type_source
                    .as_ref()
                    .and_then(client_local_resource_family);
                let direct_resource = matches!(&statement.expression, ClientExpression::Call { callee, .. } if resource_constructor_kind(&semantic_name(callee)).is_some());
                let (checked, expression_type, kind) = if declared_resource_family.is_some() || direct_resource {
                    if !direct_resource {
                        diagnostics.push(diagnostic(DiagnosticCode::TypeMismatch, format!("CLIENT local {local_name} resource type requires a resource constructor"), input.logical_path, &statement.span));
                        return None;
                    }
                    let (checked, expression_type) = check_resource_constructor(&statement.expression, input, &targets, &action_targets, resource_targets, query_catalogue, base, server_names, standard, diagnostics, &mut references, &mut used_capabilities, &locals)?;
                    let actual_kind = match &checked {
                        CheckedClientExpression::Resource { operation } => operation.kind,
                        _ => unreachable!("resource constructor checker returns a resource"),
                    };
                    if let Some(source) = &statement.type_source {
                        let Some((expected_kind, descriptor)) = client_local_resource_type(source) else {
                            diagnostics.push(diagnostic(DiagnosticCode::TypeMismatch, format!("CLIENT local {local_name} must declare std.data.Resource<T> or std.data.StreamResource<T>"), input.logical_path, &source.span));
                            return None;
                        };
                        if reject_deferred_client_resource_descriptor(
                            descriptor.as_ref(),
                            &local_name,
                            input,
                            source,
                            diagnostics,
                        ) {
                            return None;
                        }
                        let expected_type = match descriptor.as_ref() {
                            Some(descriptor) => {
                                let resolved = resolve_application_type_with_named_standard(
                                    descriptor,
                                    submitted_ids,
                                    input.logical_path,
                                    diagnostics,
                                    standard,
                                    true,
                                )?;
                                Some(ClientExpressionType {
                                    semantic_type: resolved.semantic_type,
                                    standard_value_type: resolved.standard_value_type,
                                    result_shape: ClientExpressionResultShape::Value,
                                })
                            }
                            None => None,
                        };
                        if actual_kind != expected_kind {
                            diagnostics.push(diagnostic(DiagnosticCode::TypeMismatch, format!("CLIENT local {local_name} type does not match its resource constructor"), input.logical_path, &source.span));
                            return None;
                        }
                        if let Some(expected_type) = expected_type
                            && !client_expression_types_compatible(expression_type, expected_type)
                        {
                            diagnostics.push(diagnostic(DiagnosticCode::TypeMismatch, format!("CLIENT local {local_name} descriptor does not match its SERVER resource result"), input.logical_path, &source.span));
                            return None;
                        }
                    }
                    (checked, expression_type, CheckedClientLocalKind::Resource(actual_kind))
                } else {
                    let (checked, expression_type) = check_client_expression(&statement.expression, input, &targets, &action_targets, resource_targets, query_catalogue, base, server_names, standard, diagnostics, &mut references, &mut used_capabilities, &locals)?;
                    if !client_expression_type_is_evaluable(expression_type, base, standard) {
                        diagnostics.push(diagnostic(DiagnosticCode::TypeMismatch, "this CLIENT local type is not supported by the local evaluator", input.logical_path, &statement.span));
                        return None;
                    }
                    if let Some(source) = &statement.type_source {
                        let Some(specification) = client_type_specification_from_source(source) else {
                            diagnostics.push(diagnostic(DiagnosticCode::TypeMismatch, format!("unsupported CLIENT local type for {local_name}"), input.logical_path, &source.span));
                            return None;
                        };
                        let resolved = resolve_application_type_with_named_standard(&specification, submitted_ids, input.logical_path, diagnostics, standard, true)?;
                        let expected = ClientExpressionType { semantic_type: resolved.semantic_type, standard_value_type: resolved.standard_value_type, result_shape: ClientExpressionResultShape::Value };
                        if !client_expression_types_compatible(expression_type, expected) {
                            diagnostics.push(diagnostic(DiagnosticCode::TypeMismatch, format!("CLIENT local {local_name} initializer does not match its declared type"), input.logical_path, &statement.span));
                            return None;
                        }
                    }
                    (checked, expression_type, CheckedClientLocalKind::Value)
                };
                let ordinal = next_ordinal; next_ordinal += 1;
                checked_locals.push(CheckedClientLocal { ordinal, name: local_name.clone(), semantic_type: expression_type.semantic_type, standard_value_type: expression_type.standard_value_type, kind, location: location(input.logical_path, &statement.span) });
                locals.insert(local_name, ClientLocalBinding { checked: checked.clone(), expression_type, ordinal: Some(ordinal), kind });
                statements.push(CheckedClientStatement::Let { local: ordinal, expression: checked });
            }
            orna_syntax::ClientProceduralStatement::Assignment(statement) => {
                let local_name = semantic_part(&statement.target);
                let Some(binding) = locals.get(&local_name).cloned() else {
                    diagnostics.push(diagnostic(DiagnosticCode::UnknownQualifiedName, format!("unknown CLIENT local {local_name}"), input.logical_path, &statement.target.span));
                    return None;
                };
                let diagnostics_before = diagnostics.len();
                validate_client_await_positions(&statement.expression, true, input, diagnostics);
                if diagnostics.len() != diagnostics_before { return None; }
                let direct_resource = matches!(&statement.expression, ClientExpression::Call { callee, .. } if resource_constructor_kind(&semantic_name(callee)).is_some());
                let (checked, expression_type) = if matches!(binding.kind, CheckedClientLocalKind::Resource(_)) && direct_resource {
                    check_resource_constructor(&statement.expression, input, &targets, &action_targets, resource_targets, query_catalogue, base, server_names, standard, diagnostics, &mut references, &mut used_capabilities, &locals)?
                } else {
                    check_client_expression(&statement.expression, input, &targets, &action_targets, resource_targets, query_catalogue, base, server_names, standard, diagnostics, &mut references, &mut used_capabilities, &locals)?
                };
                if !client_expression_types_compatible(expression_type, binding.expression_type) || (matches!(binding.kind, CheckedClientLocalKind::Resource(_)) != matches!(checked, CheckedClientExpression::Resource { .. })) {
                    diagnostics.push(diagnostic(DiagnosticCode::TypeMismatch, format!("CLIENT assignment to local {local_name} does not match its declared type"), input.logical_path, &statement.span));
                    return None;
                }
                statements.push(CheckedClientStatement::Assignment { local: binding.ordinal.expect("procedural local has ordinal"), expression: checked.clone() });
                if let Some(binding) = locals.get_mut(&local_name) { binding.checked = checked; }
            }
            orna_syntax::ClientProceduralStatement::Return(_)
            | orna_syntax::ClientProceduralStatement::If(_)
            | orna_syntax::ClientProceduralStatement::While(_) => {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DomainIncompatible,
                    "CLIENT procedural statements require the control-flow plan",
                    input.logical_path,
                    &input.declaration_span,
                ));
                return None;
            }

        }
    }
    let Some(expression) = block.return_expression.as_ref() else {
        diagnostics.push(diagnostic(DiagnosticCode::DomainIncompatible, "CLIENT procedural bodies must return an expression", input.logical_path, &block.span));
        return None;
    };
    let diagnostics_before = diagnostics.len();
    validate_client_await_positions(expression, true, input, diagnostics);
    if diagnostics.len() != diagnostics_before { return None; }
    let (checked_return, return_type) = check_client_expression(expression, input, &targets, &action_targets, resource_targets, query_catalogue, base, server_names, standard, diagnostics, &mut references, &mut used_capabilities, &locals)?;
    if !client_expression_types_compatible(return_type, ClientExpressionType { semantic_type: input.return_type, standard_value_type: input.standard_value_type, result_shape: input.result_shape }) {
        diagnostics.push(diagnostic(DiagnosticCode::TypeMismatch, "this CLIENT function must return the declared value type", input.logical_path, expression.span()));
        return None;
    }
    for capability in input.capabilities {
        let capability_name = semantic_name(&capability.name);
        if !used_capabilities.contains(&capability_name) {
            diagnostics.push(diagnostic(DiagnosticCode::CapabilityRequirement, format!("declared CLIENT capability {capability_name} is not exercised"), input.logical_path, &input.declaration_span));
            return None;
        }
    }
    (CheckedClientFunctionBody::Procedural { locals: checked_locals, statements, return_expression: checked_return }, return_type, location(input.logical_path, expression.span()), references)
} else if let Some(block) = input.body.as_state_block() {
                    if !block.locals.is_empty() || !block.statements.is_empty() {
                        diagnostics.push(diagnostic(
                            DiagnosticCode::DomainIncompatible,
                            "CLIENT state blocks do not support procedural statements",
                            input.logical_path,
                            &block.span,
                        ));
                        return None;
                    }
                    let mut references = Vec::new();
                    let mut used_capabilities = HashSet::new();
                    let mut state_names = HashSet::new();
                    let mut states = Vec::with_capacity(block.states.len());
                    for (ordinal, state) in block.states.iter().enumerate() {
                        let state_name = semantic_part(&state.name);
                        if !state_names.insert(state_name.clone()) {
                            diagnostics.push(diagnostic(
                                DiagnosticCode::DuplicateDefinition,
                                format!("duplicate state definition {state_name} in {}", input.name),
                                input.logical_path,
                                &state.name.span,
                            ));
                            return None;
                        }
                        let resolved = resolve_application_type_with_named_standard(
                            &state.type_specification,
                            submitted_ids,
                            input.logical_path,
                            diagnostics,
                            standard,
                            true,
                        )?;
                        record_standard_type_use(
                            uses,
                            standard,
                            CheckedTypeUseKind::State {
                                owner: input.id,
                                ordinal: ordinal as u32,
                            },
                            resolved,
                            type_use_location(&state.type_specification, input.logical_path),
                        );
                        if let SemanticType::Named(CheckedTypeId::Existing(type_id)) =
                            resolved.semantic_type
                        {
                            if is_sealed_inspect_type_id(type_id) {
                                diagnostics.push(diagnostic(
                                    DiagnosticCode::DomainIncompatible,
                                    "sealed sys.inspect carriers are transient and cannot be stored in CLIENT state",
                                    input.logical_path,
                                    state.type_specification.span(),
                                ));
                                return None;
                            }
                            if standard.is_some_and(|standard| {
                                standard.value_types().iter().any(|value_type| {
                                    value_type.id() == type_id
                                        && value_type.kind() == ValueTypeKind::Opaque
                                })
                            }) {
                                diagnostics.push(diagnostic(
                                    DiagnosticCode::DomainIncompatible,
                                    "opaque CLIENT values are transient and cannot be stored in state",
                                    input.logical_path,
                                    state.type_specification.span(),
                                ));
                                return None;
                            }
                        }
                        let state_type = ClientExpressionType {
                            semantic_type: resolved.semantic_type,
                            standard_value_type: resolved.standard_value_type,
                            result_shape: ClientExpressionResultShape::Value,
                        };
                        if !client_expression_type_is_evaluable(state_type, base, standard) {
                            diagnostics.push(diagnostic(
                                DiagnosticCode::TypeMismatch,
                                "this CLIENT state type is not supported by the local evaluator",
                                input.logical_path,
                                state.type_specification.span(),
                            ));
                            return None;
                        }
                        let default = match &state.default {
                            StateDefault::Unset => CheckedStateDefault::Unset,
                            StateDefault::Null => CheckedStateDefault::Null,
                            StateDefault::Expression(expression) => {
                                let diagnostics_before = diagnostics.len();
                                validate_client_await_positions(expression, false, input, diagnostics);
                                if diagnostics.len() != diagnostics_before {
                                    return None;
                                }
                                if client_expression_contains_action(expression) {
                                    diagnostics.push(diagnostic(
                                        DiagnosticCode::DomainIncompatible,
                                        "CLIENT state defaults do not support action expressions",
                                        input.logical_path,
                                        expression.span(),
                                    ));
                                    return None;
                                }
                                if let Some(span) = unsupported_client_state_reference(
                                    expression,
                                    input,
                                    &state_names,
                                ) {
                                    diagnostics.push(diagnostic(
                                        DiagnosticCode::DomainIncompatible,
                                        "CLIENT state references are not supported in expressions",
                                        input.logical_path,
                                        &span,
                                    ));
                                    return None;
                                }
                                let (checked, expression_type) = check_client_expression(
                                    expression,
                                    input,
                                    &targets,
                                    &action_targets,
                                    resource_targets,
                                    query_catalogue,
                                    base,
                                    server_names,
                                    standard,
                                    diagnostics,
                                    &mut references,
                                    &mut used_capabilities,
                                    &ClientLocalEnvironment::new(),
                                )?;
                                if client_expression_contains_inspect(&checked) {
                                    diagnostics.push(diagnostic(
                                        DiagnosticCode::DomainIncompatible,
                                        "CLIENT state defaults do not support Inspector expressions",
                                        input.logical_path,
                                        expression.span(),
                                    ));
                                    return None;
                                }
                                if !client_expression_types_compatible(expression_type, state_type) {
                                    diagnostics.push(diagnostic(
                                        DiagnosticCode::TypeMismatch,
                                        "this CLIENT state default must have the declared state type",
                                        input.logical_path,
                                        expression.span(),
                                    ));
                                    return None;
                                }
                                CheckedStateDefault::Expression(checked)
                            }
                        };
                        let scope = match state.scope {
                            StateScope::Local => CheckedStateScope::Local,
                            StateScope::Session => CheckedStateScope::Session,
                            StateScope::User => CheckedStateScope::User,
                        };
                        states.push(CheckedClientStateSlot {
                            id: checked_state_slot_id(input.id, &state_name),
                            name: state_name,
                            ordinal: ordinal as u32,
                            semantic_type: resolved.semantic_type,
                            standard_value_type: resolved.standard_value_type,
                            scope,
                            default,
                            location: location(input.logical_path, &state.span),
                        });
                    }
                    let mut locals = ClientLocalEnvironment::new();
                    for local in &block.locals {
                        let local_name = semantic_part(&local.name);
                        if locals.contains_key(&local_name) {
                            diagnostics.push(diagnostic(
                                DiagnosticCode::DuplicateDefinition,
                                format!("duplicate CLIENT local definition {local_name} in {}", input.name),
                                input.logical_path,
                                &local.name.span,
                            ));
                            return None;
                        }
                        let diagnostics_before = diagnostics.len();
                        validate_client_await_positions(&local.expression, false, input, diagnostics);
                        if diagnostics.len() != diagnostics_before {
                            return None;
                        }
                        if let Some(span) = unsupported_client_state_reference(
                            &local.expression,
                            input,
                            &state_names,
                        ) {
                            diagnostics.push(diagnostic(
                                DiagnosticCode::DomainIncompatible,
                                "CLIENT state references are not supported in expressions",
                                input.logical_path,
                                &span,
                            ));
                            return None;
                        }
                        let (checked, expression_type) = check_resource_constructor(
                            &local.expression,
                            input,
                            &targets,
                            &action_targets,
                            resource_targets,
                            query_catalogue,
                            base,
                            server_names,
                            standard,
                            diagnostics,
                            &mut references,
                            &mut used_capabilities,
                            &locals,
                        )?;
                        let Some((expected_kind, descriptor)) = client_local_resource_type(&local.type_source) else {
                            diagnostics.push(diagnostic(
                                DiagnosticCode::TypeMismatch,
                                format!("CLIENT local {local_name} must declare std.data.Resource<T> or std.data.StreamResource<T>"),
                                input.logical_path,
                                &local.type_source.span,
                            ));
                            return None;
                        };
                        if reject_deferred_client_resource_descriptor(
                            descriptor.as_ref(),
                            &local_name,
                            input,
                            &local.type_source,
                            diagnostics,
                        ) {
                            return None;
                        }
                        let expected_type = match descriptor.as_ref() {
                            Some(descriptor) => {
                                let resolved = resolve_application_type_with_named_standard(
                                    descriptor,
                                    submitted_ids,
                                    input.logical_path,
                                    diagnostics,
                                    standard,
                                    true,
                                )?;
                                Some(ClientExpressionType {
                                    semantic_type: resolved.semantic_type,
                                    standard_value_type: resolved.standard_value_type,
                                    result_shape: ClientExpressionResultShape::Value,
                                })
                            }
                            None => None,
                        };
                        let actual_kind = match &checked {
                            CheckedClientExpression::Resource { operation } => operation.kind,
                            _ => unreachable!("resource constructor checker returns a resource"),
                        };
                        if actual_kind != expected_kind {
                            diagnostics.push(diagnostic(
                                DiagnosticCode::TypeMismatch,
                                format!("CLIENT local {local_name} type does not match its resource constructor"),
                                input.logical_path,
                                &local.type_source.span,
                            ));
                            return None;
                        }
                        if let Some(expected_type) = expected_type
                            && !client_expression_types_compatible(expression_type, expected_type)
                        {
                            diagnostics.push(diagnostic(
                                DiagnosticCode::TypeMismatch,
                                format!(
                                    "CLIENT local {local_name} descriptor does not match its SERVER resource result"
                                ),
                                input.logical_path,
                                &local.type_source.span,
                            ));
                            return None;
                        }
                        locals.insert(local_name, ClientLocalBinding { checked, expression_type, ordinal: None, kind: CheckedClientLocalKind::Resource(actual_kind) });
                    }
                    let Some(expression) = block.return_expression.as_ref() else {
                        diagnostics.push(diagnostic(
                            DiagnosticCode::DomainIncompatible,
                            "CLIENT state blocks must return an expression",
                            input.logical_path,
                            &block.span,
                        ));
                        return None;
                    };
                    if let Some(span) =
                        unsupported_client_state_reference(expression, input, &state_names)
                    {
                        diagnostics.push(diagnostic(
                            DiagnosticCode::DomainIncompatible,
                            "CLIENT state references are not supported in expressions",
                            input.logical_path,
                            &span,
                        ));
                        return None;
                    }
                    let diagnostics_before = diagnostics.len();
                    validate_client_await_positions(expression, false, input, diagnostics);
                    if diagnostics.len() != diagnostics_before {
                        return None;
                    }
                    if client_expression_contains_action(expression) {
                        diagnostics.push(diagnostic(
                            DiagnosticCode::DomainIncompatible,
                            "CLIENT state blocks do not support action expressions",
                            input.logical_path,
                            expression.span(),
                        ));
                        return None;
                    }
                    let (checked_return, return_type) = check_client_expression(
                        expression,
                        input,
                        &targets,
                        &action_targets,
                        resource_targets,
                        query_catalogue,
                        base,
                        server_names,
                        standard,
                        diagnostics,
                        &mut references,
                        &mut used_capabilities,
                        &locals,
                    )?;
                    if client_expression_contains_inspect(&checked_return) {
                        diagnostics.push(diagnostic(
                            DiagnosticCode::DomainIncompatible,
                            "CLIENT state blocks do not support Inspector expressions",
                            input.logical_path,
                            expression.span(),
                        ));
                        return None;
                    }
                    if !client_expression_types_compatible(
                        return_type,
                        ClientExpressionType {
                            semantic_type: input.return_type,
                            standard_value_type: input.standard_value_type,
                            result_shape: input.result_shape,
                        },
                    ) {
                        diagnostics.push(diagnostic(
                            DiagnosticCode::TypeMismatch,
                            "this CLIENT function must return the declared value type",
                            input.logical_path,
                            expression.span(),
                        ));
                        return None;
                    }
                    for capability in input.capabilities {
                        let capability_name = semantic_name(&capability.name);
                        if !used_capabilities.contains(&capability_name) {
                            diagnostics.push(diagnostic(
                                DiagnosticCode::CapabilityRequirement,
                                format!(
                                    "declared CLIENT capability {capability_name} is not exercised"
                                ),
                                input.logical_path,
                                &input.declaration_span,
                            ));
                            return None;
                        }
                    }
                    (
                        CheckedClientFunctionBody::StateBlock {
                            states,
                            return_expression: checked_return,
                        },
                        return_type,
                        location(input.logical_path, expression.span()),
                        references,
                    )
                } else if let Some(contract) = input.body.as_external_contract() {
                    let Some(identity) = client_contract_identity(contract) else {
                        diagnostics.push(diagnostic(
                        DiagnosticCode::DomainIncompatible,
                        "RUNTIME CONTRACT identity must be '<qualified-name>@<positive-version>'",
                        input.logical_path,
                        &input.declaration_span,
                    ));
                        return None;
                    };
                    if !validate_registered_client_external_contract(
                        &input.name,
                        &identity,
                        &input.parameters,
                        ResolvedApplicationType {
                            semantic_type: input.return_type,
                            standard_value_type: input.standard_value_type,
                        },
                        input.result_shape,
                        input.logical_path,
                        &input.declaration_span,
                        diagnostics,
                    ) {
                        return None;
                    }
                    (
                        CheckedClientFunctionBody::ExternalContract {
                            identity,
                            location: location(input.logical_path, &contract.span),
                        },
                        ClientExpressionType {
                            semantic_type: input.return_type,
                            standard_value_type: input.standard_value_type,
                            result_shape: input.result_shape,
                        },
                        location(input.logical_path, &contract.span),
                        Vec::new(),
                    )
                } else {
                    diagnostics.push(diagnostic(
                        DiagnosticCode::DomainIncompatible,
                        "CLIENT function body is not supported",
                        input.logical_path,
                        &input.declaration_span,
                    ));
                    return None;
                };
            references.extend(parameter_references(&input.parameters));
            match &body {
                CheckedClientFunctionBody::ExternalContract { .. } => {}
                #[cfg(test)]
                CheckedClientFunctionBody::Unsupported => {}
                _ => {
                    let resolved = ResolvedApplicationType {
                        semantic_type: body_type.semantic_type,
                        standard_value_type: body_type.standard_value_type,
                    };
                    let mut recorder = StandardTypeUseRecorder::new(
                        uses,
                        standard,
                        input.id,
                        input.logical_path,
                    );
                    recorder.record_client_body(resolved, body_location);
                }
            }
            Some(CheckedClientFunction {
                id: input.id,
                name: input.name.clone(),
                domain: FunctionDomain::Client,
                parameters: input
                    .parameters
                    .iter()
                    .map(|parameter| CheckedServerFunctionParameter {
                        id: parameter.id,
                        name: parameter.name.clone(),
                        ordinal: parameter.ordinal,
                        semantic_type: parameter.semantic_type,
                        location: parameter.location.clone(),
                    })
                    .collect(),
                return_type: input.return_type,
                return_shape: input.return_shape,
                security: CatalogueFunctionSecurity::Invoker,
                transaction: None,
                volatility: CatalogueFunctionVolatility::Immutable,
                location: input.location.clone(),
                body,
                references,
                capabilities: input
                    .capabilities
                    .iter()
                    .filter_map(checked_client_capability)
                    .collect(),
            })
        })
        .collect()
}

struct ClientControlFlowChecker<'a, 'b> {
    input: &'a ResolvedClientFunctionInput<'b>,
    submitted_ids: &'a HashMap<QualifiedSemanticName, SubmittedType>,
    targets: &'a HashMap<QualifiedSemanticName, ClientExpressionTarget>,
    action_targets: &'a HashMap<QualifiedSemanticName, ClientActionTarget>,
    resource_targets: &'a HashMap<QualifiedSemanticName, ClientResourceTarget>,
    query_catalogue: &'a ResolutionCatalogue<CheckedTypeId, CheckedFieldId>,
    base: &'a CatalogueSnapshot,
    server_names: &'a [QualifiedSemanticName],
    standard: Option<&'a CheckedStandardLibrary>,
    diagnostics: &'a mut Vec<CompilerDiagnostic>,
    locals: ClientLocalEnvironment,
    checked_locals: Vec<CheckedClientLocal>,
    references: Vec<CheckedDefinitionReference>,
    used_capabilities: HashSet<QualifiedSemanticName>,
    next_ordinal: u32,
    _source_lifetime: std::marker::PhantomData<&'b ()>,
}

impl<'a, 'b> ClientControlFlowChecker<'a, 'b> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        input: &'a ResolvedClientFunctionInput<'b>,
        submitted_ids: &'a HashMap<QualifiedSemanticName, SubmittedType>,
        targets: &'a HashMap<QualifiedSemanticName, ClientExpressionTarget>,
        action_targets: &'a HashMap<QualifiedSemanticName, ClientActionTarget>,
        resource_targets: &'a HashMap<QualifiedSemanticName, ClientResourceTarget>,
        query_catalogue: &'a ResolutionCatalogue<CheckedTypeId, CheckedFieldId>,
        base: &'a CatalogueSnapshot,
        server_names: &'a [QualifiedSemanticName],
        standard: Option<&'a CheckedStandardLibrary>,
        diagnostics: &'a mut Vec<CompilerDiagnostic>,
    ) -> Self {
        Self {
            input,
            submitted_ids,
            targets,
            action_targets,
            resource_targets,
            query_catalogue,
            base,
            server_names,
            standard,
            diagnostics,
            locals: ClientLocalEnvironment::new(),
            checked_locals: Vec::new(),
            references: Vec::new(),
            used_capabilities: HashSet::new(),
            next_ordinal: 0,
            _source_lifetime: std::marker::PhantomData,
        }
    }

    fn expression(
        &mut self,
        expression: &ClientExpression,
    ) -> Option<(CheckedClientExpression, ClientExpressionType)> {
        check_client_expression(
            expression,
            self.input,
            self.targets,
            self.action_targets,
            self.resource_targets,
            self.query_catalogue,
            self.base,
            self.server_names,
            self.standard,
            self.diagnostics,
            &mut self.references,
            &mut self.used_capabilities,
            &self.locals,
        )
    }

    fn declare_local(
        &mut self,
        name: &NamePart,
        type_source: Option<&SourceSlice>,
        expression: &ClientExpression,
        span: &SourceSpan,
        pre_begin_resource: bool,
    ) -> Option<(u32, CheckedClientExpression)> {
        let local_name = semantic_part(name);
        if self.locals.contains_key(&local_name) {
            self.diagnostics.push(diagnostic(
                DiagnosticCode::DuplicateDefinition,
                format!(
                    "duplicate CLIENT local definition {local_name} in {}",
                    self.input.name
                ),
                self.input.logical_path,
                &name.span,
            ));
            return None;
        }
        let diagnostics_before = self.diagnostics.len();
        validate_client_await_positions(expression, true, self.input, self.diagnostics);
        if self.diagnostics.len() != diagnostics_before {
            return None;
        }

        let declared_resource_family = type_source.and_then(client_local_resource_family);
        let direct_resource = matches!(
            expression,
            ClientExpression::Call { callee, .. }
                if resource_constructor_kind(&semantic_name(callee)).is_some()
        );
        if pre_begin_resource && !direct_resource {
            self.diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                format!("CLIENT local {local_name} requires a resource constructor initializer"),
                self.input.logical_path,
                span,
            ));
            return None;
        }

        let (checked, expression_type, kind) = if declared_resource_family.is_some()
            || direct_resource
        {
            if !direct_resource {
                self.diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    format!(
                        "CLIENT local {local_name} resource type requires a resource constructor"
                    ),
                    self.input.logical_path,
                    span,
                ));
                return None;
            }
            let (checked, expression_type) = check_resource_constructor(
                expression,
                self.input,
                self.targets,
                self.action_targets,
                self.resource_targets,
                self.query_catalogue,
                self.base,
                self.server_names,
                self.standard,
                self.diagnostics,
                &mut self.references,
                &mut self.used_capabilities,
                &self.locals,
            )?;
            let actual_kind = match &checked {
                CheckedClientExpression::Resource { operation } => operation.kind,
                _ => unreachable!("resource constructor checker returns a resource"),
            };
            if let Some(source) = type_source {
                let Some((expected_kind, descriptor)) = client_local_resource_type(source) else {
                    self.diagnostics.push(diagnostic(
                            DiagnosticCode::TypeMismatch,
                            format!(
                                "CLIENT local {local_name} must declare std.data.Resource<T> or std.data.StreamResource<T>"
                            ),
                            self.input.logical_path,
                            &source.span,
                        ));
                    return None;
                };
                if reject_deferred_client_resource_descriptor(
                    descriptor.as_ref(),
                    &local_name,
                    self.input,
                    source,
                    self.diagnostics,
                ) {
                    return None;
                }
                if actual_kind != expected_kind {
                    self.diagnostics.push(diagnostic(
                        DiagnosticCode::TypeMismatch,
                        format!(
                            "CLIENT local {local_name} type does not match its resource constructor"
                        ),
                        self.input.logical_path,
                        &source.span,
                    ));
                    return None;
                }
                if let Some(descriptor) = descriptor {
                    let resolved = resolve_application_type_with_named_standard(
                        &descriptor,
                        self.submitted_ids,
                        self.input.logical_path,
                        self.diagnostics,
                        self.standard,
                        true,
                    )?;
                    let expected_type = ClientExpressionType {
                        semantic_type: resolved.semantic_type,
                        standard_value_type: resolved.standard_value_type,
                        result_shape: ClientExpressionResultShape::Value,
                    };
                    if !client_expression_types_compatible(expression_type, expected_type) {
                        self.diagnostics.push(diagnostic(
                                DiagnosticCode::TypeMismatch,
                                format!(
                                    "CLIENT local {local_name} descriptor does not match its SERVER resource result"
                                ),
                                self.input.logical_path,
                                &source.span,
                            ));
                        return None;
                    }
                }
            }
            (
                checked,
                expression_type,
                CheckedClientLocalKind::Resource(actual_kind),
            )
        } else {
            let (checked, expression_type) = self.expression(expression)?;
            if !client_expression_type_is_evaluable(expression_type, self.base, self.standard) {
                self.diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    "this CLIENT local type is not supported by the local evaluator",
                    self.input.logical_path,
                    span,
                ));
                return None;
            }
            if let Some(source) = type_source {
                let Some(specification) = client_type_specification_from_source(source) else {
                    self.diagnostics.push(diagnostic(
                        DiagnosticCode::TypeMismatch,
                        format!("unsupported CLIENT local type for {local_name}"),
                        self.input.logical_path,
                        &source.span,
                    ));
                    return None;
                };
                let resolved = resolve_application_type_with_named_standard(
                    &specification,
                    self.submitted_ids,
                    self.input.logical_path,
                    self.diagnostics,
                    self.standard,
                    true,
                )?;
                let expected_type = ClientExpressionType {
                    semantic_type: resolved.semantic_type,
                    standard_value_type: resolved.standard_value_type,
                    result_shape: ClientExpressionResultShape::Value,
                };
                if !client_expression_types_compatible(expression_type, expected_type) {
                    self.diagnostics.push(diagnostic(
                        DiagnosticCode::TypeMismatch,
                        format!(
                            "CLIENT local {local_name} initializer does not match its declared type"
                        ),
                        self.input.logical_path,
                        &source.span,
                    ));
                    return None;
                }
            }
            (checked, expression_type, CheckedClientLocalKind::Value)
        };

        let ordinal = self.next_ordinal;
        self.next_ordinal = self.next_ordinal.checked_add(1)?;
        self.checked_locals.push(CheckedClientLocal {
            ordinal,
            name: local_name.clone(),
            semantic_type: expression_type.semantic_type,
            standard_value_type: expression_type.standard_value_type,
            kind,
            location: location(self.input.logical_path, span),
        });
        self.locals.insert(
            local_name,
            ClientLocalBinding {
                checked: checked.clone(),
                expression_type,
                ordinal: Some(ordinal),
                kind,
            },
        );
        Some((ordinal, checked))
    }

    fn statements(
        &mut self,
        statements: &[orna_syntax::ClientProceduralStatement],
    ) -> Option<(Vec<CheckedClientControlFlowStatement>, bool)> {
        let mut checked = Vec::with_capacity(statements.len());
        let mut guaranteed_return = false;
        for statement in statements {
            let (statement, statement_returns) = match statement {
                orna_syntax::ClientProceduralStatement::Let(statement) => {
                    let (local, expression) = self.declare_local(
                        &statement.name,
                        statement.type_source.as_ref(),
                        &statement.expression,
                        &statement.span,
                        false,
                    )?;
                    (
                        CheckedClientControlFlowStatement::Let {
                            local,
                            expression,
                            location: location(self.input.logical_path, &statement.span),
                        },
                        false,
                    )
                }
                orna_syntax::ClientProceduralStatement::Assignment(statement) => {
                    let local_name = semantic_part(&statement.target);
                    let Some(binding) = self.locals.get(&local_name).cloned() else {
                        self.diagnostics.push(diagnostic(
                            DiagnosticCode::UnknownQualifiedName,
                            format!("unknown CLIENT local {local_name}"),
                            self.input.logical_path,
                            &statement.target.span,
                        ));
                        return None;
                    };
                    let diagnostics_before = self.diagnostics.len();
                    validate_client_await_positions(
                        &statement.expression,
                        true,
                        self.input,
                        self.diagnostics,
                    );
                    if self.diagnostics.len() != diagnostics_before {
                        return None;
                    }
                    let direct_resource = matches!(
                        &statement.expression,
                        ClientExpression::Call { callee, .. }
                            if resource_constructor_kind(&semantic_name(callee)).is_some()
                    );
                    let (expression, expression_type) =
                        if matches!(binding.kind, CheckedClientLocalKind::Resource(_))
                            && direct_resource
                        {
                            check_resource_constructor(
                                &statement.expression,
                                self.input,
                                self.targets,
                                self.action_targets,
                                self.resource_targets,
                                self.query_catalogue,
                                self.base,
                                self.server_names,
                                self.standard,
                                self.diagnostics,
                                &mut self.references,
                                &mut self.used_capabilities,
                                &self.locals,
                            )?
                        } else {
                            self.expression(&statement.expression)?
                        };
                    if !client_expression_types_compatible(expression_type, binding.expression_type)
                        || (matches!(binding.kind, CheckedClientLocalKind::Resource(_))
                            != matches!(expression, CheckedClientExpression::Resource { .. }))
                    {
                        self.diagnostics.push(diagnostic(
                            DiagnosticCode::TypeMismatch,
                            format!(
                                "CLIENT assignment to local {local_name} does not match its declared type"
                            ),
                            self.input.logical_path,
                            &statement.span,
                        ));
                        return None;
                    }
                    if let Some(binding) = self.locals.get_mut(&local_name) {
                        binding.checked = expression.clone();
                    }
                    (
                        CheckedClientControlFlowStatement::Assignment {
                            local: binding.ordinal.expect("control-flow local has ordinal"),
                            expression,
                            location: location(self.input.logical_path, &statement.span),
                        },
                        false,
                    )
                }
                orna_syntax::ClientProceduralStatement::Return(statement) => {
                    let expression = if let Some(expression) = statement.expression.as_ref() {
                        let diagnostics_before = self.diagnostics.len();
                        validate_client_await_positions(
                            expression,
                            true,
                            self.input,
                            self.diagnostics,
                        );
                        if self.diagnostics.len() != diagnostics_before {
                            return None;
                        }
                        let (checked, expression_type) = self.expression(expression)?;
                        let expected = ClientExpressionType {
                            semantic_type: self.input.return_type,
                            standard_value_type: self.input.standard_value_type,
                            result_shape: self.input.result_shape,
                        };
                        if !client_expression_types_compatible(expression_type, expected) {
                            self.diagnostics.push(diagnostic(
                                DiagnosticCode::TypeMismatch,
                                "this CLIENT RETURN expression does not match the declared value type",
                                self.input.logical_path,
                                expression.span(),
                            ));
                            return None;
                        }
                        Some(checked)
                    } else if self.input.return_type == SemanticType::scalar(StandardScalar::Void) {
                        None
                    } else {
                        self.diagnostics.push(diagnostic(
                            DiagnosticCode::TypeMismatch,
                            "CLIENT RETURN without an expression requires a VOID return type",
                            self.input.logical_path,
                            &statement.span,
                        ));
                        return None;
                    };
                    (
                        CheckedClientControlFlowStatement::Return {
                            expression,
                            location: location(self.input.logical_path, &statement.span),
                        },
                        true,
                    )
                }
                orna_syntax::ClientProceduralStatement::If(statement) => {
                    let incoming = self.locals.clone();
                    let mut branches = Vec::with_capacity(1 + statement.elsif_branches.len());
                    let mut all_return = true;

                    self.locals = incoming.clone();
                    let diagnostics_before = self.diagnostics.len();
                    validate_client_await_positions(
                        &statement.condition,
                        false,
                        self.input,
                        self.diagnostics,
                    );
                    if self.diagnostics.len() != diagnostics_before {
                        return None;
                    }
                    let (condition, condition_type) = self.expression(&statement.condition)?;
                    if control_flow_supported_scalar(condition_type)
                        != Some(StandardScalar::Boolean)
                    {
                        self.diagnostics.push(diagnostic(
                            DiagnosticCode::TypeMismatch,
                            "CLIENT IF condition must be BOOLEAN",
                            self.input.logical_path,
                            statement.condition.span(),
                        ));
                        return None;
                    }
                    self.locals = incoming.clone();
                    let (then_statements, then_returns) =
                        self.statements(&statement.then_statements)?;
                    all_return &= then_returns;
                    branches.push(CheckedClientControlFlowBranch {
                        condition,
                        statements: then_statements,
                        location: location(self.input.logical_path, &statement.span),
                    });

                    for branch in &statement.elsif_branches {
                        self.locals = incoming.clone();
                        let diagnostics_before = self.diagnostics.len();
                        validate_client_await_positions(
                            &branch.condition,
                            false,
                            self.input,
                            self.diagnostics,
                        );
                        if self.diagnostics.len() != diagnostics_before {
                            return None;
                        }
                        let (condition, condition_type) = self.expression(&branch.condition)?;
                        if control_flow_supported_scalar(condition_type)
                            != Some(StandardScalar::Boolean)
                        {
                            self.diagnostics.push(diagnostic(
                                DiagnosticCode::TypeMismatch,
                                "CLIENT ELSIF condition must be BOOLEAN",
                                self.input.logical_path,
                                branch.condition.span(),
                            ));
                            return None;
                        }
                        self.locals = incoming.clone();
                        let (branch_statements, branch_returns) =
                            self.statements(&branch.statements)?;
                        all_return &= branch_returns;
                        branches.push(CheckedClientControlFlowBranch {
                            condition,
                            statements: branch_statements,
                            location: location(self.input.logical_path, &branch.span),
                        });
                    }

                    let (else_statements, else_returns) =
                        if let Some(statements) = statement.else_statements.as_ref() {
                            self.locals = incoming.clone();
                            let (statements, returns) = self.statements(statements)?;
                            (Some(statements), returns)
                        } else {
                            (None, false)
                        };
                    all_return &= else_returns;
                    self.locals = incoming;
                    (
                        CheckedClientControlFlowStatement::If {
                            branches,
                            else_statements,
                            location: location(self.input.logical_path, &statement.span),
                        },
                        all_return,
                    )
                }
                orna_syntax::ClientProceduralStatement::While(statement) => {
                    let diagnostics_before = self.diagnostics.len();
                    validate_client_await_positions(
                        &statement.condition,
                        false,
                        self.input,
                        self.diagnostics,
                    );
                    if self.diagnostics.len() != diagnostics_before {
                        return None;
                    }
                    let incoming = self.locals.clone();
                    let (condition, condition_type) = self.expression(&statement.condition)?;
                    if control_flow_supported_scalar(condition_type)
                        != Some(StandardScalar::Boolean)
                    {
                        self.diagnostics.push(diagnostic(
                            DiagnosticCode::TypeMismatch,
                            "CLIENT WHILE condition must be BOOLEAN",
                            self.input.logical_path,
                            statement.condition.span(),
                        ));
                        return None;
                    }
                    self.locals = incoming.clone();
                    let (statements, _) = self.statements(&statement.body)?;
                    self.locals = incoming;
                    (
                        CheckedClientControlFlowStatement::While {
                            condition,
                            statements,
                            location: location(self.input.logical_path, &statement.span),
                        },
                        false,
                    )
                }
            };
            guaranteed_return |= statement_returns;
            checked.push(statement);
        }
        Some((checked, guaranteed_return))
    }

    fn finish_capabilities(&mut self) -> bool {
        for capability in self.input.capabilities {
            let capability_name = semantic_name(&capability.name);
            if !self.used_capabilities.contains(&capability_name) {
                self.diagnostics.push(diagnostic(
                    DiagnosticCode::CapabilityRequirement,
                    format!("declared CLIENT capability {capability_name} is not exercised"),
                    self.input.logical_path,
                    &self.input.declaration_span,
                ));
                return false;
            }
        }
        true
    }

    fn finish_direct_expression(
        mut self,
        expression: &ClientExpression,
        allow_await: bool,
    ) -> Option<(
        CheckedClientFunctionBody,
        ClientExpressionType,
        SourceLocation,
        Vec<CheckedDefinitionReference>,
    )> {
        let diagnostics_before = self.diagnostics.len();
        validate_client_await_positions(expression, allow_await, self.input, self.diagnostics);
        if self.diagnostics.len() != diagnostics_before {
            return None;
        }
        let (checked, expression_type) = self.expression(expression)?;
        let expected = ClientExpressionType {
            semantic_type: self.input.return_type,
            standard_value_type: self.input.standard_value_type,
            result_shape: self.input.result_shape,
        };
        if !client_expression_types_compatible(expression_type, expected) {
            self.diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                "this CLIENT function must return the declared value type",
                self.input.logical_path,
                expression.span(),
            ));
            return None;
        }
        if !self.finish_capabilities() {
            return None;
        }
        let location = location(self.input.logical_path, expression.span());
        Some((
            CheckedClientFunctionBody::ControlFlow {
                locals: Vec::new(),
                statements: vec![CheckedClientControlFlowStatement::Return {
                    expression: Some(checked),
                    location: location.clone(),
                }],
            },
            expression_type,
            location,
            self.references,
        ))
    }

    fn finish_block(
        mut self,
        block: &orna_syntax::ClientStateBlockBody,
    ) -> Option<(
        CheckedClientFunctionBody,
        ClientExpressionType,
        SourceLocation,
        Vec<CheckedDefinitionReference>,
    )> {
        if !block.states.is_empty() {
            self.diagnostics.push(diagnostic(
                DiagnosticCode::DomainIncompatible,
                "CLIENT state blocks cannot contain programmable control flow",
                self.input.logical_path,
                &block.span,
            ));
            return None;
        }

        let mut checked_statements = Vec::new();
        for local in &block.locals {
            let (ordinal, expression) = self.declare_local(
                &local.name,
                Some(&local.type_source),
                &local.expression,
                &local.span,
                false,
            )?;
            checked_statements.push(CheckedClientControlFlowStatement::Let {
                local: ordinal,
                expression,
                location: location(self.input.logical_path, &local.span),
            });
        }
        let (statements, mut guaranteed_return) = self.statements(&block.statements)?;
        checked_statements.extend(statements);

        let mut body_type = ClientExpressionType {
            semantic_type: self.input.return_type,
            standard_value_type: self.input.standard_value_type,
            result_shape: self.input.result_shape,
        };
        if let Some(expression) = block.return_expression.as_ref() {
            let diagnostics_before = self.diagnostics.len();
            validate_client_await_positions(expression, true, self.input, self.diagnostics);
            if self.diagnostics.len() != diagnostics_before {
                return None;
            }
            let (checked, expression_type) = self.expression(expression)?;
            if !client_expression_types_compatible(
                expression_type,
                ClientExpressionType {
                    semantic_type: self.input.return_type,
                    standard_value_type: self.input.standard_value_type,
                    result_shape: self.input.result_shape,
                },
            ) {
                self.diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    "this CLIENT function must return the declared value type",
                    self.input.logical_path,
                    expression.span(),
                ));
                return None;
            }
            body_type = expression_type;
            checked_statements.push(CheckedClientControlFlowStatement::Return {
                expression: Some(checked),
                location: location(self.input.logical_path, expression.span()),
            });
            guaranteed_return = true;
        }
        if !guaranteed_return {
            self.diagnostics.push(diagnostic(
                DiagnosticCode::DomainIncompatible,
                "CLIENT control-flow blocks must return on every path",
                self.input.logical_path,
                &block.span,
            ));
            return None;
        }
        if !self.finish_capabilities() {
            return None;
        }
        Some((
            CheckedClientFunctionBody::ControlFlow {
                locals: self.checked_locals,
                statements: checked_statements,
            },
            body_type,
            location(self.input.logical_path, &block.span),
            self.references,
        ))
    }
}

#[allow(clippy::too_many_arguments)]
fn check_client_control_flow_body(
    input: &ResolvedClientFunctionInput<'_>,
    submitted_ids: &HashMap<QualifiedSemanticName, SubmittedType>,
    targets: &HashMap<QualifiedSemanticName, ClientExpressionTarget>,
    action_targets: &HashMap<QualifiedSemanticName, ClientActionTarget>,
    resource_targets: &HashMap<QualifiedSemanticName, ClientResourceTarget>,
    query_catalogue: &ResolutionCatalogue<CheckedTypeId, CheckedFieldId>,
    base: &CatalogueSnapshot,
    server_names: &[QualifiedSemanticName],
    standard: Option<&CheckedStandardLibrary>,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) -> Option<(
    CheckedClientFunctionBody,
    ClientExpressionType,
    SourceLocation,
    Vec<CheckedDefinitionReference>,
)> {
    let checker = ClientControlFlowChecker::new(
        input,
        submitted_ids,
        targets,
        action_targets,
        resource_targets,
        query_catalogue,
        base,
        server_names,
        standard,
        diagnostics,
    );
    match input.body {
        orna_syntax::ClientFunctionBody::Expression { expression } => {
            checker.finish_direct_expression(expression, false)
        }
        orna_syntax::ClientFunctionBody::ReturnExpression { expression } => {
            checker.finish_direct_expression(expression, true)
        }
        orna_syntax::ClientFunctionBody::StateBlock(block) => checker.finish_block(block),
        orna_syntax::ClientFunctionBody::BooleanLiteral { .. }
        | orna_syntax::ClientFunctionBody::ExternalContract { .. } => None,
        _ => None,
    }
}

fn is_closed_client_boolean_return(specification: &TypeSpecification) -> bool {
    let TypeSpecification::Named(name) = specification else {
        return false;
    };
    if name.parts.len() != 1 || name.parts[0].text.starts_with('"') {
        return false;
    }
    let spelling = &name.parts[0].text;
    spelling.eq_ignore_ascii_case("BOOLEAN") || spelling.eq_ignore_ascii_case("BOOL")
}

fn is_standard_client_boolean_return(specification: &TypeSpecification) -> bool {
    if is_closed_client_boolean_return(specification) {
        return true;
    }
    let TypeSpecification::Named(name) = specification else {
        return false;
    };
    match semantic_name(name).parts() {
        [schema, value_type] => schema == "std" && value_type == "boolean",
        [schema, types, value_type] => {
            schema == "std" && types == "types" && value_type == "boolean"
        }
        _ => false,
    }
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
