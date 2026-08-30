use super::{CheckedClientExpression, CheckedClientFunctionBody, ClientResourceTypeParser};
use std::{cell::Cell, error::Error};

use crate::relational::ExpressionKind;
use orna_artifact::server_mutation_plan::{
    MutationExpressionKind as ServerMutationExpressionKind, RECORD_INSERT_FORMAT_VERSION,
    RecordFieldExpressionKind as ServerRecordFieldExpressionKind, ServerMutationPlan,
};
use orna_artifact::server_parameter_echo::ServerParameterEcho;
use orna_core::{
    CatalogueRevisionId, ExpressionId, FieldId, FunctionId, FunctionRevisionId, ParameterId,
    SchemaId, SourceBundleId, SourceRevisionId, SourceUnitId, StandardLibraryRevisionId, TypeId,
    canonical_hash::{
        artifact_payload_digest, calculate_standard_library_digest, catalogue_digest_with_context,
        function_declaration_digest, function_semantic_digest_with_version, source_bundle_digest,
        source_revision_record_digest, source_unit_content_digest,
        verify_standard_library_snapshot, verify_standard_library_v2_snapshot,
    },
    catalogue::{
        CatalogueSnapshot, CatalogueSnapshotError, EnumTypeDefinition, FieldDefinition,
        FunctionDefinition, FunctionDomain, FunctionReturn, FunctionReturnColumnDefinition,
        FunctionSecurity, FunctionTransaction, FunctionVolatility, ObjectTypeDefinition,
        OnDeleteAction, ParameterDefinition, PreludeTypeName, QualifiedSemanticName,
        SchemaDefinition, TypeBinding, TypeLookupName, ValueTypeDefinition, ValueTypeKind,
        ValueTypeMutability, ValueTypePersistence,
    },
    revision::{
        ActiveDatabaseRevision, ActiveDatabaseRevisionInput, ActiveRevisionContent,
        CatalogueHashContext, DefinitionIdentity, DefinitionOrigin, DefinitionReference,
        DefinitionReferenceKind, DefinitionReferenceTarget, DeployableRevision, ExecutableArtifact,
        ExecutableArtifactKind, FunctionRevisionRecord, FunctionSemanticHashVersion, RevisionPair,
        Sha256Digest, SourceOrigin, StandardExecutable, StandardLibraryDigestVersion,
        StandardLibrarySnapshot, StoredSourceRevision, StoredSourceUnit,
        VerifiedStandardLibrarySnapshot,
    },
    source::{SourceBundle, SourceUnit},
    types::{ResolvedType, StandardScalar},
};
use orna_syntax::{
    FunctionSecurity as SyntaxFunctionSecurity, FunctionTransaction as SyntaxFunctionTransaction,
    FunctionVolatility as SyntaxFunctionVolatility, ServerFunctionDeclaration, SourceSlice,
    SourceSpan, TypeExportTarget, TypeSpecification, parse,
};

use super::{
    CheckAssignments, CheckedApplicationTypeUse, CheckedClientReturnShape,
    CheckedDefinitionReferenceTarget, CheckedStandardExecutable, CheckedStandardJsonEncode,
    CheckedStandardParameterEcho, CheckedStandardTerminalPresentTable, CheckedStateDefault,
    CheckedTypeId, CheckedTypeUseKind, CheckedValueTypeUse, ClientExpressionResultShape,
    ClientExpressionType, ConstantValue, DiagnosticCode, IdentityAssignments,
    NewApplicationCheckError, STANDARD_LIBRARY_V3_REVISION_ID, STANDARD_LIBRARY_V4_REVISION_ID,
    STD_ACTION_CONTRACT, STD_ACTION_SCHEMA_ID, STD_ACTION_SOURCE_UNIT_ID, STD_ACTION_TYPE_ID,
    STD_DATA_ROWS_TYPE_ID, STD_DATA_SCHEMA_ID, STD_INTEGER_TYPE_ID, STD_INVOKE_ECHO_FUNCTION_ID,
    STD_INVOKE_ECHO_FUNCTION_REVISION_ID, STD_INVOKE_ECHO_PARAMETER_ID,
    STD_INVOKE_ECHO_REVISION_NUMBER, STD_INVOKE_SCHEMA_ID, STD_INVOKE_SOURCE_UNIT_ID,
    STD_IO_BYTE_STREAM_TYPE_ID, STD_IO_SCHEMA_ID, STD_JSON_CONTRACT, STD_JSON_ENCODE_FUNCTION_ID,
    STD_JSON_ENCODE_FUNCTION_REVISION_ID, STD_JSON_ENCODE_PARAMETER_ID, STD_JSON_SCHEMA_ID,
    STD_JSON_SOURCE_UNIT_ID, STD_JSON_VALUE_TYPE_ID, STD_OUTPUT_SOURCE_UNIT_ID,
    STD_TERMINAL_DOCUMENT_TYPE_ID, STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
    STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID, STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
    STD_TERMINAL_SCHEMA_ID, STD_TYPES_SOURCE_UNIT_ID, STD_UI_CONTRACT, STD_UI_SCHEMA_ID,
    STD_UI_SOURCE_UNIT_ID, STD_UI_TYPE_ID, SemanticType, StandardApplicationCheckContext,
    StandardApplicationContextError, StandardLibraryCheckError, StandardSourceFamilies, check,
    check_new_application, check_new_application_with_catalogue, check_standard_application,
    check_standard_json_encode, check_standard_library_source,
    check_standard_library_source_v1_identity, check_standard_library_source_v2_parts,
    check_standard_library_source_v3_parts, check_standard_library_source_v4_parts,
    check_standard_library_source_v5_parts, check_standard_library_source_v6_parts,
    check_standard_parameter_echo, check_standard_terminal_present_table,
    checked_standard_library_with_contract_overrides_for_test,
    client_resource_stream_type_is_supported, expected_standard_json_executable, location,
    reconcile_standard_executable, reconcile_standard_json_executable, reconcile_standard_source,
    sort_standard_type_uses, supports_record_value_scalar, unquoted_prelude_name,
    unquoted_semantic_name, validate_client_capability,
};
use crate::mutation::{MutationExpressionKind, MutationRecordFieldExpressionKind};
use crate::{
    ParsedSourceUnit, PrepareError, PrepareStandardApplicationError, parse_bundle,
    prepare_standard_application,
};

const STANDARD_SOURCE: &str = "CREATE SCHEMA std;CREATE SCHEMA std.types;CREATE TYPE std.types.BOOLEAN AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.boolean@1' IMMUTABLE PERSISTABLE;EXPORT TYPE std.types.BOOLEAN AS std.BOOLEAN;EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;";
const STANDARD_SOURCE_UNIT_ID: SourceUnitId =
    SourceUnitId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
const TWO_TYPE_STANDARD_SOURCE: &str = "CREATE SCHEMA std.types;CREATE SCHEMA std;CREATE TYPE std.types.INTEGER AS VALUE PRIMITIVE KERNEL CONTRACT 'int@1' IMMUTABLE TRANSIENT;CREATE TYPE std.types.BOOLEAN AS VALUE PRIMITIVE KERNEL CONTRACT 'boolean@1' IMMUTABLE PERSISTABLE;EXPORT TYPE std.INTEGER TO PRELUDE AS INTEGER;EXPORT TYPE std.types.INTEGER AS std.INTEGER;EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;EXPORT TYPE std.types.BOOLEAN AS std.BOOLEAN;";
const LEGACY_CANONICAL_SCALAR_SPELLINGS: [&str; 13] = [
    "BOOLEAN",
    "INTEGER",
    "BIGINT",
    "FLOAT",
    "DECIMAL",
    "CHARACTER LARGE OBJECT",
    "BINARY LARGE OBJECT",
    "UUID",
    "DATE",
    "TIME",
    "TIMESTAMP",
    "DURATION",
    "VOID",
];

fn empty_catalogue() -> CatalogueSnapshot {
    catalogue(Vec::new(), Vec::new(), Vec::new())
}

fn legacy_type_specification(spelling: &str) -> TypeSpecification {
    let source = format!("CREATE TYPE app.value AS OBJECT (value {spelling});");
    let parsed = parse(&source);

    assert!(
        parsed.diagnostics().is_empty(),
        "{source}: {:?}",
        parsed.diagnostics()
    );
    parsed.object_types()[0].fields[0]
        .type_specification
        .clone()
}

#[test]
fn stream_resource_type_guard_matches_runtime_collection_scalar_boundary() {
    let base = empty_catalogue();
    let scalar = |semantic_type| ClientExpressionType {
        semantic_type: SemanticType::Scalar(semantic_type),
        standard_value_type: None,
        result_shape: ClientExpressionResultShape::Value,
    };

    for supported in [
        StandardScalar::Boolean,
        StandardScalar::Integer,
        StandardScalar::BigInt,
        StandardScalar::Float,
        StandardScalar::CharacterLargeObject,
        StandardScalar::BinaryLargeObject,
    ] {
        assert!(client_resource_stream_type_is_supported(
            scalar(supported),
            &base,
            None,
        ));
    }
    for unsupported in [
        StandardScalar::Decimal,
        StandardScalar::Uuid,
        StandardScalar::Date,
        StandardScalar::Time,
        StandardScalar::Timestamp,
        StandardScalar::Duration,
        StandardScalar::Void,
    ] {
        assert!(!client_resource_stream_type_is_supported(
            scalar(unsupported),
            &base,
            None,
        ));
    }
}

#[test]
fn legacy_scalar_compatibility_adapter_has_the_closed_spelling_matrix() {
    for (spelling, expected) in [
        ("BOOLEAN", StandardScalar::Boolean),
        ("BOOL", StandardScalar::Boolean),
        ("INTEGER", StandardScalar::Integer),
        ("INT", StandardScalar::Integer),
        ("BIGINT", StandardScalar::BigInt),
        ("FLOAT", StandardScalar::Float),
        ("DECIMAL", StandardScalar::Decimal),
        (
            "CHARACTER LARGE OBJECT",
            StandardScalar::CharacterLargeObject,
        ),
        ("TEXT", StandardScalar::CharacterLargeObject),
        ("BINARY LARGE OBJECT", StandardScalar::BinaryLargeObject),
        ("BYTES", StandardScalar::BinaryLargeObject),
        ("UUID", StandardScalar::Uuid),
        ("DATE", StandardScalar::Date),
        ("TIME", StandardScalar::Time),
        ("TIMESTAMP", StandardScalar::Timestamp),
        ("DURATION", StandardScalar::Duration),
        ("VOID", StandardScalar::Void),
    ] {
        for spelling in [spelling.to_owned(), spelling.to_ascii_lowercase()] {
            let mut diagnostics = Vec::new();
            let resolved = super::resolve_application_type(
                &legacy_type_specification(&spelling),
                &std::collections::HashMap::new(),
                "legacy.orna",
                &mut diagnostics,
                None,
            );

            assert_eq!(
                resolved.map(|resolved| resolved.semantic_type),
                Some(SemanticType::scalar(expected)),
                "{spelling}"
            );
            assert!(diagnostics.is_empty(), "{spelling}");
        }
    }

    for spelling in [
        "BYTEA",
        "BLOB",
        "CLOB",
        "SERIAL",
        "JSONB",
        "TIMESTAMPTZ",
        "\"BOOLEAN\"",
        "std.BOOLEAN",
        "std.types.BOOLEAN",
    ] {
        let mut diagnostics = Vec::new();
        let resolved = super::resolve_application_type(
            &legacy_type_specification(spelling),
            &std::collections::HashMap::new(),
            "legacy.orna",
            &mut diagnostics,
            None,
        );

        assert!(resolved.is_none(), "{spelling}");
        assert_eq!(diagnostics.len(), 1, "{spelling}");
        assert_eq!(diagnostics[0].code(), DiagnosticCode::UnknownQualifiedName);
    }
}

#[test]
fn parsed_constructed_types_do_not_open_semantic_positions() {
    for spelling in [
        "LIST<BOOL>",
        "SET<BOOL>",
        "MAP<TEXT, BOOL>",
        "OPTION<BOOL>",
        "BOOL?",
        "STREAM<BOOL>",
    ] {
        let specification = legacy_type_specification(spelling);
        let mut diagnostics = Vec::new();
        let resolved = super::resolve_application_type(
            &specification,
            &std::collections::HashMap::new(),
            "constructed.orna",
            &mut diagnostics,
            None,
        );

        assert!(resolved.is_none(), "{spelling}");
        assert_eq!(diagnostics.len(), 1, "{spelling}");
        assert_eq!(diagnostics[0].code(), DiagnosticCode::TypeMismatch);
        assert_eq!(
            diagnostics[0].message(),
            "constructed types are not admitted in this position"
        );
        assert_eq!(
            diagnostics[0].location().span().end() - diagnostics[0].location().span().start(),
            spelling.len()
        );
    }

    let specification = legacy_type_specification("REF LIST<BOOL>");
    let mut diagnostics = Vec::new();
    assert!(
        super::resolve_application_type(
            &specification,
            &std::collections::HashMap::new(),
            "constructed.orna",
            &mut diagnostics,
            None,
        )
        .is_none()
    );
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(
        diagnostics[0].code(),
        DiagnosticCode::InvalidReferenceTarget
    );
    assert_eq!(
        diagnostics[0].message(),
        "REF target must be one named object type"
    );
}

#[test]
fn resolves_named_standard_types_in_server_signature_positions() {
    let standard =
        check_standard_library_source(&verified_standard_library_for_relational_test()).unwrap();
    let source = "CREATE SCHEMA app; CREATE TYPE app.row AS OBJECT (value BOOLEAN); \
            CREATE SERVER FUNCTION app.parameter(p_value BOOLEAN) RETURNS BOOLEAN AS SELECT TRUE FROM app.row r; \
            CREATE SERVER FUNCTION app.single() RETURNS BOOLEAN AS SELECT TRUE FROM app.row r; \
            CREATE SERVER FUNCTION app.stream() RETURNS STREAM<BOOLEAN> AS SELECT TRUE FROM app.row r; \
            CREATE SERVER FUNCTION app.unknown(p_value std.missing) RETURNS std.missing AS SELECT TRUE FROM app.row r;";
    let parsed = parse_bundle(&bundle([("server-signatures.orna", source)]));
    assert!(
        parsed.diagnostics().is_empty(),
        "{:?}",
        parsed.diagnostics()
    );

    let mut assignments = CheckAssignments::new();
    let mut function_ids = std::collections::HashMap::new();
    for declaration in parsed.units()[0].parsed().server_functions() {
        function_ids.insert(
            super::semantic_name(&declaration.name),
            assignments.function_id(None),
        );
    }
    let headers = super::resolve_server_function_headers(&parsed, &function_ids);
    let mut diagnostics = Vec::new();
    let mut uses = Vec::new();
    let base = empty_catalogue();
    let inputs = super::resolve_server_function_inputs(
        &headers,
        &std::collections::HashMap::new(),
        &base,
        &mut assignments,
        &mut diagnostics,
        Some(&standard),
        &mut uses,
    );

    assert_eq!(inputs.len(), 3);
    assert_eq!(diagnostics.len(), 2);
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code() == DiagnosticCode::UnknownQualifiedName)
    );
    let boolean = TypeId::from_bytes([3; 16]);
    assert!(inputs.iter().all(|input| {
        let return_type = match &input.return_type {
            super::ResolvedServerFunctionReturn::Single { semantic_type, .. }
            | super::ResolvedServerFunctionReturn::Stream { semantic_type, .. } => *semantic_type,
            super::ResolvedServerFunctionReturn::Rows { .. } => return false,
        };
        return_type == SemanticType::scalar(StandardScalar::Boolean)
    }));
    assert_eq!(inputs[0].parameters.len(), 1);
    assert_eq!(
        inputs[0].parameters[0].semantic_type,
        SemanticType::scalar(StandardScalar::Boolean)
    );
    assert_eq!(inputs[0].parameters[0].standard_value_type, Some(boolean));
    assert_eq!(uses.len(), 4);
    assert!(uses.iter().all(|use_| {
        matches!(
            use_,
            CheckedApplicationTypeUse::Value(value) if value.type_id() == boolean
        )
    }));
    assert!(
        uses.iter()
            .any(|use_| matches!(use_.kind(), CheckedTypeUseKind::Parameter { .. }))
    );
    assert_eq!(
        uses.iter()
            .filter(|use_| matches!(use_.kind(), CheckedTypeUseKind::Return { .. }))
            .count(),
        3
    );
}

#[test]
fn canonical_type_use_order_breaks_coincident_same_kind_ties() {
    let mut assignments = CheckAssignments::new();
    let first_field_owner = assignments.type_id(Some(TypeId::from_bytes([0x10; 16])));
    let second_field_owner = assignments.type_id(Some(TypeId::from_bytes([0x20; 16])));
    let first_field = assignments.field_id(Some(FieldId::from_bytes([0x10; 16])));
    let second_field = assignments.field_id(Some(FieldId::from_bytes([0x20; 16])));
    let first_return_owner = assignments.function_id(Some(FunctionId::from_bytes([0x10; 16])));
    let second_return_owner = assignments.function_id(Some(FunctionId::from_bytes([0x20; 16])));
    let first_parameter = assignments.parameter_id(Some(ParameterId::from_bytes([0x10; 16])));
    let second_parameter = assignments.parameter_id(Some(ParameterId::from_bytes([0x20; 16])));
    let span = SourceSpan { start: 0, end: 0 };
    let type_id = TypeId::from_bytes([0x55; 16]);
    let mut uses = vec![
        CheckedApplicationTypeUse::Value(CheckedValueTypeUse {
            type_id,
            kind: CheckedTypeUseKind::Return {
                owner: second_return_owner,
                ordinal: 1,
            },
            location: location("application.orna", &span),
        }),
        CheckedApplicationTypeUse::Value(CheckedValueTypeUse {
            type_id,
            kind: CheckedTypeUseKind::Field {
                owner: second_field_owner,
                field: second_field,
            },
            location: location("application.orna", &span),
        }),
        CheckedApplicationTypeUse::Value(CheckedValueTypeUse {
            type_id,
            kind: CheckedTypeUseKind::Return {
                owner: second_return_owner,
                ordinal: 0,
            },
            location: location("application.orna", &span),
        }),
        CheckedApplicationTypeUse::Value(CheckedValueTypeUse {
            type_id,
            kind: CheckedTypeUseKind::Field {
                owner: first_field_owner,
                field: first_field,
            },
            location: location("application.orna", &span),
        }),
        CheckedApplicationTypeUse::Value(CheckedValueTypeUse {
            type_id,
            kind: CheckedTypeUseKind::Parameter {
                owner: second_return_owner,
                parameter: second_parameter,
            },
            location: location("application.orna", &span),
        }),
        CheckedApplicationTypeUse::Value(CheckedValueTypeUse {
            type_id,
            kind: CheckedTypeUseKind::Parameter {
                owner: first_return_owner,
                parameter: first_parameter,
            },
            location: location("application.orna", &span),
        }),
        CheckedApplicationTypeUse::Value(CheckedValueTypeUse {
            type_id,
            kind: CheckedTypeUseKind::Return {
                owner: first_return_owner,
                ordinal: 1,
            },
            location: location("application.orna", &span),
        }),
        CheckedApplicationTypeUse::Value(CheckedValueTypeUse {
            type_id,
            kind: CheckedTypeUseKind::State {
                owner: second_return_owner,
                ordinal: 0,
            },
            location: location("application.orna", &span),
        }),
        CheckedApplicationTypeUse::Value(CheckedValueTypeUse {
            type_id,
            kind: CheckedTypeUseKind::Result {
                owner: second_return_owner,
                ordinal: 1,
            },
            location: location("application.orna", &span),
        }),
        CheckedApplicationTypeUse::Value(CheckedValueTypeUse {
            type_id,
            kind: CheckedTypeUseKind::Expression {
                owner: second_return_owner,
                ordinal: 1,
            },
            location: location("application.orna", &span),
        }),
        CheckedApplicationTypeUse::Value(CheckedValueTypeUse {
            type_id,
            kind: CheckedTypeUseKind::Result {
                owner: first_return_owner,
                ordinal: 1,
            },
            location: location("application.orna", &span),
        }),
        CheckedApplicationTypeUse::Value(CheckedValueTypeUse {
            type_id,
            kind: CheckedTypeUseKind::Expression {
                owner: first_return_owner,
                ordinal: 1,
            },
            location: location("application.orna", &span),
        }),
    ];
    let report = parse_bundle(&bundle([("application.orna", "")]));

    sort_standard_type_uses(&mut uses, &report);

    assert_eq!(
        uses.iter()
            .map(CheckedApplicationTypeUse::kind)
            .collect::<Vec<_>>(),
        vec![
            CheckedTypeUseKind::Field {
                owner: first_field_owner,
                field: first_field,
            },
            CheckedTypeUseKind::Field {
                owner: second_field_owner,
                field: second_field,
            },
            CheckedTypeUseKind::Parameter {
                owner: first_return_owner,
                parameter: first_parameter,
            },
            CheckedTypeUseKind::Parameter {
                owner: second_return_owner,
                parameter: second_parameter,
            },
            CheckedTypeUseKind::State {
                owner: second_return_owner,
                ordinal: 0,
            },
            CheckedTypeUseKind::Return {
                owner: second_return_owner,
                ordinal: 0,
            },
            CheckedTypeUseKind::Return {
                owner: first_return_owner,
                ordinal: 1,
            },
            CheckedTypeUseKind::Return {
                owner: second_return_owner,
                ordinal: 1,
            },
            CheckedTypeUseKind::Expression {
                owner: first_return_owner,
                ordinal: 1,
            },
            CheckedTypeUseKind::Expression {
                owner: second_return_owner,
                ordinal: 1,
            },
            CheckedTypeUseKind::Result {
                owner: first_return_owner,
                ordinal: 1,
            },
            CheckedTypeUseKind::Result {
                owner: second_return_owner,
                ordinal: 1,
            },
        ]
    );
}

#[test]
fn new_application_checker_orders_cardinality_catalogue_and_context_gates() {
    let snapshot = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&snapshot).unwrap();
    let empty = bundle([]);
    let two_units = bundle([
        ("first.orna", "CREATE SCHEMA ;"),
        ("second.orna", "CREATE SCHEMA ;"),
    ]);

    for (bundle, actual) in [(&empty, 0), (&two_units, 2)] {
        let catalogue_was_constructed = Cell::new(false);
        let error = check_new_application_with_catalogue(bundle, &standard, || {
            catalogue_was_constructed.set(true);
            Err(CatalogueSnapshotError::DuplicateSchemaId {
                id: SchemaId::from_bytes([0; 16]),
            })
        })
        .unwrap_err();
        assert_eq!(error, NewApplicationCheckError::SourceUnitCount { actual });
        assert_eq!(error.clone(), error);
        assert_eq!(
            error.to_string(),
            format!("new-application check requires exactly one source unit; received {actual}")
        );
        assert!(Error::source(&error).is_none());
        assert!(!catalogue_was_constructed.get());
    }

    let hostile_standard =
        checked_standard_library_with_contract_overrides_for_test(&snapshot, &[(0, "other@1")])
            .unwrap();
    let source = bundle([("application.orna", "CREATE SCHEMA ;")]);
    let catalogue_source = CatalogueSnapshotError::DuplicateSchemaId {
        id: SchemaId::from_bytes([0; 16]),
    };
    let catalogue_error = check_new_application_with_catalogue(&source, &hostile_standard, || {
        Err(catalogue_source.clone())
    })
    .unwrap_err();
    assert_eq!(
        catalogue_error,
        NewApplicationCheckError::Catalogue {
            source: catalogue_source,
        }
    );
    assert_eq!(
        catalogue_error.to_string(),
        "new-application check could not create the empty application catalogue: duplicate schema identity schema:00000000000000000000000000"
    );
    assert_eq!(
        Error::source(&catalogue_error).map(ToString::to_string),
        Some("duplicate schema identity schema:00000000000000000000000000".to_owned())
    );

    let context_error = check_new_application(&source, &hostile_standard).unwrap_err();
    let expected_context = StandardApplicationContextError::UnsupportedCompatibilityContract {
        type_id: TypeId::from_bytes([3; 16]),
        contract: "other@1".to_owned(),
    };
    assert_eq!(
        context_error,
        NewApplicationCheckError::Context {
            source: expected_context.clone(),
        }
    );
    assert_eq!(
        context_error.to_string(),
        "new-application check could not establish the standard application context: the standard value type type:0c1g60r30c1g60r30c1g60r30c uses unsupported compatibility contract other@1"
    );
    assert_eq!(
            Error::source(&context_error).map(ToString::to_string),
            Some(
                "the standard value type type:0c1g60r30c1g60r30c1g60r30c uses unsupported compatibility contract other@1"
                    .to_owned()
            )
        );
}

#[test]
fn application_diagnostics_are_ordered_by_source_unit_and_span() {
    let first_source = "CREATE SCHEMA app;\nCREATE TYPE app.item AS OBJECT (value app.missing);\nCREATE SCHEMA app;";
    let second_source = "CREATE SCHEMA other;\nCREATE SCHEMA other;";
    let report = check(
        &bundle([("first.orna", first_source), ("second.orna", second_source)]),
        &empty_catalogue(),
    );

    let expected = [
        (
            "first.orna",
            first_source.find("app.missing").unwrap(),
            DiagnosticCode::UnknownQualifiedName,
        ),
        (
            "first.orna",
            first_source.rfind("app").unwrap(),
            DiagnosticCode::DuplicateDefinition,
        ),
        (
            "second.orna",
            second_source.rfind("other").unwrap(),
            DiagnosticCode::DuplicateDefinition,
        ),
    ];
    assert_eq!(report.diagnostics().len(), expected.len());
    for (diagnostic, (logical_path, start, code)) in report.diagnostics().iter().zip(expected) {
        assert_eq!(diagnostic.location().logical_path(), logical_path);
        assert_eq!(diagnostic.location().span().start(), start);
        assert_eq!(diagnostic.code(), code);
    }
    assert_no_checked_bundle(&report);
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

fn parameter(id: u8, name: &str, ordinal: u32, resolved_type: ResolvedType) -> ParameterDefinition {
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

#[test]
fn accepts_the_ordinary_inspector_signature_and_nested_projection_calls() {
    let source = "CREATE SCHEMA devtools; CREATE CLIENT FUNCTION devtools.inspect(p_target REF sys.inspect.invocation) RETURNS sys.inspect.snapshot IS BEGIN RETURN sys.inspect.snapshot(p_target => p_target); END; CREATE CLIENT FUNCTION devtools.project(p_target REF sys.inspect.invocation) RETURNS sys.inspect.calls IS BEGIN RETURN sys.inspect.calls(p_snapshot => sys.inspect.snapshot(p_target => p_target)); END;";
    let report = check(&bundle([("inspector.orna", source)]), &empty_catalogue());
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let checked = report.checked_bundle().unwrap();
    let clients = checked.client_functions();
    assert_eq!(clients.len(), 2);
    assert!(matches!(
        clients[0].body(),
        CheckedClientFunctionBody::Expression {
            expression: CheckedClientExpression::Inspect {
                operation: super::CheckedInspectOperation::Snapshot { .. }
            }
        }
    ));
    assert!(matches!(
        clients[1].body(),
        CheckedClientFunctionBody::Expression {
            expression: CheckedClientExpression::Inspect {
                operation: super::CheckedInspectOperation::Projection {
                    projection: super::CheckedInspectProjection::Calls,
                    ..
                }
            }
        }
    ));
}

#[test]
fn checked_client_calls_follow_reference_order_and_retain_locations() {
    let source = "CREATE SCHEMA app; \
            CREATE CLIENT FUNCTION app.first() RETURNS INTEGER AS 1; \
            CREATE CLIENT FUNCTION app.second(p_value INTEGER) RETURNS INTEGER AS p_value; \
            CREATE CLIENT FUNCTION app.wrapper() RETURNS INTEGER IS \
                BEGIN \
                    IF app.first() = 1 THEN \
                        RETURN app.second(p_value => app.first()); \
                    ELSE \
                        RETURN app.first(); \
                    END IF; \
                END;";
    let report = check(&bundle([("calls.orna", source)]), &empty_catalogue());
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let checked = report.checked_bundle().unwrap();
    let wrapper = &checked.client_functions()[2];
    let calls = wrapper.called_functions();
    assert_eq!(calls.len(), 4);
    let names = calls
        .iter()
        .map(|id| {
            checked
                .function(*id)
                .expect("call target must be a checked CLIENT function")
                .name()
                .to_string()
        })
        .collect::<Vec<_>>();
    assert_eq!(names, ["app.first", "app.first", "app.second", "app.first"]);
    let call_references = wrapper
        .references()
        .iter()
        .filter(|reference| reference.kind() == DefinitionReferenceKind::FunctionCall)
        .collect::<Vec<_>>();
    assert_eq!(call_references.len(), 4);
    assert_eq!(
        call_references
            .iter()
            .map(|reference| reference.location().span().start())
            .collect::<Vec<_>>(),
        vec![
            source.find("app.first() = 1").unwrap(),
            source.find("app.first());").unwrap(),
            source.find("app.second(p_value =>").unwrap(),
            source.rfind("app.first();").unwrap(),
        ]
    );
}

#[test]
fn lowers_ordinary_client_call_with_canonical_target_identities_and_reference() {
    let integer = ResolvedType::Scalar(StandardScalar::Integer);
    let target_id = FunctionId::from_bytes([0x61; 16]);
    let target_first_parameter_id = ParameterId::from_bytes([0x62; 16]);
    let target_second_parameter_id = ParameterId::from_bytes([0x63; 16]);
    let base = catalogue(
        vec![schema(1, &["tasks"])],
        Vec::new(),
        vec![FunctionDefinition::new(
            target_id,
            QualifiedSemanticName::new(["tasks", "add"]).unwrap(),
            FunctionDomain::Client,
            vec![
                parameter(0x62, "p_first", 0, integer),
                parameter(0x63, "p_second", 1, integer),
            ],
            FunctionReturn::Single(integer),
            FunctionRevisionId::from_bytes([0x64; 16]),
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        )],
    );
    let source = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.call(p_input INTEGER) RETURNS INTEGER AS tasks.add(p_first => p_input, p_second => 7);";
    let report = check(&bundle([("client-call.orna", source)]), &base);
    assert_eq!(report.diagnostics(), &[], "{:?}", report.diagnostics());

    let function = report
        .checked_bundle()
        .unwrap()
        .client_functions()
        .iter()
        .next()
        .unwrap();
    let caller_parameter_id = function.parameters()[0].id();
    let CheckedClientFunctionBody::Expression { expression } = function.body() else {
        panic!("ordinary CLIENT call body was not an expression");
    };
    let CheckedClientExpression::Call {
        function: checked_target_id,
        arguments,
        location,
    } = expression
    else {
        panic!("ordinary CLIENT call did not lower to a call expression");
    };
    assert_eq!(
        *checked_target_id,
        super::CheckedFunctionId::Existing(target_id)
    );
    assert_eq!(
        arguments
            .iter()
            .map(|(parameter, _)| *parameter)
            .collect::<Vec<_>>(),
        vec![
            super::CheckedParameterId::Existing(target_first_parameter_id),
            super::CheckedParameterId::Existing(target_second_parameter_id),
        ]
    );
    assert!(matches!(
        &arguments[0].1,
        CheckedClientExpression::ParameterRead { parameter, .. }
            if *parameter == caller_parameter_id
    ));
    assert!(matches!(
        &arguments[1].1,
        CheckedClientExpression::Integer { value: 7, .. }
    ));

    let call_start = source.find("tasks.add").unwrap();
    let call_text = "tasks.add(p_first => p_input, p_second => 7)";
    assert_eq!(location.logical_path(), "client-call.orna");
    assert_eq!(location.span().start(), call_start);
    assert_eq!(location.span().end(), call_start + call_text.len());

    let call_references = function
        .references()
        .iter()
        .filter(|reference| reference.kind() == DefinitionReferenceKind::FunctionCall)
        .collect::<Vec<_>>();
    assert_eq!(call_references.len(), 1);
    let call_reference = call_references[0];
    assert_eq!(
        call_reference.target(),
        CheckedDefinitionReferenceTarget::Function(super::CheckedFunctionId::Existing(target_id,))
    );
    assert_eq!(call_reference.location().logical_path(), "client-call.orna");
    assert_eq!(call_reference.location().span().start(), call_start);
    assert_eq!(
        call_reference.location().span().end(),
        call_start + call_text.len()
    );
}

#[test]
fn orders_reversed_named_arguments_by_application_declaration() {
    let source = "CREATE SCHEMA app; \
            CREATE CLIENT FUNCTION app.target(p_first INTEGER, p_second INTEGER) RETURNS INTEGER AS p_first; \
            CREATE CLIENT FUNCTION app.call() RETURNS INTEGER AS app.target(p_second => 22, p_first => 11);";
    let report = check(
        &bundle([("client-call-reversed.orna", source)]),
        &empty_catalogue(),
    );
    assert_eq!(report.diagnostics(), &[], "{:?}", report.diagnostics());

    let checked = report.checked_bundle().unwrap();
    let target = checked
        .client_functions()
        .iter()
        .find(|function| function.name().to_string() == "app.target")
        .unwrap();
    let caller = checked
        .client_functions()
        .iter()
        .find(|function| function.name().to_string() == "app.call")
        .unwrap();
    let target_parameter_ids = target
        .parameters()
        .iter()
        .map(|parameter| parameter.id())
        .collect::<Vec<_>>();
    let CheckedClientFunctionBody::Expression { expression } = caller.body() else {
        panic!("application CLIENT call body was not an expression");
    };
    let CheckedClientExpression::Call { arguments, .. } = expression else {
        panic!("application CLIENT call did not lower to a call expression");
    };
    assert_eq!(
        arguments
            .iter()
            .map(|(parameter, _)| *parameter)
            .collect::<Vec<_>>(),
        target_parameter_ids
    );
    assert!(matches!(
        &arguments[0].1,
        CheckedClientExpression::Integer { value: 11, .. }
    ));
    assert!(matches!(
        &arguments[1].1,
        CheckedClientExpression::Integer { value: 22, .. }
    ));
}

#[test]
fn lowers_client_ref_field_path_concat_with_stable_identities_and_spans() {
    let source = "CREATE SCHEMA app; CREATE TYPE app.item AS OBJECT (title TEXT); CREATE CLIENT FUNCTION app.render(p_item REF app.item) RETURNS TEXT AS p_item.title || '!' || '?';";
    let report = check(
        &bundle([("client-field-concat.orna", source)]),
        &empty_catalogue(),
    );
    assert_eq!(report.diagnostics(), &[], "{:?}", report.diagnostics());

    let checked = report.checked_bundle().unwrap();
    let object = &checked.object_types()[0];
    let field_id = object.fields()[0].id();
    let function = checked.client_functions().iter().next().unwrap();
    let parameter_id = function.parameters()[0].id();
    let CheckedClientFunctionBody::Expression { expression } = function.body() else {
        panic!("CLIENT field concat body was not an expression");
    };
    let CheckedClientExpression::Concat {
        left,
        right,
        location,
    } = expression
    else {
        panic!("CLIENT field concat did not lower to a concat expression");
    };
    let CheckedClientExpression::Concat {
        left: first_field,
        right: first_string,
        location: first_location,
    } = left.as_ref()
    else {
        panic!("left-associative concat left operand was not a concat");
    };
    let CheckedClientExpression::FieldPath {
        root,
        fields,
        location: field_location,
    } = first_field.as_ref()
    else {
        panic!("left-associative concat root was not a field path");
    };
    assert_eq!(*root, parameter_id);
    assert_eq!(fields, &vec![field_id]);
    let CheckedClientExpression::String {
        value,
        location: string_location,
    } = first_string.as_ref()
    else {
        panic!("first concat right operand was not a string literal");
    };
    assert_eq!(value, "!");
    let CheckedClientExpression::String {
        value: final_value,
        location: final_string_location,
    } = right.as_ref()
    else {
        panic!("final concat operand was not a string literal");
    };
    assert_eq!(final_value, "?");

    let concat_start = source.find("p_item.title || '!' || '?'").unwrap();
    let field_start = source.find("p_item.title").unwrap();
    let string_start = source.find("'!'").unwrap();
    let final_string_start = source.find("'?' ".trim_end()).unwrap();
    assert_eq!(location.logical_path(), "client-field-concat.orna");
    assert_eq!(location.span().start(), concat_start);
    assert_eq!(location.span().end(), final_string_start + 3);
    assert_eq!(first_location.span().start(), field_start);
    assert_eq!(first_location.span().end(), string_start + 3);
    assert_eq!(field_location.span().start(), field_start);
    assert_eq!(
        field_location.span().end(),
        field_start + "p_item.title".len()
    );
    assert_eq!(string_location.span().start(), string_start);
    assert_eq!(string_location.span().end(), string_start + 3);
    assert_eq!(final_string_location.span().start(), final_string_start);
    assert_eq!(final_string_location.span().end(), final_string_start + 3);

    let non_text = "CREATE SCHEMA app; CREATE TYPE app.item AS OBJECT (title INTEGER); CREATE CLIENT FUNCTION app.render(p_item REF app.item) RETURNS INTEGER AS p_item.title || 1;";
    let rejected = check(
        &bundle([("client-field-concat-rejected.orna", non_text)]),
        &empty_catalogue(),
    );
    assert_eq!(rejected.diagnostics().len(), 1);
    assert_eq!(
        rejected.diagnostics()[0].code(),
        DiagnosticCode::TypeMismatch
    );
    let rejected_start = non_text.find("p_item.title || 1").unwrap();
    assert_eq!(
        rejected.diagnostics()[0].location().span().start(),
        rejected_start
    );
    assert_eq!(
        rejected.diagnostics()[0].location().span().end(),
        rejected_start + "p_item.title || 1".len()
    );
    assert!(rejected.checked_bundle().is_none());
}

#[test]
fn accepts_ordinary_inspector_structural_default_without_options() {
    let source = "CREATE SCHEMA devtools; CREATE CLIENT FUNCTION devtools.inspect(p_target REF sys.inspect.invocation) RETURNS sys.inspect.snapshot IS BEGIN RETURN sys.inspect.snapshot(p_target => p_target); END;";
    let report = check(&bundle([("inspector.orna", source)]), &empty_catalogue());
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let checked = report.checked_bundle().unwrap();
    let CheckedClientFunctionBody::Expression { expression } = checked.client_functions()[0].body()
    else {
        panic!("ordinary Inspector body was not an expression");
    };
    let CheckedClientExpression::Inspect { operation } = expression else {
        panic!("ordinary Inspector body was not an inspect operation");
    };
    assert!(matches!(
        operation,
        super::CheckedInspectOperation::Snapshot { options: None, .. }
    ));
}

#[test]
fn rejects_inspector_wrong_carrier_types_and_server_calls() {
    let cases = [
        "CREATE SCHEMA devtools; CREATE CLIENT FUNCTION devtools.bad(p_target REF sys.inspect.invocation, p_options sys.inspect.snapshot_options) RETURNS sys.inspect.snapshot IS BEGIN RETURN sys.inspect.snapshot(p_target => p_target, p_options => p_options); END;",
        "CREATE SCHEMA devtools; CREATE CLIENT FUNCTION devtools.bad(p_target REF sys.inspect.invocation, p_options sys.inspect.snapshot_options) RETURNS sys.inspect.resources IS BEGIN RETURN sys.inspect.calls(p_snapshot => sys.inspect.snapshot(p_target => p_target, p_options => p_options)); END;",
        "CREATE SCHEMA devtools; CREATE SERVER FUNCTION devtools.server() RETURNS ROWS (value TEXT) AS SELECT 'x'; CREATE CLIENT FUNCTION devtools.bad() RETURNS sys.inspect.snapshot IS BEGIN RETURN devtools.server(); END;",
    ];
    for source in cases {
        let report = check(&bundle([("inspector.orna", source)]), &empty_catalogue());
        assert!(
            !report.diagnostics().is_empty(),
            "source unexpectedly accepted: {source}"
        );
        assert_no_checked_bundle(&report);
    }

    let supplied_options = "CREATE SCHEMA devtools; CREATE CLIENT FUNCTION devtools.bad(p_target REF sys.inspect.invocation, p_options sys.inspect.snapshot_options) RETURNS sys.inspect.snapshot IS BEGIN RETURN sys.inspect.snapshot(p_target => p_target, p_options => p_options); END;";
    let report = check(
        &bundle([("inspector.orna", supplied_options)]),
        &empty_catalogue(),
    );
    assert_eq!(
        report
            .diagnostics()
            .iter()
            .map(|diagnostic| diagnostic.message())
            .collect::<Vec<_>>(),
        vec!["sys.inspect.snapshot options are not supported in Inspector v1"],
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn enforces_explicit_return_for_ui_expression_bodies() {
    let standard = check_standard_library_source(&verified_standard_v4_snapshot()).unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();

    let short = check_standard_application(
        &bundle([(
            "ui-return.orna",
            "CREATE SCHEMA app; CREATE EXTERNAL CLIENT FUNCTION app.factory() RETURNS std.UI RUNTIME CONTRACT 'app.factory@1'; CREATE CLIENT FUNCTION app.ui() RETURNS std.UI RETURN app.factory();",
        )]),
        &context,
    );
    assert!(short.diagnostics().is_empty(), "{:?}", short.diagnostics());
    assert!(short.checked_bundle().is_some());

    let as_ui = check_standard_application(
        &bundle([(
            "ui-as.orna",
            "CREATE SCHEMA app; CREATE EXTERNAL CLIENT FUNCTION app.factory() RETURNS std.UI RUNTIME CONTRACT 'app.factory@1'; CREATE CLIENT FUNCTION app.ui() RETURNS std.UI AS app.factory();",
        )]),
        &context,
    );
    assert_eq!(
        as_ui
            .diagnostics()
            .iter()
            .map(|diagnostic| diagnostic.message())
            .collect::<Vec<_>>(),
        vec!["CLIENT UI functions must use explicit RETURN instead of AS expression"],
    );
    assert!(as_ui.checked_bundle().is_none());

    let non_ui_as = check_standard_application(
        &bundle([(
            "non-ui-as.orna",
            "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.text() RETURNS INTEGER AS 1;",
        )]),
        &context,
    );
    assert!(
        non_ui_as.diagnostics().is_empty(),
        "{:?}",
        non_ui_as.diagnostics()
    );
}

#[test]
fn accepts_generic_inspect_render_contract_exact_signature() {
    let standard = check_standard_library_source(&verified_standard_v4_snapshot()).unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source = "CREATE SCHEMA app; CREATE EXTERNAL CLIENT FUNCTION app.inspector_renderer(
            p_snapshot sys.inspect.snapshot,
            p_invocation_nodes sys.inspect.invocation_nodes,
            p_calls sys.inspect.calls,
            p_resources sys.inspect.resources,
            p_state_cells sys.inspect.state_cells,
            p_ui_nodes sys.inspect.ui_nodes,
            p_presentation_candidates sys.inspect.presentation_candidates,
            p_runtime_bindings sys.inspect.runtime_bindings,
            p_security_decisions sys.inspect.security_decisions
        ) RETURNS std.ui.UI RUNTIME CONTRACT 'std.inspect.render@1';";
    let report = check_standard_application(&bundle([("inspect-render.orna", source)]), &context);
    assert_eq!(report.diagnostics(), &[], "{:?}", report.diagnostics());
    let function = report
        .checked_bundle()
        .unwrap()
        .client_functions()
        .next()
        .unwrap();
    assert_eq!(function.name().to_string(), "app.inspector_renderer");
    assert_eq!(function.parameters().count(), 9);
}

#[test]
fn rejects_historical_inspector_shell_contract_before_provider_dispatch() {
    let standard = check_standard_library_source(&verified_standard_v4_snapshot()).unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source = "CREATE SCHEMA app; CREATE EXTERNAL CLIENT FUNCTION app.inspector_renderer(
            p_snapshot sys.inspect.snapshot,
            p_invocation_nodes sys.inspect.invocation_nodes,
            p_calls sys.inspect.calls,
            p_resources sys.inspect.resources,
            p_state_cells sys.inspect.state_cells,
            p_ui_nodes sys.inspect.ui_nodes,
            p_presentation_candidates sys.inspect.presentation_candidates,
            p_runtime_bindings sys.inspect.runtime_bindings,
            p_security_decisions sys.inspect.security_decisions
        ) RETURNS std.ui.UI RUNTIME CONTRACT 'devtools.inspector_shell@1';";
    let report = check_standard_application(&bundle([("inspect-render.orna", source)]), &context);
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::UnknownQualifiedName
    );
    assert_eq!(
        report.diagnostics()[0].message(),
        "unregistered CLIENT external contract devtools.inspector_shell@1"
    );
    assert!(report.checked_bundle().is_none());
}

#[test]
fn accepts_procedural_inspector_with_pre_begin_value_locals() {
    let standard = check_standard_library_source(&verified_standard_v4_snapshot()).unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source = r#"CREATE SCHEMA inspector_app; CREATE SCHEMA app;
            CREATE EXTERNAL CLIENT FUNCTION app.inspector_renderer(
                p_snapshot sys.inspect.snapshot,
                p_invocation_nodes sys.inspect.invocation_nodes,
                p_calls sys.inspect.calls,
                p_resources sys.inspect.resources,
                p_state_cells sys.inspect.state_cells,
                p_ui_nodes sys.inspect.ui_nodes,
                p_presentation_candidates sys.inspect.presentation_candidates,
                p_runtime_bindings sys.inspect.runtime_bindings,
                p_security_decisions sys.inspect.security_decisions
            ) RETURNS std.ui.UI
            RUNTIME CONTRACT 'std.inspect.render@1';
            CREATE CLIENT FUNCTION inspector_app.inspector(p_target REF sys.inspect.invocation)
            RETURNS std.ui.UI IS
            LET snapshot sys.inspect.snapshot := sys.inspect.snapshot(p_target => p_target);
            LET invocation_nodes sys.inspect.invocation_nodes :=
                sys.inspect.invocation_nodes(p_snapshot => snapshot);
            LET calls sys.inspect.calls := sys.inspect.calls(p_snapshot => snapshot);
            LET resources sys.inspect.resources :=
                sys.inspect.resources(p_snapshot => snapshot);
            LET state_cells sys.inspect.state_cells :=
                sys.inspect.state_cells(p_snapshot => snapshot);
            LET ui_nodes sys.inspect.ui_nodes := sys.inspect.ui_nodes(p_snapshot => snapshot);
            LET presentation_candidates sys.inspect.presentation_candidates :=
                sys.inspect.presentation_candidates(p_snapshot => snapshot);
            LET runtime_bindings sys.inspect.runtime_bindings :=
                sys.inspect.runtime_bindings(p_snapshot => snapshot);
            LET security_decisions sys.inspect.security_decisions :=
                sys.inspect.security_decisions(p_snapshot => snapshot);
            BEGIN
                RETURN app.inspector_renderer(
                    p_snapshot => snapshot,
                    p_invocation_nodes => invocation_nodes,
                    p_calls => calls,
                    p_resources => resources,
                    p_state_cells => state_cells,
                    p_ui_nodes => ui_nodes,
                    p_presentation_candidates => presentation_candidates,
                    p_runtime_bindings => runtime_bindings,
                    p_security_decisions => security_decisions
                );
            END;"#;
    let report = check_standard_application(&bundle([("inspector.orna", source)]), &context);
    assert_eq!(report.diagnostics(), &[], "{:?}", report.diagnostics());
    let checked = report.preparation_view().unwrap().checked();
    let function = checked
        .client_functions()
        .iter()
        .find(|function| function.name().to_string() == "inspector_app.inspector")
        .expect("ordinary Inspector function");
    let CheckedClientFunctionBody::Procedural {
        locals,
        statements,
        return_expression,
    } = function.body()
    else {
        panic!("expected checked procedural Inspector body");
    };
    assert_eq!(locals.len(), 9);
    assert_eq!(statements.len(), 9);
    assert_eq!(
        locals
            .iter()
            .map(|local| local.ordinal())
            .collect::<Vec<_>>(),
        (0..9).collect::<Vec<_>>()
    );
    assert!(
        locals
            .iter()
            .all(|local| local.kind() == super::CheckedClientLocalKind::Value)
    );
    assert_eq!(
        statements
            .iter()
            .map(|statement| statement.local())
            .collect::<Vec<_>>(),
        (0..9).collect::<Vec<_>>()
    );
    assert!(matches!(
        statements[0].expression(),
        CheckedClientExpression::Inspect {
            operation: super::CheckedInspectOperation::Snapshot { target, .. }
        } if matches!(target.as_ref(), CheckedClientExpression::ParameterRead { .. })
    ));
    for statement in &statements[1..] {
        assert!(matches!(
            statement.expression(),
            CheckedClientExpression::Inspect {
                operation: super::CheckedInspectOperation::Projection { snapshot, .. }
            } if matches!(snapshot.as_ref(), CheckedClientExpression::LocalRead { local: 0, .. })
        ));
    }
    let CheckedClientExpression::Call { arguments, .. } = return_expression else {
        panic!("expected Inspector shell call");
    };
    assert_eq!(arguments.len(), 9);
    for (ordinal, (_, expression)) in arguments.iter().enumerate() {
        assert!(matches!(
            expression,
            CheckedClientExpression::LocalRead { local, .. }
                if *local == ordinal as u32
        ));
    }
}

#[test]
fn rejects_wrong_version_or_malformed_inspect_render_contracts() {
    let standard = check_standard_library_source(&verified_standard_v4_snapshot()).unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let cases = [
        "CREATE SCHEMA app; CREATE EXTERNAL CLIENT FUNCTION app.inspector_renderer(p_snapshot sys.inspect.snapshot, p_invocation_nodes sys.inspect.invocation_nodes, p_calls sys.inspect.calls, p_resources sys.inspect.resources, p_state_cells sys.inspect.state_cells, p_ui_nodes sys.inspect.ui_nodes, p_presentation_candidates sys.inspect.presentation_candidates, p_runtime_bindings sys.inspect.runtime_bindings) RETURNS std.ui.UI RUNTIME CONTRACT 'std.inspect.render@1';",
        "CREATE SCHEMA app; CREATE EXTERNAL CLIENT FUNCTION app.inspector_renderer(p_snapshot sys.inspect.snapshot, p_invocation_nodes sys.inspect.invocation_nodes, p_calls sys.inspect.resources, p_resources sys.inspect.resources, p_state_cells sys.inspect.state_cells, p_ui_nodes sys.inspect.ui_nodes, p_presentation_candidates sys.inspect.presentation_candidates, p_runtime_bindings sys.inspect.runtime_bindings, p_security_decisions sys.inspect.security_decisions) RETURNS std.ui.UI RUNTIME CONTRACT 'std.inspect.render@1';",
        "CREATE SCHEMA app; CREATE EXTERNAL CLIENT FUNCTION app.inspector_renderer(p_snapshot sys.inspect.snapshot, p_invocation_nodes sys.inspect.invocation_nodes, p_calls sys.inspect.calls, p_resources sys.inspect.resources, p_state_cells sys.inspect.state_cells, p_ui_nodes sys.inspect.ui_nodes, p_presentation_candidates sys.inspect.presentation_candidates, p_runtime_bindings sys.inspect.runtime_bindings, p_security_decisions sys.inspect.security_decisions) RETURNS BOOLEAN RUNTIME CONTRACT 'std.inspect.render@1';",
        "CREATE SCHEMA app; CREATE EXTERNAL CLIENT FUNCTION app.inspector_renderer(p_snapshot sys.inspect.snapshot, p_invocation_nodes sys.inspect.invocation_nodes, p_calls sys.inspect.calls, p_resources sys.inspect.resources, p_state_cells sys.inspect.state_cells, p_ui_nodes sys.inspect.ui_nodes, p_presentation_candidates sys.inspect.presentation_candidates, p_runtime_bindings sys.inspect.runtime_bindings, p_security_decisions sys.inspect.security_decisions) RETURNS std.ui.UI RUNTIME CONTRACT 'std.inspect.render@2';",
    ];
    for source in cases {
        let report =
            check_standard_application(&bundle([("inspect-render.orna", source)]), &context);
        assert!(
            !report.diagnostics().is_empty(),
            "source unexpectedly accepted: {source}"
        );
        assert!(report.checked_bundle().is_none());
    }
}

mod actions;
mod client_functions;
mod record_values;
mod resources;
mod schema_resolution;
mod server_functions;
mod standard_bundles;
mod standard_contracts;
mod standard_reconciliation;
mod type_evidence;

use actions::{
    active_from_prepared, assert_type_use_span, checked_use_index, empty_version_two_active,
    expression_use, result_use,
};
use client_functions::{STANDARD_V2_TYPES_SOURCE, STD_INVOKE_SOURCE};
use record_values::{
    opaque_standard_reconciliation_inputs, standard_origin, standard_reconciliation_inputs,
    verified_standard_library_for_relational_test,
    verified_standard_library_for_relational_test_with_boolean_id,
    verified_standard_library_with_action_for_test, verified_standard_library_with_opaque_for_test,
};
use standard_bundles::{
    STANDARD_V3_OUTPUT_SOURCE, STANDARD_V4_UI_SOURCE, check_v4_parts, standard_v2_executable,
    standard_v2_invoke_origins, standard_v2_types_origins, standard_v3_output_origins,
    standard_v4_catalogue, standard_v4_catalogue_with_ui_value_type, standard_v4_ui_origins,
    standard_v4_units, stored_v2_unit, verified_standard_v2_snapshot,
    verified_standard_v4_snapshot,
};
use standard_contracts::{check_echo, standard_parameter_echo_origins};
use standard_reconciliation::assert_no_checked_bundle;
use type_evidence::{
    assert_standard_source_mismatch, parsed_origin, parsed_standard_unit,
    rebase_standard_origins_to_source, two_type_reconciliation_inputs,
};
