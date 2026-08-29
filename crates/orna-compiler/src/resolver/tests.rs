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

#[test]
fn resolves_enum_labels_and_rejects_decoded_duplicates_before_a_checked_bundle() {
    let accepted = check(
        &bundle([(
            "types.orna",
            "CREATE SCHEMA crm; CREATE TYPE crm.stage AS ENUM ('lead', 'owner''s');",
        )]),
        &empty_catalogue(),
    );
    assert!(accepted.diagnostics().is_empty());
    let checked = accepted.checked_bundle().unwrap();
    let enum_types = checked.enum_types().collect::<Vec<_>>();
    assert_eq!(enum_types.len(), 1);
    assert_eq!(enum_types[0].1.to_string(), "crm.stage");
    assert_eq!(enum_types[0].2, &["lead", "owner's"]);

    let existing_id = TypeId::from_bytes([0x44; 16]);
    let base = CatalogueSnapshot::new_with_enum_types(
        CatalogueRevisionId::from_bytes([0x45; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x46; 16]),
            QualifiedSemanticName::new(["crm"]).unwrap(),
        )],
        vec![],
        vec![],
        vec![EnumTypeDefinition::new(
            existing_id,
            QualifiedSemanticName::new(["crm", "stage"]).unwrap(),
            ["lead"],
        )],
        vec![],
    )
    .unwrap();
    let changed = check(
        &bundle([(
            "types.orna",
            "CREATE SCHEMA crm; CREATE TYPE crm.stage AS ENUM ('lead', 'customer');",
        )]),
        &base,
    );
    assert_eq!(
        changed
            .checked_bundle()
            .unwrap()
            .enum_types()
            .next()
            .unwrap()
            .0,
        CheckedTypeId::Existing(existing_id)
    );

    let duplicate = check(
        &bundle([(
            "types.orna",
            "CREATE SCHEMA crm; CREATE TYPE crm.stage AS ENUM ('owner''s', 'owner''s');",
        )]),
        &empty_catalogue(),
    );
    assert!(duplicate.checked_bundle().is_none());
    assert_eq!(duplicate.diagnostics().len(), 1);
    assert_eq!(
        duplicate.diagnostics()[0].message(),
        "duplicate enum label \"owner's\" in crm.stage"
    );
}

#[test]
fn resolves_record_value_fields_through_the_closed_standard_and_enum_family() {
    let snapshot = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&snapshot).unwrap();
    let source = bundle([(
        "types.orna",
        "CREATE SCHEMA app;\nCREATE TYPE app.phase AS ENUM ('new', 'done');\nCREATE TYPE app.status AS VALUE (active BOOLEAN, phase app.phase) IMMUTABLE PERSISTABLE;",
    )]);
    let report = check_new_application(&source, &standard).unwrap();

    assert_eq!(report.diagnostics(), &[]);
    assert!(report.preparation_view().is_some());
    let checked = report.checked_bundle().unwrap();
    let records = checked.record_value_types().collect::<Vec<_>>();
    assert_eq!(records.len(), 1);
    let record = records[0];
    assert!(record.id().is_provisional());
    assert_eq!(record.name().to_string(), "app.status");
    let fields = record.fields().collect::<Vec<_>>();
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0].name(), "active");
    assert_eq!(fields[0].ordinal(), 0);
    assert_eq!(
        fields[0]
            .resolved_type()
            .value()
            .map(CheckedValueTypeUse::type_id),
        Some(TypeId::from_bytes([3; 16]))
    );
    assert_eq!(fields[1].name(), "phase");
    assert_eq!(fields[1].ordinal(), 1);
    assert!(
        fields[1]
            .resolved_type()
            .named_type()
            .is_some_and(CheckedTypeId::is_provisional)
    );
    assert!(fields.iter().all(|field| field.id().is_provisional()));
    assert_eq!(checked.uses().len(), 2);
}

#[test]
fn checked_bundle_preserves_object_enum_and_record_value_categories_together() {
    let snapshot = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&snapshot).unwrap();
    let source = bundle([(
        "categories.orna",
        "CREATE SCHEMA app;\n\
                CREATE TYPE app.phase AS ENUM ('new', 'done');\n\
                CREATE TYPE app.status AS VALUE (phase app.phase) IMMUTABLE PERSISTABLE;\n\
                CREATE TYPE app.item AS OBJECT (status app.status NOT NULL, phase app.phase NOT NULL);",
    )]);
    let report = check_new_application(&source, &standard).unwrap();

    assert_eq!(report.diagnostics(), &[]);
    let checked = report.checked_bundle().unwrap();
    let object = checked.object_types().next().unwrap();
    assert_eq!(checked.object_types().count(), 1);
    assert_eq!(object.name().to_string(), "app.item");

    let enum_types = checked.inner.enum_types().collect::<Vec<_>>();
    assert_eq!(enum_types.len(), 1);
    let (enum_id, enum_name, labels, _) = enum_types[0];
    assert_eq!(enum_name.to_string(), "app.phase");
    assert_eq!(labels, &["new".to_owned(), "done".to_owned()]);

    let record = checked.record_value_types().next().unwrap();
    assert_eq!(checked.record_value_types().count(), 1);
    assert_eq!(record.name().to_string(), "app.status");
    assert_ne!(object.id(), enum_id);
    assert_ne!(object.id(), record.id());
    assert_ne!(enum_id, record.id());

    let object_fields = object.fields().collect::<Vec<_>>();
    assert_eq!(object_fields.len(), 2);
    assert_eq!(
        object_fields[0].resolved_type().named_type(),
        Some(record.id())
    );
    assert_eq!(object_fields[1].resolved_type().named_type(), Some(enum_id));
}

#[test]
fn checks_record_constructor_identities_in_declaration_order_and_prepares_artifact() {
    let verified = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_version_two_active(&verified);
    let source = "CREATE SCHEMA app;\n\
            CREATE TYPE app.flags AS VALUE (active BOOLEAN, visible BOOLEAN) IMMUTABLE PERSISTABLE;\n\
            CREATE TYPE app.item AS OBJECT (flags app.flags NOT NULL);\n\
            CREATE SERVER FUNCTION app.create(p_visible BOOLEAN)\n\
            RETURNS ROWS (item REF app.item) SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
            AS INSERT INTO app.item AS made (flags)\n\
            VALUES (app.flags{visible: p_visible, active: TRUE}) RETURNING REF(made);";
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let report = check_standard_application(&bundle([("constructor.orna", source)]), &context);

    assert_eq!(report.diagnostics(), &[]);
    let checked = report.checked_bundle().unwrap();
    let preparation = report.preparation_view().unwrap();
    let raw = preparation.checked();
    let record = &raw.record_value_types()[0];
    let record_fields = record.fields();
    let object = &raw.object_types()[0];
    let function = &raw.server_functions()[0];
    let plan = function.mutation_plan().unwrap();
    let MutationExpressionKind::RecordConstructor {
        record_type,
        fields,
    } = plan.assignments()[0].expression().kind()
    else {
        panic!("checked INSERT value must be a record constructor");
    };
    assert_eq!(*record_type, record.id());
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0].owner(), record.id());
    assert_eq!(fields[0].field(), record_fields[0].id());
    assert!(matches!(
        fields[0].kind(),
        MutationRecordFieldExpressionKind::BooleanLiteral { value: true }
    ));
    assert_eq!(fields[1].field(), record_fields[1].id());
    assert!(matches!(
        fields[1].kind(),
        MutationRecordFieldExpressionKind::ParameterRead { parameter, .. }
            if *parameter == function.parameters()[0].id()
    ));
    assert_eq!(
        function
            .references()
            .iter()
            .map(|reference| (reference.kind(), reference.target()))
            .collect::<Vec<_>>(),
        vec![
            (
                DefinitionReferenceKind::ObjectReference,
                CheckedDefinitionReferenceTarget::ObjectType(object.id()),
            ),
            (
                DefinitionReferenceKind::WriteObject,
                CheckedDefinitionReferenceTarget::ObjectType(object.id()),
            ),
            (
                DefinitionReferenceKind::WriteField,
                CheckedDefinitionReferenceTarget::Field {
                    owner: object.id(),
                    field: object.fields()[0].id(),
                },
            ),
            (
                DefinitionReferenceKind::NamedType,
                CheckedDefinitionReferenceTarget::ValueType(record.id()),
            ),
            (
                DefinitionReferenceKind::WriteField,
                CheckedDefinitionReferenceTarget::Field {
                    owner: record.id(),
                    field: record_fields[0].id(),
                },
            ),
            (
                DefinitionReferenceKind::WriteField,
                CheckedDefinitionReferenceTarget::Field {
                    owner: record.id(),
                    field: record_fields[1].id(),
                },
            ),
            (
                DefinitionReferenceKind::ParameterRead,
                CheckedDefinitionReferenceTarget::Parameter {
                    owner: function.id(),
                    parameter: function.parameters()[0].id(),
                },
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                CheckedDefinitionReferenceTarget::ObjectType(object.id()),
            ),
        ]
    );
    let expression_ordinals = checked
        .uses()
        .iter()
        .filter_map(|type_use| match type_use.kind() {
            CheckedTypeUseKind::Expression { ordinal, .. } => Some(ordinal),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(expression_ordinals, vec![2, 1, 3]);
    let constructor_value_uses = checked
        .uses()
        .iter()
        .filter_map(|type_use| {
            let value = type_use.value()?;
            let CheckedTypeUseKind::Expression { ordinal, .. } = value.kind() else {
                return None;
            };
            Some((
                ordinal,
                value.type_id(),
                value.location().span().start(),
                value.location().span().end(),
            ))
        })
        .collect::<Vec<_>>();
    let parameter_start = source.rfind("p_visible").unwrap();
    let literal_start = source.rfind("TRUE").unwrap();
    assert_eq!(
        constructor_value_uses,
        vec![
            (
                2,
                TypeId::from_bytes([3; 16]),
                parameter_start,
                parameter_start + "p_visible".len(),
            ),
            (
                1,
                TypeId::from_bytes([3; 16]),
                literal_start,
                literal_start + "TRUE".len(),
            ),
        ]
    );

    let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();
    let candidate = prepared.candidate();
    let durable_record = candidate
        .record_value_type_by_name(&QualifiedSemanticName::new(["app", "flags"]).unwrap())
        .unwrap();
    let durable_object = candidate
        .object_type_by_name(&QualifiedSemanticName::new(["app", "item"]).unwrap())
        .unwrap();
    let revision = &prepared.new_function_revisions()[0];
    assert_eq!(revision.artifact().version(), RECORD_INSERT_FORMAT_VERSION);
    let artifact = ServerMutationPlan::decode(revision.artifact().payload()).unwrap();
    assert_eq!(artifact.target(), durable_object.id());
    assert_eq!(artifact.assignments().len(), 1);
    assert_eq!(artifact.assignments()[0].owner(), durable_object.id());
    assert_eq!(
        artifact.assignments()[0].field(),
        durable_object.fields()[0].id()
    );
    let ServerMutationExpressionKind::RecordConstructor { fields } =
        artifact.assignments()[0].expression().kind()
    else {
        panic!("prepared INSERT value must be a record constructor");
    };
    assert_eq!(
        artifact.assignments()[0].expression().resolved_type(),
        ResolvedType::named(durable_record.id())
    );
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0].owner(), durable_record.id());
    assert_eq!(fields[0].field(), durable_record.fields()[0].id());
    assert!(matches!(
        fields[0].kind(),
        ServerRecordFieldExpressionKind::BooleanLiteral { value: true }
    ));
    assert_eq!(fields[1].owner(), durable_record.id());
    assert_eq!(fields[1].field(), durable_record.fields()[1].id());
    assert!(matches!(
        fields[1].kind(),
        ServerRecordFieldExpressionKind::Parameter { owner, parameter }
            if *owner == candidate.functions()[0].id()
                && *parameter == candidate.functions()[0].parameters()[0].id()
    ));
}

#[test]
fn record_constructor_source_order_does_not_change_checked_plan() {
    let standard =
        check_standard_library_source(&verified_standard_library_for_relational_test()).unwrap();
    let source = |fields: &str| {
        format!(
            "CREATE SCHEMA app;\n\
                 CREATE TYPE app.flags AS VALUE (active BOOLEAN, visible BOOLEAN) IMMUTABLE PERSISTABLE;\n\
                 CREATE TYPE app.item AS OBJECT (flags app.flags NOT NULL);\n\
                 CREATE SERVER FUNCTION app.create(p_visible BOOLEAN)\n\
                 RETURNS ROWS (item REF app.item) TRANSACTION ATOMIC\n\
                 AS INSERT INTO app.item AS made (flags) VALUES (app.flags{{{fields}}}) RETURNING REF(made);"
        )
    };
    let first_bundle = SourceBundle::new([SourceUnit::new(
        "first.orna",
        source("active: TRUE, visible: p_visible"),
    )])
    .unwrap();
    let second_bundle = SourceBundle::new([SourceUnit::new(
        "second.orna",
        source("visible: p_visible, active: TRUE"),
    )])
    .unwrap();
    let first = check_new_application(&first_bundle, &standard).unwrap();
    let second = check_new_application(&second_bundle, &standard).unwrap();

    assert_eq!(first.diagnostics(), &[]);
    assert_eq!(second.diagnostics(), &[]);
    assert_eq!(
        first
            .preparation_view()
            .unwrap()
            .checked()
            .server_functions()[0]
            .mutation_plan(),
        second
            .preparation_view()
            .unwrap()
            .checked()
            .server_functions()[0]
            .mutation_plan()
    );
}

#[test]
fn record_constructor_accepts_an_exact_active_enum_parameter() {
    let verified = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_version_two_active(&verified);
    let source = "CREATE SCHEMA app;\n\
            CREATE TYPE app.phase AS ENUM ('new', 'done');\n\
            CREATE TYPE app.status AS VALUE (phase app.phase) IMMUTABLE PERSISTABLE;\n\
            CREATE TYPE app.item AS OBJECT (status app.status NOT NULL);\n\
            CREATE SERVER FUNCTION app.create(p_phase app.phase) RETURNS ROWS (item REF app.item)\n\
            TRANSACTION ATOMIC AS INSERT INTO app.item AS made (status)\n\
            VALUES (app.status{phase: p_phase}) RETURNING REF(made);";
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let report = check_standard_application(&bundle([("enum_constructor.orna", source)]), &context);

    assert_eq!(report.diagnostics(), &[]);
    let raw = report.preparation_view().unwrap();
    let checked = raw.checked();
    let enum_type = checked.enum_types().next().unwrap().0;
    let plan = checked.server_functions()[0].mutation_plan().unwrap();
    let MutationExpressionKind::RecordConstructor { fields, .. } =
        plan.assignments()[0].expression().kind()
    else {
        panic!("checked INSERT value must be a record constructor");
    };
    assert_eq!(
        fields[0].value_type().semantic_type(),
        SemanticType::Named(enum_type)
    );

    let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();
    let candidate = prepared.candidate();
    let durable_enum = candidate
        .enum_type_by_name(&QualifiedSemanticName::new(["app", "phase"]).unwrap())
        .unwrap();
    let revision = &prepared.new_function_revisions()[0];
    let artifact = ServerMutationPlan::decode(revision.artifact().payload()).unwrap();
    let ServerMutationExpressionKind::RecordConstructor { fields } =
        artifact.assignments()[0].expression().kind()
    else {
        panic!("prepared INSERT value must be a record constructor");
    };
    assert_eq!(
        fields[0].resolved_type(),
        ResolvedType::named(durable_enum.id())
    );
    assert!(matches!(
        fields[0].kind(),
        ServerRecordFieldExpressionKind::Parameter { owner, parameter }
            if *owner == candidate.functions()[0].id()
                && *parameter == candidate.functions()[0].parameters()[0].id()
    ));
}

#[test]
fn record_constructor_rejects_scalar_values_for_enum_fields() {
    let standard =
        check_standard_library_source(&verified_standard_library_for_relational_test()).unwrap();
    for (value, expected) in [
        (
            "p_active",
            "parameter p_active cannot initialise record field phase because their types do not match",
        ),
        (
            "TRUE",
            "record field phase is not BOOLEAN, so it cannot accept TRUE or FALSE",
        ),
    ] {
        let source = format!(
            "CREATE SCHEMA app;\n\
                 CREATE TYPE app.phase AS ENUM ('new', 'done');\n\
                 CREATE TYPE app.status AS VALUE (phase app.phase) IMMUTABLE PERSISTABLE;\n\
                 CREATE TYPE app.item AS OBJECT (status app.status NOT NULL);\n\
                 CREATE SERVER FUNCTION app.create(p_active BOOLEAN) RETURNS ROWS (item REF app.item)\n\
                 TRANSACTION ATOMIC AS INSERT INTO app.item AS made (status)\n\
                 VALUES (app.status{{phase: {value}}}) RETURNING REF(made);"
        );
        let value_start = source.rfind(value).unwrap();
        let source_bundle =
            SourceBundle::new([SourceUnit::new("enum_mismatch.orna", source)]).unwrap();
        let report = check_new_application(&source_bundle, &standard).unwrap();

        assert_eq!(report.diagnostics().len(), 1);
        assert_eq!(report.diagnostics()[0].message(), expected);
        assert_eq!(
            report.diagnostics()[0].location().span().start(),
            value_start
        );
        assert_eq!(
            report.diagnostics()[0].location().span().end(),
            value_start + value.len()
        );
        assert!(report.checked_bundle().is_none());
    }
}

#[test]
fn record_constructor_rejects_a_record_typed_parameter_for_a_nested_child() {
    let standard =
        check_standard_library_source(&verified_standard_library_for_relational_test()).unwrap();
    let source = "CREATE SCHEMA app;\n\
            CREATE TYPE app.inner AS VALUE (flag BOOLEAN) IMMUTABLE PERSISTABLE;\n\
            CREATE TYPE app.outer AS VALUE (child app.inner) IMMUTABLE PERSISTABLE;\n\
            CREATE TYPE app.item AS OBJECT (outer app.outer NOT NULL);\n\
            CREATE SERVER FUNCTION app.create(p_inner app.inner) RETURNS ROWS (item REF app.item)\n\
            TRANSACTION ATOMIC AS INSERT INTO app.item AS made (outer)\n\
            VALUES (app.outer{child: p_inner}) RETURNING REF(made);";
    let source_bundle =
        SourceBundle::new([SourceUnit::new("nested_constructor.orna", source)]).unwrap();
    let report = check_new_application(&source_bundle, &standard).unwrap();

    assert_eq!(report.diagnostics().len(), 1);
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(diagnostic.code(), DiagnosticCode::DomainIncompatible);
    assert_eq!(
        diagnostic.message(),
        "INSERT does not yet support the type of parameter p_inner; supported types are BOOLEAN, INTEGER, BIGINT, FLOAT, CHARACTER LARGE OBJECT, BINARY LARGE OBJECT, and REF"
    );
    let value_start = source.find("p_inner").unwrap();
    assert_eq!(diagnostic.location().span().start(), value_start);
    assert_eq!(
        diagnostic.location().span().end(),
        value_start + "p_inner".len()
    );
    assert_eq!(
        &source[value_start..value_start + "p_inner".len()],
        "p_inner"
    );
    assert!(report.checked_bundle().is_none());
}

#[test]
fn record_constructor_semantics_reject_incomplete_or_incompatible_values() {
    let standard =
        check_standard_library_source(&verified_standard_library_for_relational_test()).unwrap();
    for (object_field, parameter, constructor, expected) in [
        (
            "flags app.flags NOT NULL",
            "p_visible BOOLEAN",
            "app.flags{active: TRUE}",
            "record field visible is required, but this constructor does not provide it",
        ),
        (
            "flags app.flags NOT NULL",
            "p_visible BOOLEAN",
            "app.flags{active: TRUE, visible: p_visible, extra: TRUE}",
            "record value type app.flags has no field named extra",
        ),
        (
            "flags app.flags",
            "p_visible BOOLEAN",
            "app.flags{active: TRUE, visible: p_visible}",
            "record constructor app.flags requires a non-null field of that exact record type, but field flags does not match",
        ),
        (
            "flags app.flags NOT NULL",
            "p_visible BOOLEAN",
            "app.missing{active: TRUE, visible: p_visible}",
            "unknown record value type app.missing",
        ),
        (
            "flags app.flags NOT NULL",
            "p_visible BOOLEAN",
            "app.other{active: TRUE, visible: p_visible}",
            "record constructor app.other requires a non-null field of that exact record type, but field flags does not match",
        ),
        (
            "flags app.flags NOT NULL",
            "p_flags app.flags",
            "app.flags{active: TRUE, visible: p_flags}",
            "INSERT does not yet support the type of parameter p_flags",
        ),
    ] {
        let source = format!(
            "CREATE SCHEMA app;\n\
                 CREATE TYPE app.flags AS VALUE (active BOOLEAN, visible BOOLEAN) IMMUTABLE PERSISTABLE;\n\
                 CREATE TYPE app.other AS VALUE (active BOOLEAN, visible BOOLEAN) IMMUTABLE PERSISTABLE;\n\
                 CREATE TYPE app.item AS OBJECT ({object_field});\n\
                 CREATE SERVER FUNCTION app.create({parameter}) RETURNS ROWS (item REF app.item)\n\
                 TRANSACTION ATOMIC AS INSERT INTO app.item AS made (flags)\n\
                 VALUES ({constructor}) RETURNING REF(made);"
        );
        let source_bundle =
            SourceBundle::new([SourceUnit::new("invalid_constructor.orna", source)]).unwrap();
        let report = check_new_application(&source_bundle, &standard).unwrap();

        assert!(
            report
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.message().contains(expected)),
            "expected {expected:?}, got {:?}",
            report.diagnostics()
        );
        assert!(report.checked_bundle().is_none());
    }
}

#[test]
fn prepares_and_replays_record_value_identities_with_exact_evidence() {
    let verified = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&verified).unwrap();
    let initial = empty_version_two_active(&verified);
    let source = "CREATE SCHEMA app;\nCREATE TYPE app.phase AS ENUM ('new', 'done');\nCREATE TYPE app.status AS VALUE (active BOOLEAN, phase app.phase) IMMUTABLE PERSISTABLE;";
    let bundle = bundle([("records.orna", source)]);
    let context = StandardApplicationCheckContext::try_new(initial.catalogue(), &standard).unwrap();
    let report = check_standard_application(&bundle, &context);
    assert_eq!(report.diagnostics(), &[]);

    let checked = report.checked_bundle().unwrap();
    let checked_record = checked.record_value_types().next().unwrap();
    let checked_fields = checked_record.fields().collect::<Vec<_>>();
    let prepared = prepare_standard_application(&report, initial.pair(), &initial).unwrap();
    let record = &prepared.candidate().record_value_types()[0];
    let enum_type = &prepared.candidate().enum_types()[0];
    assert_eq!(record.name().to_string(), "app.status");
    assert_eq!(record.fields().len(), 2);
    assert_eq!(
        record.fields()[0].descriptor(),
        &orna_core::types::TypeDescriptor::named(TypeId::from_bytes([3; 16]))
    );
    assert_eq!(
        record.fields()[1].descriptor(),
        &orna_core::types::TypeDescriptor::named(enum_type.id())
    );
    let unit = prepared.source().units()[0].id();
    assert!(prepared.origins().iter().any(|origin| {
        origin.identity() == DefinitionIdentity::ValueType(record.id())
            && origin.source()
                == SourceOrigin::new(
                    unit,
                    u32::try_from(checked_record.location().span().start()).unwrap(),
                    u32::try_from(checked_record.location().span().end()).unwrap(),
                )
                .unwrap()
    }));
    for (checked_field, field) in checked_fields.iter().zip(record.fields()) {
        assert!(prepared.origins().iter().any(|origin| {
            origin.identity()
                == DefinitionIdentity::Field {
                    owner: record.id(),
                    field: field.id(),
                }
                && origin.source()
                    == SourceOrigin::new(
                        unit,
                        u32::try_from(checked_field.location().span().start()).unwrap(),
                        u32::try_from(checked_field.location().span().end()).unwrap(),
                    )
                    .unwrap()
        }));
    }

    let record_id = record.id();
    let field_ids = record
        .fields()
        .iter()
        .map(|field| field.id())
        .collect::<Vec<_>>();
    let active = active_from_prepared(&prepared);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let replay_report = check_standard_application(&bundle, &context);
    assert_eq!(replay_report.diagnostics(), &[]);
    let replay = prepare_standard_application(&replay_report, active.pair(), &active).unwrap();
    let replay_record = &replay.candidate().record_value_types()[0];
    assert_eq!(replay_record.id(), record_id);
    assert_eq!(
        replay_record
            .fields()
            .iter()
            .map(|field| field.id())
            .collect::<Vec<_>>(),
        field_ids
    );

    let mut hostile = report.clone();
    let boolean_index = hostile
        .checked_bundle()
        .unwrap()
        .uses()
        .iter()
        .position(|type_use| type_use.value().is_some())
        .unwrap();
    assert!(hostile.replace_value_type_id_for_test(boolean_index, TypeId::from_bytes([0xef; 16]),));
    assert!(matches!(
        prepare_standard_application(&hostile, initial.pair(), &initial),
        Err(
            PrepareStandardApplicationError::DeclarationTypeEvidenceMismatch {
                kind: CheckedTypeUseKind::Field { .. },
            }
        )
    ));
}

#[test]
fn record_value_preparation_rejects_every_deferred_existing_shape_change() {
    let verified = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&verified).unwrap();
    let initial = empty_version_two_active(&verified);
    let original = "CREATE SCHEMA app; CREATE TYPE app.phase AS ENUM ('new', 'done'); CREATE TYPE app.status AS VALUE (active BOOLEAN, phase app.phase) IMMUTABLE PERSISTABLE;";
    let original_bundle = bundle([("records.orna", original)]);
    let context = StandardApplicationCheckContext::try_new(initial.catalogue(), &standard).unwrap();
    let report = check_standard_application(&original_bundle, &context);
    let prepared = prepare_standard_application(&report, initial.pair(), &initial).unwrap();
    let active = active_from_prepared(&prepared);

    for (source, reason) in [
        (
            "CREATE SCHEMA app; CREATE TYPE app.phase AS ENUM ('new', 'done'); CREATE TYPE app.status AS VALUE (active BOOLEAN, phase app.phase, extra BOOLEAN) IMMUTABLE PERSISTABLE;",
            "record value field addition or removal is not supported",
        ),
        (
            "CREATE SCHEMA app; CREATE TYPE app.phase AS ENUM ('new', 'done'); CREATE TYPE app.status AS VALUE (active BOOLEAN) IMMUTABLE PERSISTABLE;",
            "record value field addition or removal is not supported",
        ),
        (
            "CREATE SCHEMA app; CREATE TYPE app.phase AS ENUM ('new', 'done'); CREATE TYPE app.status AS VALUE (phase app.phase, active BOOLEAN) IMMUTABLE PERSISTABLE;",
            "record value field reordering is not supported",
        ),
        (
            "CREATE SCHEMA app; CREATE TYPE app.phase AS ENUM ('new', 'done'); CREATE TYPE app.status AS VALUE (active app.phase, phase app.phase) IMMUTABLE PERSISTABLE;",
            "record value field type change is not supported",
        ),
        (
            "CREATE SCHEMA app; CREATE TYPE app.phase AS ENUM ('new', 'done'); CREATE TYPE app.status AS VALUE (enabled BOOLEAN, phase app.phase) IMMUTABLE PERSISTABLE;",
            "record value field replacement is not supported",
        ),
        (
            "CREATE SCHEMA app; CREATE TYPE app.phase AS ENUM ('new', 'done'); CREATE TYPE app.state AS VALUE (active BOOLEAN, phase app.phase) IMMUTABLE PERSISTABLE;",
            "existing record value type is absent from the candidate catalogue",
        ),
        (
            "CREATE SCHEMA app; CREATE TYPE app.phase AS ENUM ('new', 'done');",
            "existing record value type is absent from the candidate catalogue",
        ),
    ] {
        let context =
            StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
        let report = check_standard_application(&bundle([("records.orna", source)]), &context);
        assert_eq!(report.diagnostics(), &[], "{source}");
        assert!(matches!(
            prepare_standard_application(&report, active.pair(), &active),
            Err(PrepareStandardApplicationError::Prepare {
                source: PrepareError::InvalidCheckedBundle { reason: actual },
            }) if actual == reason
        ));
    }
}

#[test]
fn record_value_resolution_rejects_legacy_nested_object_and_duplicate_shapes() {
    let source = bundle([(
        "types.orna",
        "CREATE SCHEMA app; CREATE TYPE app.status AS VALUE (active BOOLEAN) IMMUTABLE PERSISTABLE;",
    )]);
    let legacy = check(&source, &empty_catalogue());
    assert_eq!(legacy.diagnostics().len(), 1);
    assert_eq!(
        legacy.diagnostics()[0].code(),
        DiagnosticCode::DomainIncompatible
    );
    assert_eq!(
        legacy.diagnostics()[0].message(),
        "record value types require checked standard-library authority"
    );
    assert!(legacy.checked_bundle().is_none());

    let snapshot = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&snapshot).unwrap();
    let invalid = bundle([(
        "invalid.orna",
        "CREATE SCHEMA app;\nCREATE TYPE app.object AS OBJECT ();\nCREATE TYPE app.first AS VALUE (duplicate BOOLEAN, duplicate BOOLEAN) IMMUTABLE PERSISTABLE;\nCREATE TYPE app.second AS VALUE (nested app.first, object app.object) IMMUTABLE PERSISTABLE;",
    )]);
    let report = check_new_application(&invalid, &standard).unwrap();
    assert_eq!(
        report
            .diagnostics()
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>(),
        vec![
            (
                DiagnosticCode::DuplicateDefinition,
                "duplicate record field definition duplicate in app.first",
            ),
            (
                DiagnosticCode::TypeMismatch,
                "object type app.object must be declared with REF",
            ),
        ]
    );
    assert!(report.checked_bundle().is_none());

    let collision = bundle([(
        "collision.orna",
        "CREATE SCHEMA app; CREATE TYPE app.same AS ENUM ('x'); CREATE TYPE app.same AS VALUE (active BOOLEAN) IMMUTABLE PERSISTABLE;",
    )]);
    let report = check_new_application(&collision, &standard).unwrap();
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::DuplicateDefinition
    );
    assert_eq!(
        report.diagnostics()[0].message(),
        "duplicate record value type definition app.same"
    );

    let decimal_standard = checked_standard_library_with_contract_overrides_for_test(
        &snapshot,
        &[(0, "orna.kernel.value.decimal@1")],
    )
    .unwrap();
    let unsupported = check_new_application(&source, &decimal_standard).unwrap();
    assert_eq!(unsupported.diagnostics().len(), 1);
    assert_eq!(
        unsupported.diagnostics()[0].code(),
        DiagnosticCode::TypeMismatch
    );
    assert_eq!(
        unsupported.diagnostics()[0].message(),
        "record value field uses a type outside the initial record family"
    );

    let mut transient_standard = check_standard_library_source(&snapshot).unwrap();
    transient_standard
        .value_types
        .iter_mut()
        .find(|value_type| value_type.representation_contract == "orna.kernel.value.boolean@1")
        .unwrap()
        .persistence = ValueTypePersistence::Transient;
    let unsupported = check_new_application(&source, &transient_standard).unwrap();
    assert_eq!(unsupported.diagnostics().len(), 1);
    assert_eq!(
        unsupported.diagnostics()[0].code(),
        DiagnosticCode::TypeMismatch
    );
    assert_eq!(
        unsupported.diagnostics()[0].message(),
        "record value field uses a type outside the initial record family"
    );
}

#[test]
fn record_value_self_cycle_rejects_with_exact_orna0201_evidence() {
    let verified = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&verified).unwrap();
    let source =
        "CREATE SCHEMA app; CREATE TYPE app.loop AS VALUE (next app.loop) IMMUTABLE PERSISTABLE;";
    let report = check_new_application(&bundle([("cycle.orna", source)]), &standard).unwrap();
    assert_eq!(report.diagnostics().len(), 1);
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(diagnostic.code(), DiagnosticCode::TypeMismatch);
    assert_eq!(diagnostic.code().as_str(), "ORNA0201");
    assert_eq!(
        diagnostic.message(),
        "record value fields must not form a recursive cycle through app.loop"
    );
    let start = source.find("AS VALUE (next ").unwrap() + "AS VALUE (next ".len();
    assert_eq!(diagnostic.location().span().start(), start);
    assert_eq!(diagnostic.location().span().end(), start + "app.loop".len());
    assert_eq!(&source[start..start + "app.loop".len()], "app.loop");
    assert!(report.checked_bundle().is_none());
}

#[test]
fn record_value_multi_record_cycle_reports_the_exact_closing_edge() {
    let verified = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&verified).unwrap();
    let source = "CREATE SCHEMA app;\nCREATE TYPE app.a AS VALUE (left app.b) IMMUTABLE PERSISTABLE;\nCREATE TYPE app.b AS VALUE (right app.a) IMMUTABLE PERSISTABLE;";
    let report = check_new_application(&bundle([("cycle.orna", source)]), &standard).unwrap();
    assert_eq!(report.diagnostics().len(), 1);
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(diagnostic.code(), DiagnosticCode::TypeMismatch);
    assert_eq!(diagnostic.code().as_str(), "ORNA0201");
    assert_eq!(
        diagnostic.message(),
        "record value fields must not form a recursive cycle through app.a"
    );
    let start = source.find("right app.a").unwrap() + "right ".len();
    assert_eq!(diagnostic.location().span().start(), start);
    assert_eq!(diagnostic.location().span().end(), start + "app.a".len());
    assert_eq!(&source[start..start + "app.a".len()], "app.a");
    assert!(report.checked_bundle().is_none());
}

#[test]
fn record_value_cycle_phase_precedes_depth_validation() {
    let verified = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&verified).unwrap();
    let mut source = String::from("CREATE SCHEMA app;\n");
    for index in 0..=32 {
        source.push_str(&format!(
            "CREATE TYPE app.d{index} AS VALUE (next app.d{}) IMMUTABLE PERSISTABLE;\n",
            index + 1
        ));
    }
    source.push_str("CREATE TYPE app.d33 AS VALUE (flag BOOLEAN) IMMUTABLE PERSISTABLE;\n");
    source.push_str(
            "CREATE TYPE app.c1 AS VALUE (next app.c2) IMMUTABLE PERSISTABLE;\nCREATE TYPE app.c2 AS VALUE (next app.c1) IMMUTABLE PERSISTABLE;\n",
        );
    let bundle = SourceBundle::new([SourceUnit::new("cycle.orna", source.clone())]).unwrap();
    let report = check_new_application(&bundle, &standard).unwrap();
    assert_eq!(report.diagnostics().len(), 1);
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(diagnostic.code(), DiagnosticCode::TypeMismatch);
    assert_eq!(diagnostic.code().as_str(), "ORNA0201");
    assert_eq!(
        diagnostic.message(),
        "record value fields must not form a recursive cycle through app.c1"
    );
    let start = source.find("next app.c1").unwrap() + "next ".len();
    assert_eq!(diagnostic.location().span().start(), start);
    assert_eq!(diagnostic.location().span().end(), start + "app.c1".len());
    assert_eq!(&source[start..start + "app.c1".len()], "app.c1");
    assert!(report.checked_bundle().is_none());
}

#[test]
fn record_value_depth_thirty_two_chain_is_accepted_and_prepared() {
    let verified = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&verified).unwrap();
    let mut source = String::from("CREATE SCHEMA app;\n");
    for index in 0..32 {
        source.push_str(&format!(
            "CREATE TYPE app.r{index} AS VALUE (next app.r{}) IMMUTABLE PERSISTABLE;\n",
            index + 1
        ));
    }
    source.push_str("CREATE TYPE app.r32 AS VALUE (flag BOOLEAN) IMMUTABLE PERSISTABLE;\n");
    let bundle = SourceBundle::new([SourceUnit::new("chain.orna", source.clone())]).unwrap();
    let initial = empty_version_two_active(&verified);
    let context = StandardApplicationCheckContext::try_new(initial.catalogue(), &standard).unwrap();
    let report = check_standard_application(&bundle, &context);
    assert_eq!(report.diagnostics(), &[]);
    let prepared = prepare_standard_application(&report, initial.pair(), &initial).unwrap();
    let records = prepared.candidate().record_value_types();
    assert_eq!(records.len(), 33);
    let first = records
        .iter()
        .find(|record| record.name().to_string() == "app.r0")
        .unwrap();
    let second = records
        .iter()
        .find(|record| record.name().to_string() == "app.r1")
        .unwrap();
    let last = records
        .iter()
        .find(|record| record.name().to_string() == "app.r31")
        .unwrap();
    let leaf = records
        .iter()
        .find(|record| record.name().to_string() == "app.r32")
        .unwrap();
    assert_eq!(
        first.fields()[0].descriptor(),
        &orna_core::types::TypeDescriptor::named(second.id())
    );
    assert_eq!(
        last.fields()[0].descriptor(),
        &orna_core::types::TypeDescriptor::named(leaf.id())
    );
}

#[test]
fn record_value_depth_thirty_three_chain_rejects_the_r32_edge_exactly() {
    let verified = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&verified).unwrap();
    let mut source = String::from("CREATE SCHEMA app;\n");
    for index in 0..=32 {
        source.push_str(&format!(
            "CREATE TYPE app.r{index} AS VALUE (next app.r{}) IMMUTABLE PERSISTABLE;\n",
            index + 1
        ));
    }
    source.push_str("CREATE TYPE app.r33 AS VALUE (flag BOOLEAN) IMMUTABLE PERSISTABLE;\n");
    let bundle = SourceBundle::new([SourceUnit::new("chain.orna", source.clone())]).unwrap();
    let report = check_new_application(&bundle, &standard).unwrap();
    assert_eq!(report.diagnostics().len(), 1);
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(diagnostic.code(), DiagnosticCode::TypeMismatch);
    assert_eq!(diagnostic.code().as_str(), "ORNA0201");
    assert_eq!(
        diagnostic.message(),
        "record value nesting exceeds 32 levels through app.r33"
    );
    let start = source.find("next app.r33").unwrap() + "next ".len();
    assert_eq!(diagnostic.location().span().start(), start);
    assert_eq!(diagnostic.location().span().end(), start + "app.r33".len());
    assert_eq!(&source[start..start + "app.r33".len()], "app.r33");
    assert!(report.checked_bundle().is_none());
}

#[test]
fn record_value_shared_acyclic_dag_is_accepted_and_prepared() {
    let verified = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&verified).unwrap();
    let source = "CREATE SCHEMA app;\nCREATE TYPE app.d AS VALUE (flag BOOLEAN) IMMUTABLE PERSISTABLE;\nCREATE TYPE app.b AS VALUE (next app.d) IMMUTABLE PERSISTABLE;\nCREATE TYPE app.c AS VALUE (next app.d) IMMUTABLE PERSISTABLE;\nCREATE TYPE app.a AS VALUE (left app.b, right app.c) IMMUTABLE PERSISTABLE;";
    let bundle = bundle([("dag.orna", source)]);
    let initial = empty_version_two_active(&verified);
    let context = StandardApplicationCheckContext::try_new(initial.catalogue(), &standard).unwrap();
    let report = check_standard_application(&bundle, &context);
    assert_eq!(report.diagnostics(), &[]);
    let prepared = prepare_standard_application(&report, initial.pair(), &initial).unwrap();
    let records = prepared.candidate().record_value_types();
    assert_eq!(records.len(), 4);
    let a = records
        .iter()
        .find(|record| record.name().to_string() == "app.a")
        .unwrap();
    let b = records
        .iter()
        .find(|record| record.name().to_string() == "app.b")
        .unwrap();
    let c = records
        .iter()
        .find(|record| record.name().to_string() == "app.c")
        .unwrap();
    let d = records
        .iter()
        .find(|record| record.name().to_string() == "app.d")
        .unwrap();
    assert_eq!(a.fields().len(), 2);
    assert_eq!(
        a.fields()[0].descriptor(),
        &orna_core::types::TypeDescriptor::named(b.id())
    );
    assert_eq!(
        a.fields()[1].descriptor(),
        &orna_core::types::TypeDescriptor::named(c.id())
    );
    assert_eq!(
        b.fields()[0].descriptor(),
        &orna_core::types::TypeDescriptor::named(d.id())
    );
    assert_eq!(
        c.fields()[0].descriptor(),
        &orna_core::types::TypeDescriptor::named(d.id())
    );
}

#[test]
fn record_value_enum_named_field_remains_accepted_and_never_forms_a_graph_edge() {
    let verified = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&verified).unwrap();
    let source = "CREATE SCHEMA app;\nCREATE TYPE app.phase AS ENUM ('new', 'done');\nCREATE TYPE app.status AS VALUE (phase app.phase) IMMUTABLE PERSISTABLE;";
    let bundle = bundle([("enum_field.orna", source)]);
    let initial = empty_version_two_active(&verified);
    let context = StandardApplicationCheckContext::try_new(initial.catalogue(), &standard).unwrap();
    let report = check_standard_application(&bundle, &context);
    assert_eq!(report.diagnostics(), &[]);
    let prepared = prepare_standard_application(&report, initial.pair(), &initial).unwrap();
    let records = prepared.candidate().record_value_types();
    assert_eq!(records.len(), 1);
    let status = &records[0];
    assert_eq!(status.name().to_string(), "app.status");
    assert_eq!(status.fields().len(), 1);
    let phase = prepared
        .candidate()
        .enum_types()
        .iter()
        .find(|enum_type| enum_type.name().to_string() == "app.phase")
        .unwrap();
    assert_eq!(
        status.fields()[0].descriptor(),
        &orna_core::types::TypeDescriptor::named(phase.id())
    );
}

#[test]
fn record_value_cycle_selection_follows_source_and_field_order() {
    let verified = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&verified).unwrap();
    let z_source = "CREATE SCHEMA app;\nCREATE TYPE app.z1 AS VALUE (first app.z2, second app.z3) IMMUTABLE PERSISTABLE;\nCREATE TYPE app.z2 AS VALUE (back app.z1) IMMUTABLE PERSISTABLE;\nCREATE TYPE app.z3 AS VALUE (back app.z1) IMMUTABLE PERSISTABLE;\n";
    let a_source = "CREATE TYPE app.a1 AS VALUE (next app.a2) IMMUTABLE PERSISTABLE;\nCREATE TYPE app.a2 AS VALUE (back app.a1) IMMUTABLE PERSISTABLE;\n";
    let bundle = SourceBundle::new([
        SourceUnit::new("z.orna", z_source),
        SourceUnit::new("a.orna", a_source),
    ])
    .unwrap();
    let initial = empty_version_two_active(&verified);
    let context = StandardApplicationCheckContext::try_new(initial.catalogue(), &standard).unwrap();
    let report = check_standard_application(&bundle, &context);
    assert_eq!(report.diagnostics().len(), 1);
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(
        diagnostic.code(),
        DiagnosticCode::TypeMismatch,
        "{}",
        diagnostic.message()
    );
    assert_eq!(diagnostic.code().as_str(), "ORNA0201");
    assert_eq!(
        diagnostic.message(),
        "record value fields must not form a recursive cycle through app.z1"
    );
    assert_eq!(diagnostic.location().logical_path(), "z.orna");
    let start = z_source.find("back app.z1").unwrap() + "back ".len();
    assert_eq!(diagnostic.location().span().start(), start);
    assert_eq!(diagnostic.location().span().end(), start + "app.z1".len());
    assert_eq!(&z_source[start..start + "app.z1".len()], "app.z1");
    assert!(report.checked_bundle().is_none());
}

#[test]
fn record_value_depth_validation_revisits_a_shallow_cached_suffix() {
    let verified = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&verified).unwrap();
    let mut source = String::from(
        "CREATE SCHEMA app;\nCREATE TYPE app.x0 AS VALUE (next app.x1) IMMUTABLE PERSISTABLE;\nCREATE TYPE app.x1 AS VALUE (next app.s0) IMMUTABLE PERSISTABLE;\nCREATE TYPE app.s0 AS VALUE (next app.s1) IMMUTABLE PERSISTABLE;\nCREATE TYPE app.s1 AS VALUE (next app.s2) IMMUTABLE PERSISTABLE;\nCREATE TYPE app.s2 AS VALUE (next app.s3) IMMUTABLE PERSISTABLE;\nCREATE TYPE app.s3 AS VALUE (flag BOOLEAN) IMMUTABLE PERSISTABLE;\n",
    );
    for index in 0..29 {
        source.push_str(&format!(
            "CREATE TYPE app.y{index} AS VALUE (next app.y{}) IMMUTABLE PERSISTABLE;\n",
            index + 1
        ));
    }
    source.push_str("CREATE TYPE app.y29 AS VALUE (next app.s0) IMMUTABLE PERSISTABLE;\n");
    let bundle = SourceBundle::new([SourceUnit::new("depth.orna", source.clone())]).unwrap();
    let report = check_new_application(&bundle, &standard).unwrap();
    assert_eq!(report.diagnostics().len(), 1);
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(diagnostic.code(), DiagnosticCode::TypeMismatch);
    assert_eq!(diagnostic.code().as_str(), "ORNA0201");
    assert_eq!(
        diagnostic.message(),
        "record value nesting exceeds 32 levels through app.s3"
    );
    let start = source.find("next app.s3").unwrap() + "next ".len();
    assert_eq!(diagnostic.location().span().start(), start);
    assert_eq!(diagnostic.location().span().end(), start + "app.s3".len());
    assert_eq!(&source[start..start + "app.s3".len()], "app.s3");
    assert!(report.checked_bundle().is_none());
}

#[test]
fn record_value_resolution_binds_object_fields_and_server_rows_to_one_identity() {
    let snapshot = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&snapshot).unwrap();
    let source = bundle([(
        "records.orna",
        "CREATE SCHEMA app; \
             CREATE TYPE app.status AS VALUE (active BOOLEAN) IMMUTABLE PERSISTABLE; \
             CREATE TYPE app.task AS OBJECT (status app.status NOT NULL); \
             CREATE SERVER FUNCTION app.read() RETURNS ROWS (status app.status) \
             TRANSACTION READ ONLY VOLATILITY STABLE \
             AS SELECT task.status FROM app.task task;",
    )]);

    let report = check_new_application(&source, &standard).unwrap();

    assert_eq!(report.diagnostics(), &[]);
    let checked = report.checked_bundle().unwrap();
    let record_type = checked.record_value_types().next().unwrap().id();
    assert_eq!(
        checked
            .object_types()
            .next()
            .unwrap()
            .fields()
            .next()
            .unwrap()
            .resolved_type()
            .named_type(),
        Some(record_type)
    );
    assert_eq!(
        checked
            .server_functions()
            .next()
            .unwrap()
            .return_columns()
            .next()
            .unwrap()
            .resolved_type()
            .named_type(),
        Some(record_type)
    );
}

#[test]
fn record_value_scalar_family_is_exact() {
    for scalar in [
        StandardScalar::Boolean,
        StandardScalar::Integer,
        StandardScalar::BigInt,
        StandardScalar::Float,
        StandardScalar::CharacterLargeObject,
        StandardScalar::BinaryLargeObject,
    ] {
        assert!(supports_record_value_scalar(scalar));
    }
    for scalar in [
        StandardScalar::Decimal,
        StandardScalar::Uuid,
        StandardScalar::Date,
        StandardScalar::Time,
        StandardScalar::Timestamp,
        StandardScalar::Duration,
        StandardScalar::Void,
    ] {
        assert!(!supports_record_value_scalar(scalar));
    }
}

#[test]
fn enum_and_object_declarations_share_one_resolved_type_namespace() {
    let report = check(
        &bundle([(
            "types.orna",
            "CREATE SCHEMA crm; CREATE TYPE crm.stage AS OBJECT (); CREATE TYPE crm.stage AS ENUM ('lead');",
        )]),
        &empty_catalogue(),
    );

    assert!(report.checked_bundle().is_none());
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].message(),
        "duplicate enum type definition crm.stage"
    );
}

#[test]
fn resolves_application_enum_uses_as_named_values_and_rejects_ref() {
    let standard =
        check_standard_library_source(&verified_standard_library_for_relational_test()).unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source = "CREATE SCHEMA crm; CREATE TYPE crm.stage AS ENUM ('lead', 'qualified'); \
            CREATE TYPE crm.customer AS OBJECT (stage crm.stage NOT NULL);";
    let report = check_standard_application(&bundle([("types.orna", source)]), &context);

    assert_eq!(report.diagnostics(), &[]);
    let checked = report.checked_bundle().unwrap();
    let enum_type = checked.inner.enum_types().next().unwrap().0;
    let object = checked.object_types().next().unwrap();
    let field = object.fields().next().unwrap();
    assert_eq!(field.resolved_type().named_type(), Some(enum_type));
    assert!(field.resolved_type().value().is_none());
    assert!(field.resolved_type().object_reference().is_none());
    let type_start = source.rfind("crm.stage").unwrap();
    assert_type_use_span(field.resolved_type(), type_start, "crm.stage");

    let rejected = check_standard_application(
        &bundle([(
            "types.orna",
            "CREATE SCHEMA crm; CREATE TYPE crm.stage AS ENUM ('lead'); \
                 CREATE TYPE crm.customer AS OBJECT (stage REF crm.stage);",
        )]),
        &context,
    );
    assert!(rejected.checked_bundle().is_none());
    assert_eq!(rejected.diagnostics().len(), 1);
    assert_eq!(
        rejected.diagnostics()[0].code(),
        DiagnosticCode::InvalidReferenceTarget
    );
    assert_eq!(
        rejected.diagnostics()[0].message(),
        "REF target crm.stage is an enum type"
    );
}

fn standard_reconciliation_inputs(
    source: &str,
) -> (
    StoredSourceUnit,
    ParsedSourceUnit,
    CatalogueSnapshot,
    Vec<DefinitionOrigin>,
) {
    let stored_unit = StoredSourceUnit::new(
        STANDARD_SOURCE_UNIT_ID,
        0,
        "std/types.orna",
        source,
        source_unit_content_digest(source).unwrap(),
    )
    .unwrap();
    let report =
        parse_bundle(&SourceBundle::new([SourceUnit::new("std/types.orna", source)]).unwrap());
    assert!(report.diagnostics().is_empty());
    let parsed_unit = report.units()[0].clone();

    let boolean = ValueTypeDefinition::primitive(
        TypeId::from_bytes([3; 16]),
        QualifiedSemanticName::new(["std", "types", "boolean"]).unwrap(),
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Persistable,
        "orna.kernel.value.boolean@1",
    );
    let qualified = TypeBinding::qualified(
        QualifiedSemanticName::new(["std", "boolean"]).unwrap(),
        boolean.id(),
    )
    .unwrap();
    let prelude =
        TypeBinding::prelude(PreludeTypeName::new(["boolean"]).unwrap(), boolean.id()).unwrap();
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([8; 16]),
        vec![
            SchemaDefinition::new(
                SchemaId::from_bytes([1; 16]),
                QualifiedSemanticName::new(["std"]).unwrap(),
            ),
            SchemaDefinition::new(
                SchemaId::from_bytes([2; 16]),
                QualifiedSemanticName::new(["std", "types"]).unwrap(),
            ),
        ],
        vec![],
        vec![boolean],
        vec![qualified.clone(), prelude.clone()],
    )
    .unwrap();
    let origins = vec![
        standard_origin(
            DefinitionIdentity::Schema(SchemaId::from_bytes([1; 16])),
            0,
            18,
        ),
        standard_origin(
            DefinitionIdentity::Schema(SchemaId::from_bytes([2; 16])),
            18,
            42,
        ),
        standard_origin(
            DefinitionIdentity::ValueType(TypeId::from_bytes([3; 16])),
            42,
            159,
        ),
        standard_origin(DefinitionIdentity::TypeBinding(qualified.id()), 159, 204),
        standard_origin(DefinitionIdentity::TypeBinding(prelude.id()), 204, 250),
    ];

    (stored_unit, parsed_unit, catalogue, origins)
}

fn opaque_standard_reconciliation_inputs(
    source: &str,
    name: QualifiedSemanticName,
    contract: &str,
) -> (
    StoredSourceUnit,
    ParsedSourceUnit,
    CatalogueSnapshot,
    Vec<DefinitionOrigin>,
) {
    let stored_unit = StoredSourceUnit::new(
        STANDARD_SOURCE_UNIT_ID,
        0,
        "std/types.orna",
        source,
        source_unit_content_digest(source).unwrap(),
    )
    .unwrap();
    let parsed_unit = parsed_standard_unit(source);
    let opaque = ValueTypeDefinition::opaque(TypeId::from_bytes([3; 16]), name, contract);
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([8; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([1; 16]),
            QualifiedSemanticName::new(["std"]).unwrap(),
        )],
        vec![],
        vec![opaque],
        vec![],
    )
    .unwrap();
    let origins = vec![
        parsed_origin(
            DefinitionIdentity::Schema(SchemaId::from_bytes([1; 16])),
            &parsed_unit.parsed().schemas()[0].span,
        ),
        parsed_origin(
            DefinitionIdentity::ValueType(TypeId::from_bytes([3; 16])),
            &parsed_unit.parsed().opaque_value_types()[0].span,
        ),
    ];
    (stored_unit, parsed_unit, catalogue, origins)
}

fn standard_origin(
    identity: DefinitionIdentity,
    byte_start: u32,
    byte_end: u32,
) -> DefinitionOrigin {
    DefinitionOrigin::new(
        identity,
        SourceOrigin::new(STANDARD_SOURCE_UNIT_ID, byte_start, byte_end).unwrap(),
    )
}

#[test]
fn accepts_nested_record_value_fields_with_provisional_and_durable_evidence() {
    let verified = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&verified).unwrap();
    let initial = empty_version_two_active(&verified);
    let source = "CREATE SCHEMA app;\nCREATE TYPE app.outer AS VALUE (inner app.inner) IMMUTABLE PERSISTABLE;\nCREATE TYPE app.inner AS VALUE (active BOOLEAN) IMMUTABLE PERSISTABLE;";
    let bundle = bundle([("nested.orna", source)]);
    let context = StandardApplicationCheckContext::try_new(initial.catalogue(), &standard).unwrap();
    let report = check_standard_application(&bundle, &context);
    assert_eq!(report.diagnostics(), &[]);

    let checked = report.checked_bundle().unwrap();
    let records = checked.record_value_types().collect::<Vec<_>>();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].name().to_string(), "app.outer");
    assert_eq!(records[1].name().to_string(), "app.inner");
    let outer = records[0];
    let inner = records[1];
    let CheckedTypeId::Provisional(_) = outer.id() else {
        panic!("outer record must be provisional at check time");
    };
    let CheckedTypeId::Provisional(_) = inner.id() else {
        panic!("inner record must be provisional at check time");
    };

    let outer_fields = outer.fields().collect::<Vec<_>>();
    assert_eq!(outer_fields.len(), 1);
    assert_eq!(outer_fields[0].name(), "inner");
    let type_use = outer_fields[0].resolved_type();
    let CheckedTypeUseKind::Field { owner, field } = type_use.kind() else {
        panic!("outer field must carry Field type-use evidence");
    };
    assert_eq!(owner, outer.id());
    assert_eq!(field, outer_fields[0].id());
    assert_eq!(type_use.named_type(), Some(inner.id()));
    let span = type_use.location().span();
    assert_eq!(&source[span.start()..span.end()], "app.inner");

    let prepared = prepare_standard_application(&report, initial.pair(), &initial).unwrap();
    let candidate = prepared.candidate();
    let durable_outer = candidate
        .record_value_types()
        .iter()
        .find(|record| record.name().to_string() == "app.outer")
        .unwrap();
    let durable_inner = candidate
        .record_value_types()
        .iter()
        .find(|record| record.name().to_string() == "app.inner")
        .unwrap();
    assert_eq!(
        durable_outer.fields()[0].descriptor(),
        &orna_core::types::TypeDescriptor::named(durable_inner.id())
    );
    let unit = prepared.source().units()[0].id();
    assert!(prepared.origins().iter().any(|origin| {
        origin.identity()
            == DefinitionIdentity::Field {
                owner: durable_outer.id(),
                field: durable_outer.fields()[0].id(),
            }
            && origin.source()
                == SourceOrigin::new(
                    unit,
                    u32::try_from(outer_fields[0].location().span().start()).unwrap(),
                    u32::try_from(outer_fields[0].location().span().end()).unwrap(),
                )
                .unwrap()
    }));
    assert!(prepared.origins().iter().any(|origin| {
        origin.identity() == DefinitionIdentity::ValueType(durable_inner.id())
            && origin.source()
                == SourceOrigin::new(
                    unit,
                    u32::try_from(inner.location().span().start()).unwrap(),
                    u32::try_from(inner.location().span().end()).unwrap(),
                )
                .unwrap()
    }));

    let active = active_from_prepared(&prepared);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let replay_report = check_standard_application(&bundle, &context);
    assert_eq!(replay_report.diagnostics(), &[]);
    let replay_checked = replay_report.checked_bundle().unwrap();
    let replay_records = replay_checked.record_value_types().collect::<Vec<_>>();
    assert_eq!(
        replay_records[0].id(),
        CheckedTypeId::Existing(durable_outer.id())
    );
    assert_eq!(
        replay_records[1].id(),
        CheckedTypeId::Existing(durable_inner.id())
    );
    let replay = prepare_standard_application(&replay_report, active.pair(), &active).unwrap();
    let replay_candidate = replay.candidate();
    assert_eq!(
        replay_candidate.record_value_types()[0].id(),
        durable_outer.id()
    );
    assert_eq!(
        replay_candidate.record_value_types()[1].id(),
        durable_inner.id()
    );
    assert_eq!(
        replay_candidate.record_value_types()[0].fields()[0].descriptor(),
        &orna_core::types::TypeDescriptor::named(durable_inner.id())
    );
}

fn verified_standard_library_for_relational_test()
-> orna_core::revision::VerifiedStandardLibrarySnapshot {
    const DIGEST: [u8; 32] = [
        0x72, 0x4b, 0x41, 0xcf, 0x68, 0x5c, 0x93, 0xa8, 0xc9, 0x8d, 0xf9, 0x3d, 0x96, 0x77, 0x98,
        0x98, 0x12, 0x34, 0xc0, 0x98, 0xf6, 0xc1, 0x00, 0xfa, 0x57, 0xe9, 0xac, 0x00, 0xdd, 0x03,
        0xfb, 0x6d,
    ];
    verified_standard_library_for_relational_test_with_boolean_id(
        TypeId::from_bytes([3; 16]),
        DIGEST,
    )
}

fn verified_standard_library_for_relational_test_with_boolean_id(
    boolean_id: TypeId,
    digest: [u8; 32],
) -> orna_core::revision::VerifiedStandardLibrarySnapshot {
    let source_unit = StoredSourceUnit::new(
        STANDARD_SOURCE_UNIT_ID,
        0,
        "std/types.orna",
        STANDARD_SOURCE,
        source_unit_content_digest(STANDARD_SOURCE).unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
    let source = StoredSourceRevision::new(
        SourceBundleId::from_bytes([5; 16]),
        SourceRevisionId::from_bytes([6; 16]),
        None,
        vec![source_unit],
        bundle_hash,
        source_revision_record_digest(SourceBundleId::from_bytes([5; 16]), None, bundle_hash)
            .unwrap(),
    )
    .unwrap();
    let boolean = ValueTypeDefinition::primitive(
        boolean_id,
        QualifiedSemanticName::new(["std", "types", "boolean"]).unwrap(),
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Persistable,
        "orna.kernel.value.boolean@1",
    );
    let qualified = TypeBinding::qualified(
        QualifiedSemanticName::new(["std", "boolean"]).unwrap(),
        boolean.id(),
    )
    .unwrap();
    let prelude =
        TypeBinding::prelude(PreludeTypeName::new(["boolean"]).unwrap(), boolean.id()).unwrap();
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([8; 16]),
        vec![
            SchemaDefinition::new(
                SchemaId::from_bytes([1; 16]),
                QualifiedSemanticName::new(["std"]).unwrap(),
            ),
            SchemaDefinition::new(
                SchemaId::from_bytes([2; 16]),
                QualifiedSemanticName::new(["std", "types"]).unwrap(),
            ),
        ],
        vec![],
        vec![boolean],
        vec![qualified.clone(), prelude.clone()],
    )
    .unwrap();
    let snapshot = StandardLibrarySnapshot::new(
        StandardLibraryRevisionId::from_bytes([7; 16]),
        StandardLibraryDigestVersion::Version1,
        source,
        "orna.language/1",
        catalogue,
        vec![
            standard_origin(
                DefinitionIdentity::Schema(SchemaId::from_bytes([1; 16])),
                0,
                18,
            ),
            standard_origin(
                DefinitionIdentity::Schema(SchemaId::from_bytes([2; 16])),
                18,
                42,
            ),
            standard_origin(DefinitionIdentity::ValueType(boolean_id), 42, 159),
            standard_origin(DefinitionIdentity::TypeBinding(qualified.id()), 159, 204),
            standard_origin(DefinitionIdentity::TypeBinding(prelude.id()), 204, 250),
        ],
        Sha256Digest::from_bytes(digest),
    )
    .unwrap();

    let digest = calculate_standard_library_digest(&snapshot).unwrap();
    let snapshot = StandardLibrarySnapshot::new(
        snapshot.revision(),
        snapshot.digest_version(),
        snapshot.source().clone(),
        snapshot.language_version(),
        snapshot.catalogue().clone(),
        snapshot.origins().to_vec(),
        digest,
    )
    .unwrap();
    verify_standard_library_snapshot(snapshot).unwrap()
}

fn verified_standard_library_with_opaque_for_test()
-> orna_core::revision::VerifiedStandardLibrarySnapshot {
    const SOURCE: &str = "CREATE SCHEMA std;CREATE TYPE std.BOOLEAN AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.boolean@1' IMMUTABLE PERSISTABLE;CREATE TYPE std.TOKEN AS VALUE OPAQUE KERNEL CONTRACT 'std.token@1' IMMUTABLE TRANSIENT;";
    let parsed = parsed_standard_unit(SOURCE);
    let source_unit = StoredSourceUnit::new(
        STANDARD_SOURCE_UNIT_ID,
        0,
        "std/types.orna",
        SOURCE,
        source_unit_content_digest(SOURCE).unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
    let source = StoredSourceRevision::new(
        SourceBundleId::from_bytes([0x32; 16]),
        SourceRevisionId::from_bytes([0x33; 16]),
        None,
        vec![source_unit],
        bundle_hash,
        source_revision_record_digest(SourceBundleId::from_bytes([0x32; 16]), None, bundle_hash)
            .unwrap(),
    )
    .unwrap();
    let boolean = ValueTypeDefinition::primitive(
        TypeId::from_bytes([0x34; 16]),
        QualifiedSemanticName::new(["std", "boolean"]).unwrap(),
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Persistable,
        "orna.kernel.value.boolean@1",
    );
    let opaque = ValueTypeDefinition::opaque(
        TypeId::from_bytes([0x35; 16]),
        QualifiedSemanticName::new(["std", "token"]).unwrap(),
        "std.token@1",
    );
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([0x36; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x37; 16]),
            QualifiedSemanticName::new(["std"]).unwrap(),
        )],
        vec![],
        vec![boolean, opaque],
        vec![],
    )
    .unwrap();
    let source_unit = STANDARD_SOURCE_UNIT_ID;
    let origins = vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(SchemaId::from_bytes([0x37; 16])),
            SourceOrigin::new(
                source_unit,
                parsed.parsed().schemas()[0].span.start as u32,
                parsed.parsed().schemas()[0].span.end as u32,
            )
            .unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::ValueType(TypeId::from_bytes([0x34; 16])),
            SourceOrigin::new(
                source_unit,
                parsed.parsed().primitive_value_types()[0].span.start as u32,
                parsed.parsed().primitive_value_types()[0].span.end as u32,
            )
            .unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::ValueType(TypeId::from_bytes([0x35; 16])),
            SourceOrigin::new(
                source_unit,
                parsed.parsed().opaque_value_types()[0].span.start as u32,
                parsed.parsed().opaque_value_types()[0].span.end as u32,
            )
            .unwrap(),
        ),
    ];
    let snapshot = StandardLibrarySnapshot::new(
        StandardLibraryRevisionId::from_bytes([0x38; 16]),
        StandardLibraryDigestVersion::Version1,
        source,
        "orna.language/1",
        catalogue,
        origins,
        Sha256Digest::from_bytes([0; 32]),
    )
    .unwrap();
    let digest = calculate_standard_library_digest(&snapshot).unwrap();
    verify_standard_library_snapshot(
        StandardLibrarySnapshot::new(
            snapshot.revision(),
            snapshot.digest_version(),
            snapshot.source().clone(),
            snapshot.language_version(),
            snapshot.catalogue().clone(),
            snapshot.origins().to_vec(),
            digest,
        )
        .unwrap(),
    )
    .unwrap()
}

fn verified_standard_library_with_action_for_test()
-> orna_core::revision::VerifiedStandardLibrarySnapshot {
    const SOURCE: &str = "CREATE SCHEMA std;CREATE SCHEMA std.types;CREATE SCHEMA std.action;CREATE TYPE std.types.INTEGER AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.integer@1' IMMUTABLE PERSISTABLE;CREATE TYPE std.action.Action AS VALUE OPAQUE KERNEL CONTRACT 'orna.std.value.action@1' IMMUTABLE TRANSIENT;EXPORT TYPE std.types.INTEGER AS std.INTEGER;EXPORT TYPE std.action.Action AS std.Action;EXPORT TYPE std.INTEGER TO PRELUDE AS INTEGER;";
    let parsed = parsed_standard_unit(SOURCE);
    let source_unit_id = STANDARD_SOURCE_UNIT_ID;
    let source_unit = StoredSourceUnit::new(
        source_unit_id,
        0,
        "std/types.orna",
        SOURCE,
        source_unit_content_digest(SOURCE).unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
    let source = StoredSourceRevision::new(
        SourceBundleId::from_bytes([0x42; 16]),
        SourceRevisionId::from_bytes([0x43; 16]),
        None,
        vec![source_unit],
        bundle_hash,
        source_revision_record_digest(SourceBundleId::from_bytes([0x42; 16]), None, bundle_hash)
            .unwrap(),
    )
    .unwrap();
    let integer = ValueTypeDefinition::primitive(
        TypeId::from_bytes([0x48; 16]),
        QualifiedSemanticName::new(["std", "types", "integer"]).unwrap(),
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Persistable,
        "orna.kernel.value.integer@1",
    );
    let integer_id = integer.id();
    let action = ValueTypeDefinition::opaque(
        STD_ACTION_TYPE_ID,
        QualifiedSemanticName::new(["std", "action", "action"]).unwrap(),
        STD_ACTION_CONTRACT,
    );
    let integer_binding = TypeBinding::qualified(
        QualifiedSemanticName::new(["std", "integer"]).unwrap(),
        integer_id,
    )
    .unwrap();
    let action_binding = TypeBinding::qualified(
        QualifiedSemanticName::new(["std", "action"]).unwrap(),
        action.id(),
    )
    .unwrap();
    let integer_prelude =
        TypeBinding::prelude(PreludeTypeName::new(["integer"]).unwrap(), integer_id).unwrap();
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([0x44; 16]),
        vec![
            SchemaDefinition::new(
                SchemaId::from_bytes([0x45; 16]),
                QualifiedSemanticName::new(["std"]).unwrap(),
            ),
            SchemaDefinition::new(
                SchemaId::from_bytes([0x46; 16]),
                QualifiedSemanticName::new(["std", "types"]).unwrap(),
            ),
            SchemaDefinition::new(
                SchemaId::from_bytes([0x49; 16]),
                QualifiedSemanticName::new(["std", "action"]).unwrap(),
            ),
        ],
        vec![],
        vec![integer, action],
        vec![
            integer_binding.clone(),
            action_binding.clone(),
            integer_prelude.clone(),
        ],
    )
    .unwrap();
    let action_origin = |identity, byte_start, byte_end| {
        DefinitionOrigin::new(
            identity,
            SourceOrigin::new(source_unit_id, byte_start, byte_end).unwrap(),
        )
    };
    let origins = vec![
        action_origin(
            DefinitionIdentity::Schema(SchemaId::from_bytes([0x45; 16])),
            parsed.parsed().schemas()[0].span.start as u32,
            parsed.parsed().schemas()[0].span.end as u32,
        ),
        action_origin(
            DefinitionIdentity::Schema(SchemaId::from_bytes([0x46; 16])),
            parsed.parsed().schemas()[1].span.start as u32,
            parsed.parsed().schemas()[1].span.end as u32,
        ),
        action_origin(
            DefinitionIdentity::Schema(SchemaId::from_bytes([0x49; 16])),
            parsed.parsed().schemas()[2].span.start as u32,
            parsed.parsed().schemas()[2].span.end as u32,
        ),
        action_origin(
            DefinitionIdentity::ValueType(integer_id),
            parsed.parsed().primitive_value_types()[0].span.start as u32,
            parsed.parsed().primitive_value_types()[0].span.end as u32,
        ),
        action_origin(
            DefinitionIdentity::ValueType(STD_ACTION_TYPE_ID),
            parsed.parsed().opaque_value_types()[0].span.start as u32,
            parsed.parsed().opaque_value_types()[0].span.end as u32,
        ),
        action_origin(
            DefinitionIdentity::TypeBinding(integer_binding.id()),
            parsed.parsed().type_exports()[0].span.start as u32,
            parsed.parsed().type_exports()[0].span.end as u32,
        ),
        action_origin(
            DefinitionIdentity::TypeBinding(action_binding.id()),
            parsed.parsed().type_exports()[1].span.start as u32,
            parsed.parsed().type_exports()[1].span.end as u32,
        ),
        action_origin(
            DefinitionIdentity::TypeBinding(integer_prelude.id()),
            parsed.parsed().type_exports()[2].span.start as u32,
            parsed.parsed().type_exports()[2].span.end as u32,
        ),
    ];
    let snapshot = StandardLibrarySnapshot::new(
        StandardLibraryRevisionId::from_bytes([0x47; 16]),
        StandardLibraryDigestVersion::Version1,
        source.clone(),
        "orna.language/1",
        catalogue.clone(),
        origins.clone(),
        Sha256Digest::from_bytes([0; 32]),
    )
    .unwrap();
    let digest = calculate_standard_library_digest(&snapshot).unwrap();
    verify_standard_library_snapshot(
        StandardLibrarySnapshot::new(
            snapshot.revision(),
            snapshot.digest_version(),
            source,
            snapshot.language_version(),
            catalogue,
            origins,
            digest,
        )
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn accepts_client_action_call_with_canonical_target_and_argument_identities() {
    let target_id = FunctionId::from_bytes([0x51; 16]);
    let target_parameter_id = ParameterId::from_bytes([0x52; 16]);
    let argument_type = ResolvedType::Scalar(StandardScalar::Integer);
    let integer_type_id = TypeId::from_bytes([0x48; 16]);
    let base = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes([0x53; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x54; 16]),
            QualifiedSemanticName::new(["tasks"]).unwrap(),
        )],
        Vec::new(),
        vec![FunctionDefinition::new(
            target_id,
            QualifiedSemanticName::new(["tasks", "run"]).unwrap(),
            FunctionDomain::Client,
            vec![ParameterDefinition::new(
                target_parameter_id,
                "p_value",
                0,
                argument_type,
                None,
            )],
            FunctionReturn::Single(argument_type),
            FunctionRevisionId::from_bytes([0x55; 16]),
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        )],
    )
    .unwrap();
    let standard =
        check_standard_library_source(&verified_standard_library_with_action_for_test()).unwrap();
    let context = StandardApplicationCheckContext::try_new(&base, &standard).unwrap();
    let source = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.run(p_value INTEGER) RETURNS std.Action AS std.action.call(target => tasks.run, arguments => std.call.args(p_value => p_value));";
    let report = check_standard_application(&bundle([("action.orna", source)]), &context);
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );

    let function = report
        .preparation_view()
        .unwrap()
        .checked()
        .client_functions()
        .iter()
        .find(|function| function.name().to_string() == "ui.run")
        .unwrap();
    let caller_parameter_id = function.parameters()[0].id();
    let CheckedClientFunctionBody::Expression { expression } = function.body() else {
        panic!("expected an expression CLIENT action body");
    };
    let super::CheckedClientExpression::Action { operation } = expression else {
        panic!("expected std.action.call to lower to an action operation");
    };
    assert_eq!(
        operation.target_domain(),
        orna_artifact::client_plan::ActionTargetDomain::Client
    );
    assert_eq!(
        operation.target(),
        super::CheckedFunctionId::Existing(target_id)
    );
    assert_eq!(operation.arguments().len(), 1);
    assert_eq!(
        operation.arguments()[0].0,
        super::CheckedParameterId::Existing(target_parameter_id)
    );
    assert!(matches!(
        &operation.arguments()[0].1,
        super::CheckedClientExpression::ParameterRead { parameter, .. }
            if *parameter == caller_parameter_id
    ));
    assert_eq!(
        operation.result_type(),
        super::SemanticType::Scalar(StandardScalar::Integer)
    );
    assert_eq!(operation.standard_result_type(), Some(integer_type_id));
}

#[test]
fn sorts_resource_and_action_arguments_by_checked_parameter_id() {
    let integer = ResolvedType::Scalar(StandardScalar::Integer);
    let resource_target_id = FunctionId::from_bytes([0x71; 16]);
    let resource_high_parameter_id = ParameterId::from_bytes([0x72; 16]);
    let resource_low_parameter_id = ParameterId::from_bytes([0x70; 16]);
    let resource_base = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes([0x73; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x74; 16]),
            QualifiedSemanticName::new(["tasks"]).unwrap(),
        )],
        Vec::new(),
        vec![FunctionDefinition::new(
            resource_target_id,
            QualifiedSemanticName::new(["tasks", "find"]).unwrap(),
            FunctionDomain::Server,
            vec![
                ParameterDefinition::new(resource_high_parameter_id, "p_high", 0, integer, None),
                ParameterDefinition::new(resource_low_parameter_id, "p_low", 1, integer, None),
            ],
            FunctionReturn::Single(integer),
            FunctionRevisionId::from_bytes([0x75; 16]),
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        )],
    )
    .unwrap();
    let resource_source = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.run() RETURNS INTEGER IS BEGIN RETURN AWAIT std.data.resource(target => tasks.find, arguments => std.call.args(p_low => 7, p_high => 8)); END;";
    let resource_report = check(
        &bundle([("resource-argument-order.orna", resource_source)]),
        &resource_base,
    );
    assert!(
        resource_report.diagnostics().is_empty(),
        "{:?}",
        resource_report.diagnostics()
    );
    let resource_function = resource_report
        .checked_bundle()
        .unwrap()
        .client_functions()
        .iter()
        .find(|function| function.name().to_string() == "ui.run")
        .unwrap();
    let CheckedClientFunctionBody::Expression { expression } = resource_function.body() else {
        panic!("resource body must be an expression");
    };
    let CheckedClientExpression::Await { expression, .. } = expression else {
        panic!("resource body must await the resource");
    };
    let CheckedClientExpression::Resource { operation } = expression.as_ref() else {
        panic!("await operand must be a resource");
    };
    assert_eq!(
        operation
            .arguments()
            .iter()
            .map(|(parameter, _)| *parameter)
            .collect::<Vec<_>>(),
        vec![
            super::CheckedParameterId::Existing(resource_low_parameter_id),
            super::CheckedParameterId::Existing(resource_high_parameter_id),
        ]
    );

    let action_target_id = FunctionId::from_bytes([0x76; 16]);
    let action_high_parameter_id = ParameterId::from_bytes([0x78; 16]);
    let action_low_parameter_id = ParameterId::from_bytes([0x77; 16]);
    let action_base = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes([0x79; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x7a; 16]),
            QualifiedSemanticName::new(["tasks"]).unwrap(),
        )],
        Vec::new(),
        vec![FunctionDefinition::new(
            action_target_id,
            QualifiedSemanticName::new(["tasks", "run"]).unwrap(),
            FunctionDomain::Client,
            vec![
                ParameterDefinition::new(action_high_parameter_id, "p_high", 0, integer, None),
                ParameterDefinition::new(action_low_parameter_id, "p_low", 1, integer, None),
            ],
            FunctionReturn::Single(integer),
            FunctionRevisionId::from_bytes([0x7b; 16]),
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        )],
    )
    .unwrap();
    let standard =
        check_standard_library_source(&verified_standard_library_with_action_for_test()).unwrap();
    let action_context = StandardApplicationCheckContext::try_new(&action_base, &standard).unwrap();
    let action_source = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.run() RETURNS std.Action AS std.action.call(target => tasks.run, arguments => std.call.args(p_low => 7, p_high => 8));";
    let action_report = check_standard_application(
        &bundle([("action-argument-order.orna", action_source)]),
        &action_context,
    );
    assert!(
        action_report.diagnostics().is_empty(),
        "{:?}",
        action_report.diagnostics()
    );
    let action_function = action_report
        .preparation_view()
        .unwrap()
        .checked()
        .client_functions()
        .iter()
        .find(|function| function.name().to_string() == "ui.run")
        .unwrap();
    let CheckedClientFunctionBody::Expression { expression } = action_function.body() else {
        panic!("action body must be an expression");
    };
    let CheckedClientExpression::Action { operation } = expression else {
        panic!("action body must be an action");
    };
    assert_eq!(
        operation
            .arguments()
            .iter()
            .map(|(parameter, _)| *parameter)
            .collect::<Vec<_>>(),
        vec![
            super::CheckedParameterId::Existing(action_low_parameter_id),
            super::CheckedParameterId::Existing(action_high_parameter_id),
        ]
    );
}

#[test]
fn rejects_actions_in_client_state_returns_before_preparation() {
    let target_id = FunctionId::from_bytes([0x51; 16]);
    let argument_type = ResolvedType::Scalar(StandardScalar::Integer);
    let base = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes([0x53; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x54; 16]),
            QualifiedSemanticName::new(["tasks"]).unwrap(),
        )],
        Vec::new(),
        vec![FunctionDefinition::new(
            target_id,
            QualifiedSemanticName::new(["tasks", "run"]).unwrap(),
            FunctionDomain::Client,
            Vec::new(),
            FunctionReturn::Single(argument_type),
            FunctionRevisionId::from_bytes([0x55; 16]),
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        )],
    )
    .unwrap();
    let standard =
        check_standard_library_source(&verified_standard_library_with_action_for_test()).unwrap();
    let context = StandardApplicationCheckContext::try_new(&base, &standard).unwrap();
    let source = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.run() RETURNS std.Action IS \
            STATE ready INTEGER; \
            BEGIN RETURN std.action.call(target => tasks.run, arguments => std.call.args()); END;";
    let report = check_standard_application(&bundle([("state-action.orna", source)]), &context);
    assert_eq!(report.diagnostics().len(), 1, "{:?}", report.diagnostics());
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::DomainIncompatible,
        "{:?}",
        report.diagnostics()
    );
    assert_eq!(
        report.diagnostics()[0].message(),
        "CLIENT state blocks do not support action expressions"
    );
    assert!(report.preparation_view().is_none());
}

#[test]
fn accepts_client_action_call_with_canonical_server_target() {
    let target_id = FunctionId::from_bytes([0x56; 16]);
    let target_parameter_id = ParameterId::from_bytes([0x57; 16]);
    let argument_type = ResolvedType::Scalar(StandardScalar::Integer);
    let integer_type_id = TypeId::from_bytes([0x48; 16]);
    let base = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes([0x58; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x59; 16]),
            QualifiedSemanticName::new(["tasks"]).unwrap(),
        )],
        Vec::new(),
        vec![FunctionDefinition::new(
            target_id,
            QualifiedSemanticName::new(["tasks", "rebuild"]).unwrap(),
            FunctionDomain::Server,
            vec![ParameterDefinition::new(
                target_parameter_id,
                "p_value",
                0,
                argument_type,
                None,
            )],
            FunctionReturn::Single(argument_type),
            FunctionRevisionId::from_bytes([0x5a; 16]),
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        )],
    )
    .unwrap();
    let standard =
        check_standard_library_source(&verified_standard_library_with_action_for_test()).unwrap();
    let context = StandardApplicationCheckContext::try_new(&base, &standard).unwrap();
    let source = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.run(p_value INTEGER) RETURNS std.Action AS std.action.call(target => tasks.rebuild, arguments => std.call.args(p_value => p_value));";
    let report = check_standard_application(&bundle([("action-server.orna", source)]), &context);
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );

    let function = report
        .preparation_view()
        .unwrap()
        .checked()
        .client_functions()
        .iter()
        .find(|function| function.name().to_string() == "ui.run")
        .unwrap();
    let caller_parameter_id = function.parameters()[0].id();
    let CheckedClientFunctionBody::Expression { expression } = function.body() else {
        panic!("expected an expression CLIENT action body");
    };
    let super::CheckedClientExpression::Action { operation } = expression else {
        panic!("expected std.action.call to lower to an action operation");
    };
    assert_eq!(
        operation.target_domain(),
        orna_artifact::client_plan::ActionTargetDomain::Server
    );
    assert_eq!(
        operation.target(),
        super::CheckedFunctionId::Existing(target_id)
    );
    assert_eq!(operation.arguments().len(), 1);
    assert_eq!(
        operation.arguments()[0].0,
        super::CheckedParameterId::Existing(target_parameter_id)
    );
    assert!(matches!(
        &operation.arguments()[0].1,
        super::CheckedClientExpression::ParameterRead { parameter, .. }
            if *parameter == caller_parameter_id
    ));
    assert_eq!(
        operation.result_type(),
        super::SemanticType::Scalar(StandardScalar::Integer)
    );
    assert_eq!(operation.standard_result_type(), Some(integer_type_id));
}

#[test]
fn excludes_stream_and_one_column_rows_action_targets() {
    let integer = ResolvedType::Scalar(StandardScalar::Integer);
    let base = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes([0x5a; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x5b; 16]),
            QualifiedSemanticName::new(["tasks"]).unwrap(),
        )],
        Vec::new(),
        vec![
            FunctionDefinition::new(
                FunctionId::from_bytes([0x5c; 16]),
                QualifiedSemanticName::new(["tasks", "events"]).unwrap(),
                FunctionDomain::Server,
                Vec::new(),
                FunctionReturn::Stream(integer),
                FunctionRevisionId::from_bytes([0x5d; 16]),
                FunctionSecurity::Invoker,
                None,
                FunctionVolatility::Immutable,
            ),
            FunctionDefinition::new(
                FunctionId::from_bytes([0x5e; 16]),
                QualifiedSemanticName::new(["tasks", "rows"]).unwrap(),
                FunctionDomain::Server,
                Vec::new(),
                FunctionReturn::Rows(vec![rows_column("value", 0, integer)]),
                FunctionRevisionId::from_bytes([0x5f; 16]),
                FunctionSecurity::Invoker,
                None,
                FunctionVolatility::Immutable,
            ),
        ],
    )
    .unwrap();
    let standard =
        check_standard_library_source(&verified_standard_library_with_action_for_test()).unwrap();
    let context = StandardApplicationCheckContext::try_new(&base, &standard).unwrap();
    let source = "CREATE SCHEMA ui; \
            CREATE CLIENT FUNCTION ui.stream() RETURNS STREAM<INTEGER> IS \
            BEGIN RETURN AWAIT std.data.stream_resource(target => tasks.events, arguments => std.call.args()); END; \
            CREATE CLIENT FUNCTION ui.stream_action() RETURNS std.Action AS \
            std.action.call(target => ui.stream, arguments => std.call.args()); \
            CREATE CLIENT FUNCTION ui.rows_action() RETURNS std.Action AS \
            std.action.call(target => tasks.rows, arguments => std.call.args());";
    let report = check_standard_application(&bundle([("action-shapes.orna", source)]), &context);
    let messages = report
        .diagnostics()
        .iter()
        .map(|diagnostic| diagnostic.message())
        .collect::<Vec<_>>();
    assert!(
        messages.contains(&"unknown std.action.call target ui.stream"),
        "{messages:?}"
    );
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.ends_with("does not return one durable value"))
            .count(),
        1,
        "{messages:?}"
    );
}

#[test]
fn accepts_transient_standard_opaque_action_target_result() {
    let target_id = FunctionId::from_bytes([0x5a; 16]);
    let action_type = ResolvedType::Named(STD_ACTION_TYPE_ID);
    let base = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes([0x5b; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x5c; 16]),
            QualifiedSemanticName::new(["tasks"]).unwrap(),
        )],
        Vec::new(),
        vec![FunctionDefinition::new(
            target_id,
            QualifiedSemanticName::new(["tasks", "run"]).unwrap(),
            FunctionDomain::Client,
            Vec::new(),
            FunctionReturn::Single(action_type),
            FunctionRevisionId::from_bytes([0x5d; 16]),
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        )],
    )
    .unwrap();
    let standard =
        check_standard_library_source(&verified_standard_library_with_action_for_test()).unwrap();
    let context = StandardApplicationCheckContext::try_new(&base, &standard).unwrap();
    let source = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.run() RETURNS std.Action AS std.action.call(target => tasks.run, arguments => std.call.args());";
    let report = check_standard_application(&bundle([("action-transient.orna", source)]), &context);
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let function = report
        .preparation_view()
        .unwrap()
        .checked()
        .client_functions()
        .iter()
        .find(|function| function.name().to_string() == "ui.run")
        .unwrap();
    let CheckedClientFunctionBody::Expression { expression } = function.body() else {
        panic!("expected an expression CLIENT action body");
    };
    let super::CheckedClientExpression::Action { operation } = expression else {
        panic!("expected std.action.call to lower to an action operation");
    };
    assert_eq!(
        operation.target(),
        super::CheckedFunctionId::Existing(target_id)
    );
    assert_eq!(
        operation.result_type(),
        super::SemanticType::Named(super::CheckedTypeId::Existing(STD_ACTION_TYPE_ID))
    );
    assert_eq!(operation.standard_result_type(), Some(STD_ACTION_TYPE_ID));
}

#[test]
fn accepts_orv3_enum_and_record_action_arguments() {
    let standard =
        check_standard_library_source(&verified_standard_library_with_action_for_test()).unwrap();
    let base = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&base, &standard).unwrap();
    let source = "CREATE SCHEMA app; \
            CREATE TYPE app.phase AS ENUM ('ready', 'done'); \
            CREATE TYPE app.status AS VALUE (active INTEGER) IMMUTABLE PERSISTABLE; \
            CREATE CLIENT FUNCTION app.target(p_phase app.phase, p_status app.status) RETURNS INTEGER AS 1; \
            CREATE CLIENT FUNCTION app.run(p_phase app.phase, p_status app.status) RETURNS std.Action AS \
                std.action.call(target => app.target, arguments => std.call.args(p_phase => p_phase, p_status => p_status));";
    let report = check_standard_application(&bundle([("action-orv3.orna", source)]), &context);
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let function = report
        .preparation_view()
        .unwrap()
        .checked()
        .client_functions()
        .iter()
        .find(|function| function.name().to_string() == "app.run")
        .unwrap();
    let CheckedClientFunctionBody::Expression { expression } = function.body() else {
        panic!("expected an expression CLIENT action body");
    };
    let super::CheckedClientExpression::Action { operation } = expression else {
        panic!("expected std.action.call to lower to an action operation");
    };
    assert_eq!(operation.arguments().len(), 2);
}

#[test]
fn rejects_action_call_targets_reserved_combinators_and_argument_errors() {
    let target_id = FunctionId::from_bytes([0x61; 16]);
    let target_parameter_id = ParameterId::from_bytes([0x62; 16]);
    let argument_type = ResolvedType::Scalar(StandardScalar::Integer);
    let target_bad_id = FunctionId::from_bytes([0x66; 16]);
    let bad_parameter_id = ParameterId::from_bytes([0x67; 16]);
    let target_bad_result_id = FunctionId::from_bytes([0x69; 16]);
    let application_enum_type_id = TypeId::from_bytes([0x6b; 16]);
    let action_type = ResolvedType::Named(STD_ACTION_TYPE_ID);
    let bad_result_type = ResolvedType::Named(application_enum_type_id);
    let base = CatalogueSnapshot::new_with_functions_and_enum_types(
        CatalogueRevisionId::from_bytes([0x63; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x64; 16]),
            QualifiedSemanticName::new(["tasks"]).unwrap(),
        )],
        Vec::new(),
        Vec::new(),
        vec![EnumTypeDefinition::new(
            application_enum_type_id,
            QualifiedSemanticName::new(["tasks", "status"]).unwrap(),
            ["ready", "done"],
        )],
        Vec::new(),
        vec![
            FunctionDefinition::new(
                target_id,
                QualifiedSemanticName::new(["tasks", "run"]).unwrap(),
                FunctionDomain::Client,
                vec![ParameterDefinition::new(
                    target_parameter_id,
                    "p_value",
                    0,
                    argument_type,
                    None,
                )],
                FunctionReturn::Single(argument_type),
                FunctionRevisionId::from_bytes([0x65; 16]),
                FunctionSecurity::Invoker,
                None,
                FunctionVolatility::Immutable,
            ),
            FunctionDefinition::new(
                target_bad_id,
                QualifiedSemanticName::new(["tasks", "bad"]).unwrap(),
                FunctionDomain::Client,
                vec![ParameterDefinition::new(
                    bad_parameter_id,
                    "p_action",
                    0,
                    action_type,
                    None,
                )],
                FunctionReturn::Single(argument_type),
                FunctionRevisionId::from_bytes([0x68; 16]),
                FunctionSecurity::Invoker,
                None,
                FunctionVolatility::Immutable,
            ),
            FunctionDefinition::new(
                target_bad_result_id,
                QualifiedSemanticName::new(["tasks", "bad_return"]).unwrap(),
                FunctionDomain::Client,
                Vec::new(),
                FunctionReturn::Single(bad_result_type),
                FunctionRevisionId::from_bytes([0x6a; 16]),
                FunctionSecurity::Invoker,
                None,
                FunctionVolatility::Immutable,
            ),
        ],
    )
    .unwrap();
    let standard =
        check_standard_library_source(&verified_standard_library_with_action_for_test()).unwrap();
    let context = StandardApplicationCheckContext::try_new(&base, &standard).unwrap();
    let cases = [
        (
            "std.action.call(target => missing.run, arguments => std.call.args(p_value => p_value))",
            DiagnosticCode::UnknownQualifiedName,
            "unknown std.action.call target missing.run",
        ),
        (
            "std.action.call(target => TRUE, arguments => std.call.args(p_value => p_value))",
            DiagnosticCode::TypeMismatch,
            "std.action.call target must be a qualified function name",
        ),
        (
            "std.action.call(target => tasks.run, arguments => TRUE)",
            DiagnosticCode::TypeMismatch,
            "std.action.call arguments must be a std.call.args value",
        ),
        (
            "std.action.call(target => tasks.bad, arguments => std.call.args(p_action => p_value))",
            DiagnosticCode::TypeMismatch,
            "std.action.call target tasks.bad has a parameter that is not ORV3-encodable",
        ),
        (
            "std.action.call(target => tasks.bad_return, arguments => std.call.args())",
            DiagnosticCode::UnknownQualifiedName,
            "std.action.call target tasks.bad_return does not return one durable value",
        ),
        (
            "std.action.sequence()",
            DiagnosticCode::UnknownQualifiedName,
            "unknown CLIENT function std.action.sequence",
        ),
        (
            "std.action.parallel()",
            DiagnosticCode::UnknownQualifiedName,
            "unknown CLIENT function std.action.parallel",
        ),
        (
            "std.action.call(target => tasks.run, arguments => std.call.args())",
            DiagnosticCode::TypeMismatch,
            "missing argument for std.action.call target tasks.run",
        ),
        (
            "std.action.call(target => tasks.run, arguments => std.call.args(missing => p_value))",
            DiagnosticCode::UnknownQualifiedName,
            "unknown std.action.call parameter missing",
        ),
        (
            "std.action.call(target => tasks.run, arguments => std.call.args(p_value => p_value, p_value => p_value))",
            DiagnosticCode::DuplicateDefinition,
            "duplicate std.action.call parameter p_value",
        ),
        (
            "std.action.call(target => tasks.run, arguments => std.call.args(p_value => 'wrong'))",
            DiagnosticCode::TypeMismatch,
            "std.action.call argument does not match parameter p_value",
        ),
    ];
    for (index, (expression, code, message)) in cases.into_iter().enumerate() {
        let source = format!(
            "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.run(p_value INTEGER) RETURNS std.Action AS {expression};"
        );
        let report = check_standard_application(
            &SourceBundle::new([SourceUnit::new("action-reject.orna", source)]).unwrap(),
            &context,
        );
        assert_eq!(
            report.diagnostics().len(),
            1,
            "case {index}: {:?}",
            report.diagnostics()
        );
        assert_eq!(report.diagnostics()[0].code(), code, "case {index}");
        assert_eq!(report.diagnostics()[0].message(), message, "case {index}");
        assert!(report.checked_bundle().is_none(), "case {index}");
    }
}

#[test]
fn checked_opaque_standard_remains_definition_only_for_applications() {
    let snapshot = verified_standard_library_with_opaque_for_test();
    let standard = check_standard_library_source(&snapshot).unwrap();
    assert_eq!(standard.value_types().len(), 2);
    assert_eq!(standard.value_types()[0].kind(), ValueTypeKind::Primitive);
    assert_eq!(standard.value_types()[1].kind(), ValueTypeKind::Opaque);
    assert_eq!(
        standard.value_types()[1].representation_contract(),
        "std.token@1"
    );
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();

    let source = "CREATE SCHEMA app;CREATE TYPE app.item AS OBJECT (token std.TOKEN NOT NULL);";
    let report = check_standard_application(&bundle([("opaque-use.orna", source)]), &context);
    assert!(report.checked_bundle().is_none());
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::UnknownQualifiedName
    );
    assert_eq!(
        report.diagnostics()[0].message(),
        "unknown type name std.token"
    );
}

fn empty_version_two_active(
    standard: &orna_core::revision::VerifiedStandardLibrarySnapshot,
) -> ActiveDatabaseRevision {
    let source_unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes([0x41; 16]),
        0,
        "active.orna",
        "",
        source_unit_content_digest("").unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
    let source = StoredSourceRevision::new(
        SourceBundleId::from_bytes([0x42; 16]),
        SourceRevisionId::from_bytes([0x43; 16]),
        None,
        vec![source_unit],
        bundle_hash,
        source_revision_record_digest(SourceBundleId::from_bytes([0x42; 16]), None, bundle_hash)
            .unwrap(),
    )
    .unwrap();
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([0x44; 16]),
        vec![],
        vec![],
        vec![],
        vec![],
    )
    .unwrap();
    let context = CatalogueHashContext::version_two(standard.clone());
    let catalogue_hash =
        catalogue_digest_with_context(&context, &catalogue, &[], &[], &[], &[]).unwrap();
    ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            RevisionPair::new(source.id(), catalogue.revision()),
            source,
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(Vec::new(), Vec::new(), Vec::new(), Vec::new()),
        ),
        context,
    )
    .unwrap()
}

fn active_from_prepared(prepared: &DeployableRevision) -> ActiveDatabaseRevision {
    ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            prepared.candidate_pair(),
            prepared.source().clone(),
            prepared.candidate().clone(),
            prepared.catalogue_hash(),
            ActiveRevisionContent::new(
                prepared.expressions().to_vec(),
                prepared
                    .current_function_revisions()
                    .map_or_else(Vec::new, ToOwned::to_owned),
                prepared.origins().to_vec(),
                prepared.references().to_vec(),
            ),
        ),
        prepared.catalogue_hash_context().clone(),
    )
    .unwrap()
}

fn expression_use<'a>(
    uses: &[&'a CheckedApplicationTypeUse],
    ordinal: u32,
) -> &'a CheckedApplicationTypeUse {
    let matches = uses
        .iter()
        .copied()
        .filter(|type_use| {
            matches!(
                type_use.kind(),
                CheckedTypeUseKind::Expression {
                    ordinal: candidate,
                    ..
                } if candidate == ordinal
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "expected expression ordinal {ordinal}");
    matches[0]
}

fn result_use<'a>(
    uses: &[&'a CheckedApplicationTypeUse],
    ordinal: u32,
) -> &'a CheckedApplicationTypeUse {
    let matches = uses
        .iter()
        .copied()
        .filter(|type_use| {
            matches!(
                type_use.kind(),
                CheckedTypeUseKind::Result {
                    ordinal: candidate,
                    ..
                } if candidate == ordinal
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "expected result ordinal {ordinal}");
    matches[0]
}

fn assert_type_use_span(type_use: &CheckedApplicationTypeUse, start: usize, text: &str) {
    assert_eq!(type_use.location().span().start(), start);
    assert_eq!(type_use.location().span().end(), start + text.len());
}

fn checked_use_index(
    uses: &[CheckedApplicationTypeUse],
    kind: CheckedTypeUseKind,
    start: usize,
    end: usize,
) -> usize {
    let matches = uses
        .iter()
        .enumerate()
        .filter(|(_, type_use)| {
            type_use.kind() == kind
                && type_use.location().span().start() == start
                && type_use.location().span().end() == end
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "expected one exact arena use");
    matches[0]
}

#[test]
fn records_standard_client_boolean_body_uses_with_the_resolved_type_id() {
    let standard =
        check_standard_library_source(&verified_standard_library_for_relational_test()).unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source =
        "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;";
    let report = check_standard_application(&bundle([("application.orna", source)]), &context);

    assert_eq!(report.diagnostics(), &[]);
    let checked = report.checked_bundle().unwrap();
    let clients = checked.client_functions().collect::<Vec<_>>();
    assert_eq!(clients.len(), 1);
    assert_eq!(checked.uses().len(), 3);

    let boolean = TypeId::from_bytes([3; 16]);
    let expected_kinds = [
        CheckedTypeUseKind::Return {
            owner: clients[0].id(),
            ordinal: 0,
        },
        CheckedTypeUseKind::Expression {
            owner: clients[0].id(),
            ordinal: 0,
        },
        CheckedTypeUseKind::Result {
            owner: clients[0].id(),
            ordinal: 0,
        },
    ];
    assert_eq!(
        checked
            .uses()
            .iter()
            .map(CheckedApplicationTypeUse::kind)
            .collect::<Vec<_>>(),
        expected_kinds
    );
    let literal_start = source.find("TRUE").unwrap();
    for type_use in &checked.uses()[1..] {
        assert_eq!(
            type_use.value().map(CheckedValueTypeUse::type_id),
            Some(boolean)
        );
        assert_eq!(type_use.location().span().start(), literal_start);
        assert_eq!(
            type_use.location().span().end(),
            literal_start + "TRUE".len()
        );
    }
    assert!(
        checked_use_index(
            checked.uses(),
            expected_kinds[1],
            literal_start,
            literal_start + "TRUE".len(),
        ) < checked_use_index(
            checked.uses(),
            expected_kinds[2],
            literal_start,
            literal_start + "TRUE".len(),
        )
    );
}

#[test]
fn records_standard_client_state_slot_use_as_declaration_evidence() {
    let standard =
        check_standard_library_source(&verified_standard_library_for_relational_test()).unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source = "CREATE SCHEMA app; \
            CREATE CLIENT FUNCTION app.state() RETURNS BOOLEAN IS \
            STATE flag BOOLEAN; BEGIN RETURN TRUE; END;";
    let report = check_standard_application(&bundle([("state.orna", source)]), &context);

    assert_eq!(report.diagnostics(), &[]);
    let checked = report.checked_bundle().unwrap();
    let function = checked.client_functions().next().unwrap();
    let state_kind = CheckedTypeUseKind::State {
        owner: function.id(),
        ordinal: 0,
    };
    let state_start = source.find("STATE flag BOOLEAN").unwrap() + "STATE flag ".len();
    let state_use = checked
        .uses()
        .iter()
        .find(|type_use| type_use.kind() == state_kind)
        .expect("state type use");

    assert_eq!(checked.uses().len(), 4);
    assert_eq!(
        state_use.value().map(CheckedValueTypeUse::type_id),
        Some(TypeId::from_bytes([3; 16]))
    );
    assert_type_use_span(state_use, state_start, "BOOLEAN");
    assert!(
        checked
            .preparation_evidence
            .declaration_uses
            .iter()
            .any(|type_use| type_use.kind() == state_kind)
    );
    assert_eq!(checked.standard_type_references().len(), 1);
    assert_eq!(checked.standard_type_references()[0].owner(), function.id());
    assert_eq!(checked.standard_type_references()[0].ordinal(), 0);
}

#[test]
fn rejects_nested_client_stream_call_operands() {
    let standard =
        check_standard_library_source(&verified_standard_library_for_relational_test()).unwrap();
    let base = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&base, &standard).unwrap();
    let source = "CREATE SCHEMA app; \
            CREATE EXTERNAL CLIENT FUNCTION app.events() RETURNS STREAM<BOOLEAN> \
            RUNTIME CONTRACT 'app.events@1'; \
            CREATE CLIENT FUNCTION app.forward() RETURNS STREAM<BOOLEAN> IS BEGIN RETURN app.events(); END;";
    let report = check_standard_application(&bundle([("stream-call.orna", source)]), &context);

    assert!(
        report.diagnostics().iter().any(|diagnostic| {
            diagnostic.code() == DiagnosticCode::TypeMismatch
                && diagnostic
                    .message()
                    .contains("CLIENT STREAM function app.events")
        }),
        "{:?}",
        report.diagnostics()
    );
}

#[test]
fn records_client_stream_return_shape_and_element_evidence() {
    let standard =
        check_standard_library_source(&verified_standard_library_for_relational_test()).unwrap();
    let base = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&base, &standard).unwrap();
    let source = "CREATE SCHEMA app; \
            CREATE EXTERNAL CLIENT FUNCTION app.events() RETURNS STREAM<BOOLEAN> \
            RUNTIME CONTRACT 'app.events@1';";
    let report = check_standard_application(&bundle([("stream-client.orna", source)]), &context);

    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let view = report.preparation_view().unwrap();
    let checked = view.checked();
    let function = &checked.client_functions()[0];
    assert_eq!(function.return_shape(), CheckedClientReturnShape::Stream,);
    assert_eq!(
        function.return_type(),
        SemanticType::scalar(StandardScalar::Boolean)
    );
    assert_eq!(view.uses().len(), 1);
    assert_eq!(
        view.uses()[0].kind(),
        CheckedTypeUseKind::Return {
            owner: function.id(),
            ordinal: 0,
        },
    );
    assert_eq!(
        view.uses()[0].value().map(CheckedValueTypeUse::type_id),
        Some(TypeId::from_bytes([3; 16])),
    );
}

#[test]
fn retains_standard_preparation_evidence_from_canonical_uses_and_references() {
    let changed_boolean = TypeId::from_bytes([0x53; 16]);
    let snapshot = verified_standard_library_for_relational_test_with_boolean_id(
        changed_boolean,
        [
            0xa2, 0x5b, 0xcf, 0x20, 0x76, 0x46, 0x26, 0xdf, 0xe3, 0x77, 0x67, 0xca, 0x79, 0xc9,
            0x3e, 0x5f, 0xdc, 0x53, 0x8c, 0xc0, 0x7b, 0x74, 0xce, 0xac, 0x54, 0x2d, 0xb9, 0x31,
            0x3c, 0x56, 0xe1, 0x82,
        ],
    );
    let standard = check_standard_library_source(&snapshot).unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let first_server = "CREATE SERVER FUNCTION app.create(p_ref REF app.item, p_boolean BOOLEAN, p_alias std.BOOLEAN) \
            RETURNS ROWS (created REF app.item) TRANSACTION ATOMIC \
            AS INSERT INTO app.item AS made (done) VALUES (p_boolean) RETURNING REF(made);";
    let client = "CREATE CLIENT FUNCTION app.enabled() RETURNS std.BOOLEAN RETURN TRUE;";
    let second_server = "CREATE SERVER FUNCTION app.by_ref(p_ref REF app.item) \
            RETURNS ROWS (value std.BOOLEAN) TRANSACTION READ ONLY VOLATILITY STABLE \
            AS SELECT TRUE FROM app.item item WHERE REF(item) = p_ref;";
    let declarations = "CREATE SCHEMA app; \
            CREATE TYPE app.item AS OBJECT (done BOOLEAN NOT NULL);";
    let report = check_standard_application(
        &bundle([
            ("z-first-server.orna", first_server),
            ("a-client.orna", client),
            ("y-second-server.orna", second_server),
            ("m-declarations.orna", declarations),
        ]),
        &context,
    );

    assert_eq!(report.diagnostics(), &[]);
    let checked = report.checked_bundle().unwrap();
    let server_functions = checked.server_functions().collect::<Vec<_>>();
    let client_functions = checked.client_functions().collect::<Vec<_>>();
    let [create, list] = server_functions.as_slice() else {
        assert_eq!(server_functions.len(), 2);
        return;
    };
    let [enabled] = client_functions.as_slice() else {
        assert_eq!(client_functions.len(), 1);
        return;
    };

    let first_boolean = first_server.find("p_boolean BOOLEAN").unwrap() + "p_boolean ".len();
    let first_alias = first_server.find("p_alias std.BOOLEAN").unwrap() + "p_alias ".len();
    let client_boolean = client.find("std.BOOLEAN").unwrap();
    let second_boolean = second_server.find("value std.BOOLEAN").unwrap() + "value ".len();
    assert_eq!(
        checked
            .standard_type_references()
            .iter()
            .map(|reference| {
                (
                    reference.owner(),
                    reference.ordinal(),
                    reference.target(),
                    reference.location().logical_path(),
                    reference.location().span().start(),
                    reference.location().span().end(),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (
                create.id(),
                1,
                changed_boolean,
                "z-first-server.orna",
                first_boolean,
                first_boolean + "BOOLEAN".len(),
            ),
            (
                create.id(),
                2,
                changed_boolean,
                "z-first-server.orna",
                first_alias,
                first_alias + "std.BOOLEAN".len(),
            ),
            (
                enabled.id(),
                0,
                changed_boolean,
                "a-client.orna",
                client_boolean,
                client_boolean + "std.BOOLEAN".len(),
            ),
            (
                list.id(),
                1,
                changed_boolean,
                "y-second-server.orna",
                second_boolean,
                second_boolean + "std.BOOLEAN".len(),
            ),
        ]
    );
    assert_eq!(
        checked.preparation_evidence.type_uses,
        checked.uses(),
        "preparation evidence must retain the canonical type-use arena after sorting"
    );
    let evidence_paths =
        checked
            .preparation_evidence
            .type_uses
            .iter()
            .fold(Vec::new(), |mut paths, type_use| {
                let path = type_use.location().logical_path();
                if paths.last().is_none_or(|previous| *previous != path) {
                    paths.push(path);
                }
                paths
            });
    assert_eq!(
        evidence_paths,
        vec![
            "z-first-server.orna",
            "a-client.orna",
            "y-second-server.orna",
            "m-declarations.orna",
        ],
        "canonical source-unit order is insertion order, not logical-path order"
    );
    assert_eq!(
        checked.preparation_evidence.standard_type_references, checked.standard_type_references,
        "preparation evidence must retain the canonical flattened signature references"
    );

    let object_types = checked.object_types().collect::<Vec<_>>();
    let [item] = object_types.as_slice() else {
        assert_eq!(object_types.len(), 1);
        return;
    };
    let fields = item.fields().collect::<Vec<_>>();
    let [done] = fields.as_slice() else {
        assert_eq!(fields.len(), 1);
        return;
    };
    let first_ref = first_server.find("p_ref REF app.item").unwrap() + "p_ref REF ".len();
    let created_ref = first_server.find("created REF app.item").unwrap() + "created REF ".len();
    let second_ref = second_server.find("p_ref REF app.item").unwrap() + "p_ref REF ".len();
    let field_boolean = declarations.find("done BOOLEAN").unwrap() + "done ".len();
    assert_eq!(
        [
            done.resolved_type(),
            create.parameters().next().unwrap().resolved_type(),
            create.parameters().nth(1).unwrap().resolved_type(),
            create.parameters().nth(2).unwrap().resolved_type(),
            create.return_columns().next().unwrap().resolved_type(),
            enabled.return_type(),
            list.parameters().next().unwrap().resolved_type(),
            list.return_columns().next().unwrap().resolved_type(),
        ]
        .into_iter()
        .map(|type_use| match type_use {
            CheckedApplicationTypeUse::Value(value) => (Some(value.type_id()), None),
            CheckedApplicationTypeUse::Named { .. } => (None, None),
            CheckedApplicationTypeUse::ObjectReference(reference) => {
                (None, Some(reference.target()))
            }
        })
        .collect::<Vec<_>>(),
        vec![
            (Some(changed_boolean), None),
            (None, Some(item.id())),
            (Some(changed_boolean), None),
            (Some(changed_boolean), None),
            (None, Some(item.id())),
            (Some(changed_boolean), None),
            (None, Some(item.id())),
            (Some(changed_boolean), None),
        ],
        "public scalar-free views must retain each value ID and REF target"
    );
    assert_eq!(
        checked
            .preparation_evidence
            .declaration_uses
            .iter()
            .map(|type_use| {
                (
                    type_use.kind(),
                    type_use.value().map(CheckedValueTypeUse::type_id),
                    type_use
                        .object_reference()
                        .map(|reference| reference.target()),
                    type_use.location().logical_path().to_owned(),
                    type_use.location().span().start(),
                    type_use.location().span().end(),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (
                CheckedTypeUseKind::Parameter {
                    owner: create.id(),
                    parameter: create.parameters().next().unwrap().id(),
                },
                None,
                Some(item.id()),
                "z-first-server.orna".to_owned(),
                first_ref,
                first_ref + "app.item".len(),
            ),
            (
                CheckedTypeUseKind::Parameter {
                    owner: create.id(),
                    parameter: create.parameters().nth(1).unwrap().id(),
                },
                Some(changed_boolean),
                None,
                "z-first-server.orna".to_owned(),
                first_boolean,
                first_boolean + "BOOLEAN".len(),
            ),
            (
                CheckedTypeUseKind::Parameter {
                    owner: create.id(),
                    parameter: create.parameters().nth(2).unwrap().id(),
                },
                Some(changed_boolean),
                None,
                "z-first-server.orna".to_owned(),
                first_alias,
                first_alias + "std.BOOLEAN".len(),
            ),
            (
                CheckedTypeUseKind::Return {
                    owner: create.id(),
                    ordinal: 0,
                },
                None,
                Some(item.id()),
                "z-first-server.orna".to_owned(),
                created_ref,
                created_ref + "app.item".len(),
            ),
            (
                CheckedTypeUseKind::Return {
                    owner: enabled.id(),
                    ordinal: 0,
                },
                Some(changed_boolean),
                None,
                "a-client.orna".to_owned(),
                client_boolean,
                client_boolean + "std.BOOLEAN".len(),
            ),
            (
                CheckedTypeUseKind::Parameter {
                    owner: list.id(),
                    parameter: list.parameters().next().unwrap().id(),
                },
                None,
                Some(item.id()),
                "y-second-server.orna".to_owned(),
                second_ref,
                second_ref + "app.item".len(),
            ),
            (
                CheckedTypeUseKind::Return {
                    owner: list.id(),
                    ordinal: 0,
                },
                Some(changed_boolean),
                None,
                "y-second-server.orna".to_owned(),
                second_boolean,
                second_boolean + "std.BOOLEAN".len(),
            ),
            (
                CheckedTypeUseKind::Field {
                    owner: item.id(),
                    field: done.id(),
                },
                Some(changed_boolean),
                None,
                "m-declarations.orna".to_owned(),
                field_boolean,
                field_boolean + "BOOLEAN".len(),
            ),
        ]
    );
    let made_ref = first_server.find("REF(made)").unwrap();
    assert_eq!(
        checked
            .preparation_evidence
            .type_uses
            .iter()
            .filter(|type_use| {
                type_use.location().logical_path() == "z-first-server.orna"
                    && type_use.location().span().start() == made_ref
                    && type_use.location().span().end() == made_ref + "REF(made)".len()
            })
            .map(CheckedApplicationTypeUse::kind)
            .collect::<Vec<_>>(),
        vec![
            CheckedTypeUseKind::Expression {
                owner: create.id(),
                ordinal: 1,
            },
            CheckedTypeUseKind::Result {
                owner: create.id(),
                ordinal: 0,
            },
        ],
        "the sealed full arena must retain Expression-before-Result at a coincident span"
    );

    let create_declaration_uses = checked
        .uses()
        .iter()
        .filter(|type_use| {
            matches!(
                type_use.kind(),
                CheckedTypeUseKind::Parameter { owner, .. }
                    | CheckedTypeUseKind::Return { owner, .. }
                    if owner == create.id()
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(create_declaration_uses.len(), 4);
    assert!(create_declaration_uses[0].object_reference().is_some());
    assert!(create_declaration_uses[1].value().is_some());
    assert!(create_declaration_uses[2].value().is_some());
    assert!(create_declaration_uses[3].object_reference().is_some());
    assert_eq!(
        create
            .references()
            .iter()
            .map(|reference| reference.kind())
            .collect::<Vec<_>>(),
        vec![
            DefinitionReferenceKind::ObjectReference,
            DefinitionReferenceKind::ObjectReference,
            DefinitionReferenceKind::WriteObject,
            DefinitionReferenceKind::WriteField,
            DefinitionReferenceKind::ParameterRead,
            DefinitionReferenceKind::ObjectReference,
        ]
    );
}

#[test]
fn accepts_standard_server_scalar_select_and_preserves_references() {
    let standard =
        check_standard_library_source(&verified_standard_library_for_relational_test()).unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source = "CREATE SCHEMA app; \
            CREATE TYPE app.item AS OBJECT (); \
            CREATE SERVER FUNCTION app.find() RETURNS BOOLEAN \
            AS SELECT TRUE FROM app.item item;";
    let report = check_standard_application(&bundle([("application.orna", source)]), &context);

    assert_eq!(report.diagnostics(), &[]);
    let checked = report.checked_bundle().unwrap();
    let functions = checked.server_functions().collect::<Vec<_>>();
    assert_eq!(functions.len(), 1);
    let function = functions[0];
    assert_eq!(function.return_columns().count(), 0);
    assert_eq!(function.references().len(), 1);
    assert!(matches!(
        function.references()[0].target(),
        CheckedDefinitionReferenceTarget::ObjectType(_)
    ));
}

#[test]
fn rejects_a_client_boolean_literal_when_the_checked_standard_lacks_boolean() {
    let snapshot = verified_standard_library_for_relational_test();
    let standard = checked_standard_library_with_contract_overrides_for_test(
        &snapshot,
        &[(0, "orna.kernel.value.integer@1")],
    )
    .unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source =
        "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;";
    let report = check_standard_application(&bundle([("application.orna", source)]), &context);

    assert!(report.checked_bundle().is_none());
    assert_eq!(report.diagnostics().len(), 1);
    let [diagnostic] = report.diagnostics() else {
        return;
    };
    assert_eq!(diagnostic.code(), DiagnosticCode::DomainIncompatible);
    assert_eq!(
        diagnostic.message(),
        "the checked standard library does not provide a Boolean value type"
    );
    let literal_start = source.find("TRUE").unwrap();
    assert_eq!(diagnostic.location().span().start(), literal_start);
    assert_eq!(
        diagnostic.location().span().end(),
        literal_start + "TRUE".len()
    );
}

#[test]
fn rejects_qualified_client_boolean_literals_when_the_checked_standard_lacks_boolean() {
    let snapshot = verified_standard_library_for_relational_test();
    let standard = checked_standard_library_with_contract_overrides_for_test(
        &snapshot,
        &[(0, "orna.kernel.value.integer@1")],
    )
    .unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let cases = [
        (
            "std.BOOLEAN",
            "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS std.BOOLEAN RETURN TRUE;",
        ),
        (
            "std.types.BOOLEAN",
            "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS std.types.BOOLEAN RETURN TRUE;",
        ),
        (
            "\"std\".\"boolean\"",
            "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS \"std\".\"boolean\" RETURN TRUE;",
        ),
        (
            "\"std\".\"types\".\"boolean\"",
            "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS \"std\".\"types\".\"boolean\" RETURN TRUE;",
        ),
    ];

    for (spelling, source) in cases {
        let report = check_standard_application(&bundle([("application.orna", source)]), &context);

        assert!(report.checked_bundle().is_none(), "spelling: {spelling}");
        assert_eq!(report.diagnostics().len(), 1, "spelling: {spelling}");
        let [diagnostic] = report.diagnostics() else {
            return;
        };
        assert_eq!(
            diagnostic.code(),
            DiagnosticCode::DomainIncompatible,
            "spelling: {spelling}"
        );
        assert_eq!(
            diagnostic.message(),
            "the checked standard library does not provide a Boolean value type",
            "spelling: {spelling}"
        );
        let literal_start = source.find("TRUE").unwrap();
        assert_eq!(
            diagnostic.location().span().start(),
            literal_start,
            "spelling: {spelling}"
        );
        assert_eq!(
            diagnostic.location().span().end(),
            literal_start + "TRUE".len(),
            "spelling: {spelling}"
        );
    }
}

#[test]
fn rejects_a_standard_query_equality_before_both_boolean_literals_when_boolean_is_missing() {
    let snapshot = verified_standard_library_for_relational_test();
    let standard = checked_standard_library_with_contract_overrides_for_test(
        &snapshot,
        &[(0, "orna.kernel.value.integer@1")],
    )
    .unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source = "CREATE SCHEMA app;\
            CREATE TYPE app.task AS OBJECT ();\
            CREATE SERVER FUNCTION app.matches() RETURNS ROWS (matches BOOLEAN) \
            AS SELECT TRUE = FALSE FROM app.task t;";
    let report = check_standard_application(&bundle([("application.orna", source)]), &context);

    assert!(report.checked_bundle().is_none());
    assert_eq!(report.diagnostics().len(), 3);
    let [parent, left, right] = report.diagnostics() else {
        return;
    };
    let expected = [
        ("TRUE", source.find("TRUE").unwrap()),
        ("TRUE = FALSE", source.find("TRUE = FALSE").unwrap()),
        ("FALSE", source.find("FALSE").unwrap()),
    ];
    for (diagnostic, (text, start)) in [parent, left, right].into_iter().zip(expected) {
        assert_eq!(diagnostic.code(), DiagnosticCode::DomainIncompatible);
        assert_eq!(
            diagnostic.message(),
            "the checked standard library does not provide a Boolean value type"
        );
        assert_eq!(diagnostic.location().span().start(), start);
        assert_eq!(diagnostic.location().span().end(), start + text.len());
    }
}

#[test]
fn rejects_an_identity_selected_query_before_its_missing_boolean_selector_result() {
    let snapshot = verified_standard_library_for_relational_test();
    let standard = checked_standard_library_with_contract_overrides_for_test(
        &snapshot,
        &[(0, "orna.kernel.value.integer@1")],
    )
    .unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source = "CREATE SCHEMA app;\
            CREATE TYPE app.task AS OBJECT ();\
            CREATE SERVER FUNCTION app.matches(p_task REF app.task) RETURNS ROWS (matches BOOLEAN) \
            TRANSACTION READ ONLY VOLATILITY STABLE \
            AS SELECT TRUE FROM app.task t WHERE REF(t) = p_task;";
    let report = check_standard_application(&bundle([("application.orna", source)]), &context);

    assert!(report.checked_bundle().is_none());
    assert_eq!(report.diagnostics().len(), 2);
    let [projection, selector] = report.diagnostics() else {
        return;
    };
    let expected = [
        ("TRUE", source.find("TRUE").unwrap()),
        ("REF(t) = p_task", source.find("REF(t) = p_task").unwrap()),
    ];
    for (diagnostic, (text, start)) in [projection, selector].into_iter().zip(expected) {
        assert_eq!(diagnostic.code(), DiagnosticCode::DomainIncompatible);
        assert_eq!(
            diagnostic.message(),
            "the checked standard library does not provide a Boolean value type"
        );
        assert_eq!(diagnostic.location().span().start(), start);
        assert_eq!(diagnostic.location().span().end(), start + text.len());
    }
}

#[test]
fn records_standard_relational_body_uses_in_all_three_query_families() {
    let standard =
        check_standard_library_source(&verified_standard_library_for_relational_test()).unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source = "CREATE SCHEMA app;\
            CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL, other BOOLEAN NOT NULL);\
            CREATE SERVER FUNCTION app.ordinary() RETURNS ROWS (done BOOLEAN, task REF app.task) \
            AS SELECT t.done, REF(t) FROM app.task t WHERE t.done = TRUE ORDER BY t.done;\
            CREATE SERVER FUNCTION app.distinct() RETURNS ROWS (done BOOLEAN) \
            TRANSACTION READ ONLY VOLATILITY STABLE \
            AS SELECT DISTINCT t.done FROM app.task t WHERE t.done;\
            CREATE SERVER FUNCTION app.by_ref(p_task REF app.task) RETURNS ROWS (done BOOLEAN) \
            TRANSACTION READ ONLY VOLATILITY STABLE \
            AS SELECT t.done FROM app.task t WHERE REF(t) = p_task;";
    let report = check_standard_application(&bundle([("application.orna", source)]), &context);

    assert_eq!(report.diagnostics(), &[]);
    let checked = report.checked_bundle().unwrap();
    assert!(checked.uses().windows(2).all(|pair| {
        let first = pair[0].location().span();
        let second = pair[1].location().span();
        (first.start(), first.end()) <= (second.start(), second.end())
    }));
    let functions = checked.server_functions().collect::<Vec<_>>();
    let [ordinary, distinct, by_ref] = functions.as_slice() else {
        assert_eq!(checked.server_functions().count(), 3);
        return;
    };
    let object = checked.object_types().next().unwrap();

    let body_uses = |owner| {
        checked
            .uses()
            .iter()
            .filter(|type_use| {
                matches!(
                    type_use.kind(),
                    CheckedTypeUseKind::Expression { owner: candidate, .. }
                        | CheckedTypeUseKind::Result { owner: candidate, .. }
                        if candidate == owner
                )
            })
            .collect::<Vec<_>>()
    };
    let boolean = TypeId::from_bytes([3; 16]);

    let ordinary_uses = body_uses(ordinary.id());
    assert_eq!(ordinary_uses.len(), 8);
    let ordinary_projection = expression_use(&ordinary_uses, 0);
    let ordinary_result = result_use(&ordinary_uses, 0);
    let ordinary_reference = expression_use(&ordinary_uses, 1);
    let ordinary_reference_result = result_use(&ordinary_uses, 1);
    let ordinary_equality = expression_use(&ordinary_uses, 2);
    let ordinary_left = expression_use(&ordinary_uses, 3);
    let ordinary_literal = expression_use(&ordinary_uses, 4);
    let ordinary_ordering = expression_use(&ordinary_uses, 5);
    let distinct_start = source.find("CREATE SERVER FUNCTION app.distinct").unwrap();
    let ordinary_done = source
        .match_indices("t.done")
        .filter(|(start, _)| *start < distinct_start)
        .collect::<Vec<_>>();
    assert_eq!(ordinary_done.len(), 3);
    assert_type_use_span(ordinary_projection, ordinary_done[0].0, "t.done");
    assert_type_use_span(ordinary_result, ordinary_done[0].0, "t.done");
    assert_type_use_span(ordinary_equality, ordinary_done[1].0, "t.done = TRUE");
    assert_type_use_span(ordinary_left, ordinary_done[1].0, "t.done");
    assert_type_use_span(ordinary_literal, source.find("TRUE").unwrap(), "TRUE");
    assert_type_use_span(ordinary_ordering, ordinary_done[2].0, "t.done");
    let ordinary_reference_start = source
        .match_indices("REF(t)")
        .find(|(start, _)| *start < distinct_start)
        .map(|(start, _)| start);
    assert!(ordinary_reference_start.is_some());
    let Some(ordinary_reference_start) = ordinary_reference_start else {
        return;
    };
    assert_type_use_span(ordinary_reference, ordinary_reference_start, "REF(t)");
    assert_type_use_span(
        ordinary_reference_result,
        ordinary_reference_start,
        "REF(t)",
    );
    for type_use in [
        ordinary_projection,
        ordinary_result,
        ordinary_equality,
        ordinary_left,
        ordinary_literal,
        ordinary_ordering,
    ] {
        assert_eq!(
            type_use.value().map(CheckedValueTypeUse::type_id),
            Some(boolean)
        );
    }
    for type_use in [ordinary_reference, ordinary_reference_result] {
        assert_eq!(
            type_use
                .object_reference()
                .map(|reference| reference.target()),
            Some(object.id())
        );
    }
    assert!(
        checked_use_index(
            checked.uses(),
            ordinary_projection.kind(),
            ordinary_done[0].0,
            ordinary_done[0].0 + "t.done".len(),
        ) < checked_use_index(
            checked.uses(),
            ordinary_result.kind(),
            ordinary_done[0].0,
            ordinary_done[0].0 + "t.done".len(),
        )
    );
    assert!(
        checked_use_index(
            checked.uses(),
            ordinary_reference.kind(),
            ordinary_reference_start,
            ordinary_reference_start + "REF(t)".len(),
        ) < checked_use_index(
            checked.uses(),
            ordinary_reference_result.kind(),
            ordinary_reference_start,
            ordinary_reference_start + "REF(t)".len(),
        )
    );

    let distinct_uses = body_uses(distinct.id());
    assert_eq!(distinct_uses.len(), 3);
    let distinct_projection = expression_use(&distinct_uses, 0);
    let distinct_result = result_use(&distinct_uses, 0);
    let distinct_predicate = expression_use(&distinct_uses, 1);
    for type_use in [distinct_projection, distinct_result, distinct_predicate] {
        assert_eq!(
            type_use.value().map(CheckedValueTypeUse::type_id),
            Some(boolean)
        );
    }
    let identity_start = source.find("CREATE SERVER FUNCTION app.by_ref").unwrap();
    let distinct_done = source
        .match_indices("t.done")
        .filter(|(start, _)| *start > distinct.location().span().start() && *start < identity_start)
        .collect::<Vec<_>>();
    assert_eq!(distinct_done.len(), 2);
    assert_type_use_span(distinct_projection, distinct_done[0].0, "t.done");
    assert_type_use_span(distinct_result, distinct_done[0].0, "t.done");
    assert_type_use_span(distinct_predicate, distinct_done[1].0, "t.done");
    assert!(
        checked_use_index(
            checked.uses(),
            distinct_projection.kind(),
            distinct_done[0].0,
            distinct_done[0].0 + "t.done".len(),
        ) < checked_use_index(
            checked.uses(),
            distinct_result.kind(),
            distinct_done[0].0,
            distinct_done[0].0 + "t.done".len(),
        )
    );

    let selector_uses = body_uses(by_ref.id());
    assert_eq!(selector_uses.len(), 5);
    let selector_projection = expression_use(&selector_uses, 0);
    let selector_result = result_use(&selector_uses, 0);
    let selector_equality = expression_use(&selector_uses, 1);
    let selector_left = expression_use(&selector_uses, 2);
    let selector_right = expression_use(&selector_uses, 3);
    for type_use in [selector_projection, selector_result, selector_equality] {
        assert_eq!(
            type_use.value().map(CheckedValueTypeUse::type_id),
            Some(boolean)
        );
    }
    assert_eq!(
        selector_left
            .object_reference()
            .map(|reference| reference.target()),
        Some(object.id())
    );
    assert_eq!(
        selector_right
            .object_reference()
            .map(|reference| reference.target()),
        Some(object.id())
    );
    let selector_done = source
        .match_indices("t.done")
        .find(|(start, _)| *start > identity_start)
        .map(|(start, _)| start);
    assert!(selector_done.is_some());
    let Some(selector_done) = selector_done else {
        return;
    };
    let selector_equality_start = source.find("REF(t) = p_task").unwrap();
    let selector_left_start = source
        .match_indices("REF(t)")
        .find(|(start, _)| *start > identity_start)
        .map(|(start, _)| start);
    assert!(selector_left_start.is_some());
    let Some(selector_left_start) = selector_left_start else {
        return;
    };
    let selector_right_start = source.rfind("p_task").unwrap();
    assert_type_use_span(selector_projection, selector_done, "t.done");
    assert_type_use_span(selector_result, selector_done, "t.done");
    assert_type_use_span(
        selector_equality,
        selector_equality_start,
        "REF(t) = p_task",
    );
    assert_type_use_span(selector_left, selector_left_start, "REF(t)");
    assert_type_use_span(selector_right, selector_right_start, "p_task");
    assert!(
        checked_use_index(
            checked.uses(),
            selector_projection.kind(),
            selector_done,
            selector_done + "t.done".len(),
        ) < checked_use_index(
            checked.uses(),
            selector_result.kind(),
            selector_done,
            selector_done + "t.done".len(),
        )
    );
    assert!(selector_uses.iter().all(|type_use| {
        !matches!(
            type_use.kind(),
            CheckedTypeUseKind::Result { ordinal: 1, .. }
        )
    }));
}

#[test]
fn retains_a_non_golden_checked_boolean_id_through_relational_and_client_bodies() {
    let changed_boolean = TypeId::from_bytes([0x53; 16]);
    let snapshot = verified_standard_library_for_relational_test_with_boolean_id(
        changed_boolean,
        [
            0xa2, 0x5b, 0xcf, 0x20, 0x76, 0x46, 0x26, 0xdf, 0xe3, 0x77, 0x67, 0xca, 0x79, 0xc9,
            0x3e, 0x5f, 0xdc, 0x53, 0x8c, 0xc0, 0x7b, 0x74, 0xce, 0xac, 0x54, 0x2d, 0xb9, 0x31,
            0x3c, 0x56, 0xe1, 0x82,
        ],
    );
    let standard = check_standard_library_source(&snapshot).unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source = "CREATE SCHEMA app;\
            CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL);\
            CREATE SERVER FUNCTION app.matches() RETURNS ROWS (matches BOOLEAN) \
            AS SELECT t.done = TRUE FROM app.task t;\
            CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;";
    let report = check_standard_application(&bundle([("application.orna", source)]), &context);

    assert_eq!(report.diagnostics(), &[]);
    let checked = report.checked_bundle().unwrap();
    let servers = checked.server_functions().collect::<Vec<_>>();
    let clients = checked.client_functions().collect::<Vec<_>>();
    let [server] = servers.as_slice() else {
        assert_eq!(servers.len(), 1);
        return;
    };
    let [client] = clients.as_slice() else {
        assert_eq!(clients.len(), 1);
        return;
    };
    let server_body = checked
        .uses()
        .iter()
        .filter(|type_use| {
            matches!(
                type_use.kind(),
                CheckedTypeUseKind::Expression { owner, .. }
                    | CheckedTypeUseKind::Result { owner, .. }
                    if owner == server.id()
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(server_body.len(), 4);
    for type_use in [
        expression_use(&server_body, 0),
        expression_use(&server_body, 1),
        expression_use(&server_body, 2),
        result_use(&server_body, 0),
    ] {
        assert_eq!(
            type_use.value().map(CheckedValueTypeUse::type_id),
            Some(changed_boolean)
        );
    }
    let equality_start = source.find("t.done = TRUE").unwrap();
    assert_eq!(
        expression_use(&server_body, 0).location().span().start(),
        equality_start
    );
    assert_eq!(
        expression_use(&server_body, 0).location().span().end(),
        equality_start + "t.done = TRUE".len()
    );

    let client_body = checked
        .uses()
        .iter()
        .filter(|type_use| {
            matches!(
                type_use.kind(),
                CheckedTypeUseKind::Expression { owner, .. }
                    | CheckedTypeUseKind::Result { owner, .. }
                    if owner == client.id()
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(client_body.len(), 2);
    for type_use in [expression_use(&client_body, 0), result_use(&client_body, 0)] {
        assert_eq!(
            type_use.value().map(CheckedValueTypeUse::type_id),
            Some(changed_boolean)
        );
    }
}

#[test]
fn records_standard_mutation_body_uses_in_committed_traversal_order() {
    let standard =
        check_standard_library_source(&verified_standard_library_for_relational_test()).unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source = "CREATE SCHEMA app;\
            CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL, note BOOLEAN, parent REF app.task);\
            CREATE SERVER FUNCTION app.create(p_done BOOLEAN) RETURNS ROWS (created REF app.task) \
            TRANSACTION ATOMIC AS INSERT INTO app.task AS made (done, note) VALUES (p_done, NULL) RETURNING REF(made);\
            CREATE SERVER FUNCTION app.change(p_task REF app.task, p_done BOOLEAN) RETURNS ROWS (changed REF app.task) \
            TRANSACTION ATOMIC AS UPDATE app.task AS changed SET done = p_done, note = NULL WHERE REF(changed) = p_task RETURNING REF(changed);\
            CREATE SERVER FUNCTION app.remove(p_task REF app.task) RETURNS ROWS (deleted BOOLEAN) \
            TRANSACTION ATOMIC AS DELETE FROM app.task AS deleted WHERE REF(deleted) = p_task RETURNING TRUE;";
    let report = check_standard_application(&bundle([("application.orna", source)]), &context);

    assert_eq!(report.diagnostics(), &[]);
    let checked_bundle = report.checked_bundle();
    assert!(
        checked_bundle.is_some(),
        "a diagnostic-free standard application report must contain a checked bundle"
    );
    let Some(checked) = checked_bundle else {
        return;
    };
    let functions = checked.server_functions().collect::<Vec<_>>();
    assert_eq!(functions.len(), 3);
    let [insert, update, delete] = functions.as_slice() else {
        return;
    };
    let boolean = TypeId::from_bytes([3; 16]);
    let task = checked.object_types().next().map(|object| object.id());
    assert!(task.is_some());
    let Some(task) = task else {
        return;
    };

    let insert_uses = checked
        .uses()
        .iter()
        .filter(|type_use| {
            matches!(
                type_use.kind(),
                CheckedTypeUseKind::Expression { owner, .. }
                    | CheckedTypeUseKind::Result { owner, .. }
                    if owner == insert.id()
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(insert_uses.len(), 4);
    assert_eq!(
        insert_uses
            .iter()
            .map(|type_use| type_use.kind())
            .collect::<Vec<_>>(),
        vec![
            CheckedTypeUseKind::Expression {
                owner: insert.id(),
                ordinal: 0,
            },
            CheckedTypeUseKind::Expression {
                owner: insert.id(),
                ordinal: 1,
            },
            CheckedTypeUseKind::Expression {
                owner: insert.id(),
                ordinal: 2,
            },
            CheckedTypeUseKind::Result {
                owner: insert.id(),
                ordinal: 0,
            },
        ]
    );
    assert_eq!(
        insert_uses[0].value().map(CheckedValueTypeUse::type_id),
        Some(boolean)
    );
    assert_eq!(
        insert_uses[1].value().map(CheckedValueTypeUse::type_id),
        Some(boolean)
    );
    assert_eq!(
        insert_uses[2]
            .object_reference()
            .map(|value| value.target()),
        Some(task)
    );
    assert_eq!(
        insert_uses[3]
            .object_reference()
            .map(|value| value.target()),
        Some(task)
    );
    assert_type_use_span(
        insert_uses[0],
        source.find("p_done, NULL) RETURNING").unwrap(),
        "p_done",
    );
    assert_type_use_span(
        insert_uses[1],
        source.find("NULL) RETURNING").unwrap(),
        "NULL",
    );
    let insert_returning = source.find("REF(made)").unwrap();
    assert_type_use_span(insert_uses[2], insert_returning, "REF(made)");
    assert_type_use_span(insert_uses[3], insert_returning, "REF(made)");

    let update_uses = checked
        .uses()
        .iter()
        .filter(|type_use| {
            matches!(
                type_use.kind(),
                CheckedTypeUseKind::Expression { owner, .. }
                    | CheckedTypeUseKind::Result { owner, .. }
                    if owner == update.id()
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(update_uses.len(), 7);
    assert_eq!(
        update_uses
            .iter()
            .map(|type_use| type_use.kind())
            .collect::<Vec<_>>(),
        vec![
            CheckedTypeUseKind::Expression {
                owner: update.id(),
                ordinal: 0,
            },
            CheckedTypeUseKind::Expression {
                owner: update.id(),
                ordinal: 1,
            },
            CheckedTypeUseKind::Expression {
                owner: update.id(),
                ordinal: 3,
            },
            CheckedTypeUseKind::Expression {
                owner: update.id(),
                ordinal: 2,
            },
            CheckedTypeUseKind::Expression {
                owner: update.id(),
                ordinal: 4,
            },
            CheckedTypeUseKind::Expression {
                owner: update.id(),
                ordinal: 5,
            },
            CheckedTypeUseKind::Result {
                owner: update.id(),
                ordinal: 0,
            },
        ]
    );
    assert_eq!(
        update_uses[0].value().map(CheckedValueTypeUse::type_id),
        Some(boolean)
    );
    assert_eq!(
        update_uses[1].value().map(CheckedValueTypeUse::type_id),
        Some(boolean)
    );
    assert_eq!(
        update_uses[3].value().map(CheckedValueTypeUse::type_id),
        Some(boolean)
    );
    for type_use in [
        &update_uses[2],
        &update_uses[4],
        &update_uses[5],
        &update_uses[6],
    ] {
        assert_eq!(
            type_use.object_reference().map(|value| value.target()),
            Some(task)
        );
    }
    let update_assignment = source.find("done = p_done, note").unwrap() + "done = ".len();
    let update_null = source.find("note = NULL WHERE").unwrap() + "note = ".len();
    let update_selector = source.find("REF(changed) = p_task").unwrap();
    let update_left = source.find("REF(changed)").unwrap();
    let update_right = source.find("p_task RETURNING REF(changed)").unwrap();
    let update_returning = source.rfind("REF(changed)").unwrap();
    assert_type_use_span(update_uses[0], update_assignment, "p_done");
    assert_type_use_span(update_uses[1], update_null, "NULL");
    assert_type_use_span(update_uses[3], update_selector, "REF(changed) = p_task");
    assert_type_use_span(update_uses[2], update_left, "REF(changed)");
    assert_type_use_span(update_uses[4], update_right, "p_task");
    assert_type_use_span(update_uses[5], update_returning, "REF(changed)");
    assert_type_use_span(update_uses[6], update_returning, "REF(changed)");

    let delete_uses = checked
        .uses()
        .iter()
        .filter(|type_use| {
            matches!(
                type_use.kind(),
                CheckedTypeUseKind::Expression { owner, .. }
                    | CheckedTypeUseKind::Result { owner, .. }
                    if owner == delete.id()
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(delete_uses.len(), 5);
    assert_eq!(
        delete_uses
            .iter()
            .map(|type_use| type_use.kind())
            .collect::<Vec<_>>(),
        vec![
            CheckedTypeUseKind::Expression {
                owner: delete.id(),
                ordinal: 1,
            },
            CheckedTypeUseKind::Expression {
                owner: delete.id(),
                ordinal: 0,
            },
            CheckedTypeUseKind::Expression {
                owner: delete.id(),
                ordinal: 2,
            },
            CheckedTypeUseKind::Expression {
                owner: delete.id(),
                ordinal: 3,
            },
            CheckedTypeUseKind::Result {
                owner: delete.id(),
                ordinal: 0,
            },
        ]
    );
    assert_eq!(
        delete_uses[1].value().map(CheckedValueTypeUse::type_id),
        Some(boolean)
    );
    assert_eq!(
        delete_uses[3].value().map(CheckedValueTypeUse::type_id),
        Some(boolean)
    );
    assert_eq!(
        delete_uses[4].value().map(CheckedValueTypeUse::type_id),
        Some(boolean)
    );
    for type_use in [&delete_uses[0], &delete_uses[2]] {
        assert_eq!(
            type_use.object_reference().map(|value| value.target()),
            Some(task)
        );
    }
    let delete_selector = source.find("REF(deleted) = p_task").unwrap();
    let delete_left = source.find("REF(deleted)").unwrap();
    let delete_right = source.find("p_task RETURNING TRUE").unwrap();
    let delete_true = source.rfind("TRUE").unwrap();
    assert_type_use_span(delete_uses[1], delete_selector, "REF(deleted) = p_task");
    assert_type_use_span(delete_uses[0], delete_left, "REF(deleted)");
    assert_type_use_span(delete_uses[2], delete_right, "p_task");
    assert_type_use_span(delete_uses[3], delete_true, "TRUE");
    assert_type_use_span(delete_uses[4], delete_true, "TRUE");
}

#[test]
fn missing_standard_boolean_rejects_insert_and_update_before_any_checked_bundle() {
    let snapshot = verified_standard_library_for_relational_test();
    let standard = checked_standard_library_with_contract_overrides_for_test(
        &snapshot,
        &[(0, "orna.kernel.value.integer@1")],
    )
    .unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source = "CREATE SCHEMA app;\
            CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL);\
            CREATE SERVER FUNCTION app.create() RETURNS ROWS (created REF app.task) \
            TRANSACTION ATOMIC AS INSERT INTO app.task AS made (done) VALUES (TRUE) RETURNING REF(made);\
            CREATE SERVER FUNCTION app.change(p_task REF app.task) RETURNS ROWS (changed REF app.task) \
            TRANSACTION ATOMIC AS UPDATE app.task AS changed SET done = TRUE, other = FALSE \
            WHERE REF(changed) = p_task RETURNING REF(changed);";
    let report = check_standard_application(&bundle([("application.orna", source)]), &context);

    assert!(report.checked_bundle().is_none());
    let insert_true = source.find("VALUES (TRUE)").unwrap() + "VALUES (".len();
    let update_first = source.find("done = TRUE").unwrap() + "done = ".len();
    let update_second = source.find("other = FALSE").unwrap() + "other = ".len();
    let selector = source.find("REF(changed) = p_task").unwrap();
    assert_eq!(
        report
            .diagnostics()
            .iter()
            .map(|diagnostic| {
                (
                    diagnostic.code(),
                    diagnostic.message(),
                    diagnostic.location().span().start(),
                    diagnostic.location().span().end(),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (
                DiagnosticCode::DomainIncompatible,
                "the checked standard library does not provide a Boolean value type",
                insert_true,
                insert_true + "TRUE".len(),
            ),
            (
                DiagnosticCode::DomainIncompatible,
                "the checked standard library does not provide a Boolean value type",
                update_first,
                update_first + "TRUE".len(),
            ),
            (
                DiagnosticCode::DomainIncompatible,
                "the checked standard library does not provide a Boolean value type",
                update_second,
                update_second + "FALSE".len(),
            ),
            (
                DiagnosticCode::DomainIncompatible,
                "the checked standard library does not provide a Boolean value type",
                selector,
                selector + "REF(changed) = p_task".len(),
            ),
        ]
    );
}

#[test]
fn missing_standard_boolean_rejects_delete_before_return_column_compatibility() {
    let snapshot = verified_standard_library_for_relational_test();
    let standard = checked_standard_library_with_contract_overrides_for_test(
        &snapshot,
        &[(0, "orna.kernel.value.integer@1")],
    )
    .unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source = "CREATE SCHEMA app;\
            CREATE TYPE app.task AS OBJECT ();\
            CREATE SERVER FUNCTION app.remove(p_task REF app.task) RETURNS ROWS (deleted BOOLEAN) \
            TRANSACTION ATOMIC AS DELETE FROM app.task AS deleted \
            WHERE REF(deleted) = p_task RETURNING TRUE;";
    let report = check_standard_application(&bundle([("application.orna", source)]), &context);

    assert!(report.checked_bundle().is_none());
    let selector = source.find("REF(deleted) = p_task").unwrap();
    let returned_true = source.rfind("TRUE").unwrap();
    assert_eq!(
        report
            .diagnostics()
            .iter()
            .map(|diagnostic| {
                (
                    diagnostic.code(),
                    diagnostic.message(),
                    diagnostic.location().span().start(),
                    diagnostic.location().span().end(),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (
                DiagnosticCode::DomainIncompatible,
                "the checked standard library does not provide a Boolean value type",
                selector,
                selector + "REF(deleted) = p_task".len(),
            ),
            (
                DiagnosticCode::DomainIncompatible,
                "the checked standard library does not provide a Boolean value type",
                returned_true,
                returned_true + "TRUE".len(),
            ),
        ]
    );
}

#[test]
fn retains_a_non_golden_boolean_identity_through_every_mutation_boolean_path() {
    let changed_boolean = TypeId::from_bytes([0x53; 16]);
    let snapshot = verified_standard_library_for_relational_test_with_boolean_id(
        changed_boolean,
        [
            0xa2, 0x5b, 0xcf, 0x20, 0x76, 0x46, 0x26, 0xdf, 0xe3, 0x77, 0x67, 0xca, 0x79, 0xc9,
            0x3e, 0x5f, 0xdc, 0x53, 0x8c, 0xc0, 0x7b, 0x74, 0xce, 0xac, 0x54, 0x2d, 0xb9, 0x31,
            0x3c, 0x56, 0xe1, 0x82,
        ],
    );
    let standard = check_standard_library_source(&snapshot).unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source = "CREATE SCHEMA app;\
            CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL, other BOOLEAN NOT NULL);\
            CREATE SERVER FUNCTION app.create(p_done BOOLEAN) RETURNS ROWS (created REF app.task) \
            TRANSACTION ATOMIC AS INSERT INTO app.task AS made (done, other) VALUES (p_done, TRUE) RETURNING REF(made);\
            CREATE SERVER FUNCTION app.change(p_task REF app.task, p_done BOOLEAN) RETURNS ROWS (changed REF app.task) \
            TRANSACTION ATOMIC AS UPDATE app.task AS changed SET done = p_done, other = TRUE \
            WHERE REF(changed) = p_task RETURNING REF(changed);\
            CREATE SERVER FUNCTION app.remove(p_task REF app.task) RETURNS ROWS (deleted BOOLEAN) \
            TRANSACTION ATOMIC AS DELETE FROM app.task AS deleted WHERE REF(deleted) = p_task RETURNING TRUE;";
    let report = check_standard_application(&bundle([("application.orna", source)]), &context);

    assert_eq!(report.diagnostics(), &[]);
    let checked_bundle = report.checked_bundle();
    assert!(
        checked_bundle.is_some(),
        "a diagnostic-free standard application report must contain a checked bundle"
    );
    let Some(checked) = checked_bundle else {
        return;
    };
    let functions = checked.server_functions().collect::<Vec<_>>();
    let [insert, update, delete] = functions.as_slice() else {
        assert_eq!(functions.len(), 3);
        return;
    };
    let body_uses = |owner| {
        checked
            .uses()
            .iter()
            .filter(|type_use| {
                matches!(
                    type_use.kind(),
                    CheckedTypeUseKind::Expression { owner: candidate, .. }
                        | CheckedTypeUseKind::Result { owner: candidate, .. }
                        if candidate == owner
                )
            })
            .collect::<Vec<_>>()
    };
    let insert_uses = body_uses(insert.id());
    let update_uses = body_uses(update.id());
    let delete_uses = body_uses(delete.id());
    let retained = [
        expression_use(&insert_uses, 0),
        expression_use(&insert_uses, 1),
        expression_use(&update_uses, 0),
        expression_use(&update_uses, 1),
        expression_use(&update_uses, 2),
        expression_use(&delete_uses, 0),
        expression_use(&delete_uses, 3),
        result_use(&delete_uses, 0),
    ];
    for type_use in retained {
        assert_eq!(
            type_use.value().map(CheckedValueTypeUse::type_id),
            Some(changed_boolean)
        );
    }
    let insert_parameter = source.find("p_done, TRUE)").unwrap();
    let insert_true = source.find("TRUE) RETURNING").unwrap();
    let update_parameter = source.find("done = p_done, other").unwrap() + "done = ".len();
    let update_true = source.find("other = TRUE WHERE").unwrap() + "other = ".len();
    let update_selector = source.find("REF(changed) = p_task").unwrap();
    let delete_selector = source.find("REF(deleted) = p_task").unwrap();
    let delete_true = source.rfind("TRUE").unwrap();
    assert_type_use_span(expression_use(&insert_uses, 0), insert_parameter, "p_done");
    assert_type_use_span(expression_use(&insert_uses, 1), insert_true, "TRUE");
    assert_type_use_span(expression_use(&update_uses, 0), update_parameter, "p_done");
    assert_type_use_span(expression_use(&update_uses, 1), update_true, "TRUE");
    assert_type_use_span(
        expression_use(&update_uses, 2),
        update_selector,
        "REF(changed) = p_task",
    );
    assert_type_use_span(
        expression_use(&delete_uses, 0),
        delete_selector,
        "REF(deleted) = p_task",
    );
    assert_type_use_span(expression_use(&delete_uses, 3), delete_true, "TRUE");
    assert_type_use_span(result_use(&delete_uses, 0), delete_true, "TRUE");
}

fn rebase_standard_origins_to_source(
    origins: &mut [DefinitionOrigin],
    parsed_unit: &ParsedSourceUnit,
) {
    assert_eq!(origins.len(), 5);
    assert_eq!(parsed_unit.parsed().schemas().len(), 2);
    assert_eq!(parsed_unit.parsed().primitive_value_types().len(), 1);
    assert_eq!(parsed_unit.parsed().type_exports().len(), 2);
    let identities = origins
        .iter()
        .map(DefinitionOrigin::identity)
        .collect::<Vec<_>>();
    origins[0] = parsed_origin(identities[0], &parsed_unit.parsed().schemas()[0].span);
    origins[1] = parsed_origin(identities[1], &parsed_unit.parsed().schemas()[1].span);
    origins[2] = parsed_origin(
        identities[2],
        &parsed_unit.parsed().primitive_value_types()[0].span,
    );
    origins[3] = parsed_origin(identities[3], &parsed_unit.parsed().type_exports()[0].span);
    origins[4] = parsed_origin(identities[4], &parsed_unit.parsed().type_exports()[1].span);
}

fn assert_standard_source_mismatch(source: &str) {
    let (stored_unit, parsed_unit, catalogue, origins) = standard_reconciliation_inputs(source);
    assert_eq!(
        reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins),
        Err(super::StandardLibraryCheckError::SourceMismatch)
    );
}

fn two_type_reconciliation_inputs(
    source: &str,
) -> (
    StoredSourceUnit,
    ParsedSourceUnit,
    CatalogueSnapshot,
    Vec<DefinitionOrigin>,
) {
    let stored_unit = StoredSourceUnit::new(
        STANDARD_SOURCE_UNIT_ID,
        0,
        "std/types.orna",
        source,
        source_unit_content_digest(source).unwrap(),
    )
    .unwrap();
    let parsed_unit = parsed_standard_unit(source);
    let boolean = ValueTypeDefinition::primitive(
        TypeId::from_bytes([3; 16]),
        QualifiedSemanticName::new(["std", "types", "boolean"]).unwrap(),
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Persistable,
        "boolean@1",
    );
    let integer = ValueTypeDefinition::primitive(
        TypeId::from_bytes([4; 16]),
        QualifiedSemanticName::new(["std", "types", "integer"]).unwrap(),
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Transient,
        "int@1",
    );
    let qualified_boolean = TypeBinding::qualified(
        QualifiedSemanticName::new(["std", "boolean"]).unwrap(),
        boolean.id(),
    )
    .unwrap();
    let qualified_integer = TypeBinding::qualified(
        QualifiedSemanticName::new(["std", "integer"]).unwrap(),
        integer.id(),
    )
    .unwrap();
    let prelude_boolean =
        TypeBinding::prelude(PreludeTypeName::new(["boolean"]).unwrap(), boolean.id()).unwrap();
    let prelude_integer =
        TypeBinding::prelude(PreludeTypeName::new(["integer"]).unwrap(), integer.id()).unwrap();
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([8; 16]),
        vec![
            SchemaDefinition::new(
                SchemaId::from_bytes([1; 16]),
                QualifiedSemanticName::new(["std"]).unwrap(),
            ),
            SchemaDefinition::new(
                SchemaId::from_bytes([2; 16]),
                QualifiedSemanticName::new(["std", "types"]).unwrap(),
            ),
        ],
        vec![],
        vec![boolean, integer],
        vec![
            prelude_boolean,
            qualified_integer,
            prelude_integer,
            qualified_boolean,
        ],
    )
    .unwrap();
    let canonical_unit = parsed_standard_unit(TWO_TYPE_STANDARD_SOURCE);
    let mut origins = Vec::new();
    for declaration in canonical_unit.parsed().schemas() {
        let name = QualifiedSemanticName::new(
            declaration
                .name
                .parts
                .iter()
                .map(|part| part.text.to_ascii_lowercase()),
        )
        .unwrap();
        let id = catalogue.schema_by_name(&name).unwrap().id();
        origins.push(parsed_origin(
            DefinitionIdentity::Schema(id),
            &declaration.span,
        ));
    }
    for declaration in canonical_unit.parsed().primitive_value_types() {
        let name = QualifiedSemanticName::new(
            declaration
                .name
                .parts
                .iter()
                .map(|part| part.text.to_ascii_lowercase()),
        )
        .unwrap();
        let id = catalogue.value_type_by_name(&name).unwrap().id();
        origins.push(parsed_origin(
            DefinitionIdentity::ValueType(id),
            &declaration.span,
        ));
    }
    for declaration in canonical_unit.parsed().type_exports() {
        let name = match &declaration.target {
            orna_syntax::TypeExportTarget::Qualified { name } => TypeLookupName::qualified(
                QualifiedSemanticName::new(
                    name.parts.iter().map(|part| part.text.to_ascii_lowercase()),
                )
                .unwrap(),
            ),
            orna_syntax::TypeExportTarget::Prelude { words, .. } => TypeLookupName::prelude(
                PreludeTypeName::new(words.iter().map(|word| word.text.as_str())).unwrap(),
            ),
        };
        let id = catalogue.type_binding_by_name(&name).unwrap().id();
        origins.push(parsed_origin(
            DefinitionIdentity::TypeBinding(id),
            &declaration.span,
        ));
    }
    origins.reverse();

    (stored_unit, parsed_unit, catalogue, origins)
}

fn parsed_standard_unit(source: &str) -> ParsedSourceUnit {
    let report =
        parse_bundle(&SourceBundle::new([SourceUnit::new("std/types.orna", source)]).unwrap());
    assert!(report.diagnostics().is_empty());
    report.units()[0].clone()
}

fn parsed_origin(identity: DefinitionIdentity, span: &SourceSpan) -> DefinitionOrigin {
    DefinitionOrigin::new(
        identity,
        SourceOrigin::new(
            STANDARD_SOURCE_UNIT_ID,
            u32::try_from(span.start).unwrap(),
            u32::try_from(span.end).unwrap(),
        )
        .unwrap(),
    )
}

#[test]
fn standard_reconciliation_accepts_reordered_declarations_and_catalogue_facts() {
    let (stored_unit, parsed_unit, catalogue, origins) =
        two_type_reconciliation_inputs(TWO_TYPE_STANDARD_SOURCE);

    let families =
        reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins).unwrap();
    let schemas = families.schemas;
    let value_types = families.value_types;
    let bindings = families.type_bindings;

    assert_eq!(schemas[0].name().to_string(), "std.types");
    assert_eq!(schemas[1].name().to_string(), "std");
    assert_eq!(value_types[0].name().to_string(), "std.types.integer");
    assert_eq!(value_types[1].name().to_string(), "std.types.boolean");
    assert_eq!(bindings[0].name().to_string(), "integer");
    assert_eq!(bindings[1].name().to_string(), "std.integer");
    assert_eq!(bindings[2].name().to_string(), "boolean");
    assert_eq!(bindings[3].name().to_string(), "std.boolean");
}

#[test]
fn standard_reconciliation_binds_one_exact_opaque_definition_and_origin() {
    let source = "CREATE SCHEMA std;CREATE TYPE std.TOKEN AS VALUE OPAQUE KERNEL CONTRACT 'std.token@1' IMMUTABLE TRANSIENT;";
    let (stored_unit, parsed_unit, catalogue, origins) = opaque_standard_reconciliation_inputs(
        source,
        QualifiedSemanticName::new(["std", "token"]).unwrap(),
        "std.token@1",
    );

    let families =
        reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins).unwrap();
    assert_eq!(families.value_types.len(), 1);
    let opaque = &families.value_types[0];
    assert_eq!(opaque.id(), TypeId::from_bytes([3; 16]));
    assert_eq!(opaque.name().to_string(), "std.token");
    assert_eq!(opaque.kind(), ValueTypeKind::Opaque);
    assert_eq!(opaque.mutability(), ValueTypeMutability::Immutable);
    assert_eq!(opaque.persistence(), ValueTypePersistence::Transient);
    assert_eq!(opaque.representation_contract(), "std.token@1");
    assert_eq!(opaque.origin(), origins[1].source());

    for contract in ["", &"a".repeat(129), "line\nbreak", "\u{7f}"] {
        assert!(!super::opaque_contract_is_valid(contract));
    }
    for contract in ["a", "std.token@1", "!~"] {
        assert!(super::opaque_contract_is_valid(contract));
    }
}

#[test]
fn standard_reconciliation_keeps_primitive_and_opaque_kinds_distinct() {
    let opaque_source = "CREATE SCHEMA std;CREATE TYPE std.TOKEN AS VALUE OPAQUE KERNEL CONTRACT 'std.token@1' IMMUTABLE TRANSIENT;";
    let (stored_unit, parsed_unit, mut catalogue, origins) = opaque_standard_reconciliation_inputs(
        opaque_source,
        QualifiedSemanticName::new(["std", "token"]).unwrap(),
        "std.token@1",
    );
    catalogue = CatalogueSnapshot::new_with_types(
        catalogue.revision(),
        catalogue.schemas().to_vec(),
        vec![],
        vec![ValueTypeDefinition::primitive(
            TypeId::from_bytes([3; 16]),
            QualifiedSemanticName::new(["std", "token"]).unwrap(),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Transient,
            "std.token@1",
        )],
        vec![],
    )
    .unwrap();

    assert_eq!(
        reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins),
        Err(super::StandardLibraryCheckError::SourceMismatch)
    );
}

#[test]
fn standard_reconciliation_rejects_crossed_and_duplicate_type_and_binding_facts() {
    let crossed_cases = [
            TWO_TYPE_STANDARD_SOURCE.replacen(
                "CREATE TYPE std.types.INTEGER AS VALUE PRIMITIVE KERNEL CONTRACT 'int@1' IMMUTABLE TRANSIENT;",
                "CREATE TYPE std.types.BOOLEAN AS VALUE PRIMITIVE KERNEL CONTRACT 'boolean@1' IMMUTABLE PERSISTABLE;",
                1,
            ),
            TWO_TYPE_STANDARD_SOURCE.replacen(
                "EXPORT TYPE std.types.INTEGER AS std.INTEGER;",
                "EXPORT TYPE std.types.BOOLEAN AS std.INTEGER;",
                1,
            ),
            TWO_TYPE_STANDARD_SOURCE.replacen(
                "EXPORT TYPE std.INTEGER TO PRELUDE AS INTEGER;",
                "EXPORT TYPE std.BOOLEAN TO PRELUDE AS INTEGER;",
                1,
            ),
        ];

    for source in crossed_cases {
        let (stored_unit, parsed_unit, catalogue, origins) =
            two_type_reconciliation_inputs(&source);
        assert_eq!(
            reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins),
            Err(super::StandardLibraryCheckError::SourceMismatch)
        );
    }

    let duplicate_primitive = TWO_TYPE_STANDARD_SOURCE.replacen(
            "CREATE TYPE std.types.INTEGER AS VALUE PRIMITIVE KERNEL CONTRACT 'int@1' IMMUTABLE TRANSIENT;",
            "CREATE TYPE std.types.BOOLEAN AS VALUE PRIMITIVE KERNEL CONTRACT 'boolean@1' IMMUTABLE PERSISTABLE;",
            1,
        );
    let (stored_unit, parsed_unit, catalogue, mut origins) =
        two_type_reconciliation_inputs(&duplicate_primitive);
    replace_origin(
        &mut origins,
        DefinitionIdentity::ValueType(TypeId::from_bytes([3; 16])),
        &parsed_unit.parsed().primitive_value_types()[0].span,
    );
    assert_eq!(
        reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins),
        Err(super::StandardLibraryCheckError::SourceMismatch)
    );

    let duplicate_qualified = TWO_TYPE_STANDARD_SOURCE.replacen(
        "EXPORT TYPE std.types.INTEGER AS std.INTEGER;",
        "EXPORT TYPE std.types.BOOLEAN AS std.BOOLEAN;",
        1,
    );
    let (stored_unit, parsed_unit, catalogue, mut origins) =
        two_type_reconciliation_inputs(&duplicate_qualified);
    let qualified_boolean = catalogue
        .type_binding_by_name(&TypeLookupName::qualified(
            QualifiedSemanticName::new(["std", "boolean"]).unwrap(),
        ))
        .unwrap()
        .id();
    let first_qualified = parsed_unit
        .parsed()
        .type_exports()
        .iter()
        .find(|declaration| {
            matches!(
                &declaration.target,
                orna_syntax::TypeExportTarget::Qualified { .. }
            )
        })
        .unwrap();
    replace_origin(
        &mut origins,
        DefinitionIdentity::TypeBinding(qualified_boolean),
        &first_qualified.span,
    );
    assert_eq!(
        reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins),
        Err(super::StandardLibraryCheckError::SourceMismatch)
    );

    let duplicate_prelude = TWO_TYPE_STANDARD_SOURCE.replacen(
        "EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;",
        "EXPORT TYPE std.INTEGER TO PRELUDE AS INTEGER;",
        1,
    );
    let (stored_unit, parsed_unit, catalogue, mut origins) =
        two_type_reconciliation_inputs(&duplicate_prelude);
    let prelude_integer = catalogue
        .type_binding_by_name(&TypeLookupName::prelude(
            PreludeTypeName::new(["integer"]).unwrap(),
        ))
        .unwrap()
        .id();
    let first_prelude = parsed_unit
        .parsed()
        .type_exports()
        .iter()
        .find(|declaration| {
            matches!(
                &declaration.target,
                orna_syntax::TypeExportTarget::Prelude { .. }
            )
        })
        .unwrap();
    replace_origin(
        &mut origins,
        DefinitionIdentity::TypeBinding(prelude_integer),
        &first_prelude.span,
    );
    assert_eq!(
        reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins),
        Err(super::StandardLibraryCheckError::SourceMismatch)
    );
}

fn replace_origin(
    origins: &mut [DefinitionOrigin],
    identity: DefinitionIdentity,
    span: &SourceSpan,
) {
    let origin = origins
        .iter_mut()
        .find(|origin| origin.identity() == identity)
        .unwrap();
    *origin = parsed_origin(identity, span);
}

#[test]
fn standard_reconciliation_rejects_missing_and_unsupported_declarations() {
    assert_standard_source_mismatch(
        "CREATE SCHEMA std;CREATE SCHEMA std.types;CREATE TYPE std.types.BOOLEAN AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.boolean@1' IMMUTABLE PERSISTABLE;EXPORT TYPE std.types.BOOLEAN AS std.BOOLEAN;",
    );
    assert_standard_source_mismatch(
        "CREATE SCHEMA std;CREATE SCHEMA std.types;CREATE TYPE std.types.BOOLEAN AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.boolean@1' IMMUTABLE PERSISTABLE;EXPORT TYPE std.types.BOOLEAN AS std.BOOLEAN;EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;CREATE TYPE std.extra AS OBJECT ();",
    );
}

#[test]
fn standard_reconciliation_rejects_duplicate_and_crossed_source_facts() {
    assert_standard_source_mismatch(
        "CREATE SCHEMA std;CREATE SCHEMA std;CREATE TYPE std.types.BOOLEAN AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.boolean@1' IMMUTABLE PERSISTABLE;EXPORT TYPE std.types.BOOLEAN AS std.BOOLEAN;EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;",
    );
    assert_standard_source_mismatch(
        "CREATE SCHEMA std;CREATE SCHEMA std.types;CREATE TYPE std.types.BOOLEAN AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.boolean@1' IMMUTABLE PERSISTABLE;EXPORT TYPE std.types.BOOLEAN AS std.BOOLEAN;EXPORT TYPE std.types.BOOLEAN TO PRELUDE AS BOOLEAN;",
    );
}

#[test]
fn standard_reconciliation_rejects_quoted_and_changed_primitive_facts() {
    let cases = [
        STANDARD_SOURCE.replacen("CREATE SCHEMA std;", "CREATE SCHEMA \"std\";", 1),
        STANDARD_SOURCE.replacen(
            "CREATE TYPE std.types.BOOLEAN",
            "CREATE TYPE \"std\".types.BOOLEAN",
            1,
        ),
        STANDARD_SOURCE.replacen("AS std.BOOLEAN", "AS \"std\".BOOLEAN", 1),
        STANDARD_SOURCE.replacen(
            "EXPORT TYPE std.types.BOOLEAN",
            "EXPORT TYPE \"std\".types.BOOLEAN",
            1,
        ),
        STANDARD_SOURCE.replacen(
            "EXPORT TYPE std.BOOLEAN TO PRELUDE",
            "EXPORT TYPE \"std\".BOOLEAN TO PRELUDE",
            1,
        ),
        STANDARD_SOURCE.replacen("boolean@1", "boolean@2", 1),
        STANDARD_SOURCE.replacen("PERSISTABLE", "TRANSIENT", 1),
    ];

    for source in cases {
        let (stored_unit, parsed_unit, catalogue, mut origins) =
            standard_reconciliation_inputs(&source);
        rebase_standard_origins_to_source(&mut origins, &parsed_unit);
        assert_eq!(
            reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins),
            Err(super::StandardLibraryCheckError::SourceMismatch)
        );
    }
}

#[test]
fn quoted_prelude_words_are_rejected_by_the_parse_gate() {
    let report = parse_bundle(
        &SourceBundle::new([SourceUnit::new(
            "std/types.orna",
            "EXPORT TYPE std.BOOLEAN TO PRELUDE AS \"BOOLEAN\";",
        )])
        .unwrap(),
    );

    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::UnexpectedToken
    );
    assert_eq!(
        report.diagnostics()[0].message(),
        "expected an unquoted prelude type name after AS"
    );
}

#[test]
fn standard_reconciliation_rejects_every_missing_or_extra_supported_family() {
    let cases = [
            "CREATE SCHEMA std.types;CREATE TYPE std.types.BOOLEAN AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.boolean@1' IMMUTABLE PERSISTABLE;EXPORT TYPE std.types.BOOLEAN AS std.BOOLEAN;EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;".to_owned(),
            format!("CREATE SCHEMA std.extra;{STANDARD_SOURCE}"),
            "CREATE SCHEMA std;CREATE SCHEMA std.types;EXPORT TYPE std.types.BOOLEAN AS std.BOOLEAN;EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;".to_owned(),
            format!("{STANDARD_SOURCE}CREATE TYPE std.types.EXTRA AS VALUE PRIMITIVE KERNEL CONTRACT 'extra@1' IMMUTABLE PERSISTABLE;"),
            "CREATE SCHEMA std;CREATE SCHEMA std.types;CREATE TYPE std.types.BOOLEAN AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.boolean@1' IMMUTABLE PERSISTABLE;EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;".to_owned(),
            format!("{STANDARD_SOURCE}EXPORT TYPE std.types.BOOLEAN AS std.BOOL;"),
            "CREATE SCHEMA std;CREATE SCHEMA std.types;CREATE TYPE std.types.BOOLEAN AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.boolean@1' IMMUTABLE PERSISTABLE;EXPORT TYPE std.types.BOOLEAN AS std.BOOLEAN;".to_owned(),
            format!("{STANDARD_SOURCE}EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOL;"),
        ];

    for source in cases {
        assert_standard_source_mismatch(&source);
    }
}

#[test]
fn standard_reconciliation_rejects_every_unsupported_source_category() {
    let cases = [
        format!("{STANDARD_SOURCE}CREATE TYPE std.extra AS OBJECT ();"),
        format!("{STANDARD_SOURCE}ALTER TYPE std.extra RENAME FIELD old TO new;"),
        format!(
            "{STANDARD_SOURCE}CREATE SERVER FUNCTION std.extra() RETURNS ROWS (value BOOLEAN) AS SELECT o.value FROM std.object o;"
        ),
        format!("{STANDARD_SOURCE}CREATE CLIENT FUNCTION std.extra() RETURNS BOOLEAN RETURN TRUE;"),
    ];

    for source in cases {
        assert_standard_source_mismatch(&source);
    }
}

#[test]
fn standard_reconciliation_requires_exact_stored_bytes_and_origins() {
    let (stored_unit, mut parsed_unit, catalogue, origins) =
        standard_reconciliation_inputs(STANDARD_SOURCE);
    assert_eq!(parsed_unit.parsed().schemas().len(), 2);
    assert_eq!(parsed_unit.parsed().primitive_value_types().len(), 1);
    assert_eq!(parsed_unit.parsed().type_exports().len(), 2);
    parsed_unit.replace_source_text_for_test(format!("{STANDARD_SOURCE} "));
    assert_eq!(
        reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins),
        Err(super::StandardLibraryCheckError::SourceMismatch)
    );

    let (stored_unit, parsed_unit, catalogue, mut origins) =
        standard_reconciliation_inputs(STANDARD_SOURCE);
    origins.remove(0);
    assert_eq!(
        reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins),
        Err(super::StandardLibraryCheckError::SourceMismatch)
    );

    let (stored_unit, parsed_unit, catalogue, mut origins) =
        standard_reconciliation_inputs(STANDARD_SOURCE);
    origins[2] = standard_origin(
        DefinitionIdentity::ValueType(TypeId::from_bytes([3; 16])),
        43,
        159,
    );
    assert_eq!(
        reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins),
        Err(super::StandardLibraryCheckError::SourceMismatch)
    );

    let (stored_unit, parsed_unit, catalogue, mut origins) =
        standard_reconciliation_inputs(STANDARD_SOURCE);
    origins.push(origins[0].clone());
    assert_eq!(
        reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins),
        Err(super::StandardLibraryCheckError::SourceMismatch)
    );

    let (stored_unit, parsed_unit, catalogue, mut origins) =
        standard_reconciliation_inputs(STANDARD_SOURCE);
    origins.push(standard_origin(
        DefinitionIdentity::Expression(ExpressionId::from_bytes([9; 16])),
        0,
        0,
    ));
    assert_eq!(
        reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins),
        Err(super::StandardLibraryCheckError::SourceMismatch)
    );

    let (stored_unit, parsed_unit, catalogue, mut origins) =
        standard_reconciliation_inputs(STANDARD_SOURCE);
    origins[2] = standard_origin(
        DefinitionIdentity::ValueType(TypeId::from_bytes([3; 16])),
        42,
        158,
    );
    assert_eq!(
        reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins),
        Err(super::StandardLibraryCheckError::SourceMismatch)
    );

    let (stored_unit, parsed_unit, catalogue, mut origins) =
        standard_reconciliation_inputs(STANDARD_SOURCE);
    let first_source = origins[0].source();
    let second_source = origins[1].source();
    origins[0] = DefinitionOrigin::new(
        DefinitionIdentity::Schema(SchemaId::from_bytes([1; 16])),
        second_source,
    );
    origins[1] = DefinitionOrigin::new(
        DefinitionIdentity::Schema(SchemaId::from_bytes([2; 16])),
        first_source,
    );
    assert_eq!(
        reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins),
        Err(super::StandardLibraryCheckError::SourceMismatch)
    );

    let (stored_unit, parsed_unit, catalogue, mut origins) =
        standard_reconciliation_inputs(STANDARD_SOURCE);
    origins[0] = DefinitionOrigin::new(
        DefinitionIdentity::Schema(SchemaId::from_bytes([1; 16])),
        SourceOrigin::new(SourceUnitId::from_bytes([9; 16]), 0, 18).unwrap(),
    );
    assert_eq!(
        reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins),
        Err(super::StandardLibraryCheckError::SourceMismatch)
    );
}

#[test]
fn catalogue_reconciliation_precedes_hostile_origin_validation() {
    let source = STANDARD_SOURCE.replace("boolean@1", "integer@1");
    let (stored_unit, parsed_unit, catalogue, mut origins) =
        standard_reconciliation_inputs(&source);
    origins.push(origins[0].clone());

    assert_eq!(
        super::match_standard_source_facts(&parsed_unit, &catalogue),
        Err(super::StandardLibraryCheckError::SourceMismatch)
    );
    assert_eq!(
        reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins),
        Err(super::StandardLibraryCheckError::SourceMismatch)
    );

    let (stored_unit, parsed_unit, catalogue, mut origins) =
        standard_reconciliation_inputs(STANDARD_SOURCE);
    origins.push(origins[0].clone());
    let pending = super::match_standard_source_facts(&parsed_unit, &catalogue);
    assert!(pending.is_ok());
    let Ok(pending) = pending else {
        return;
    };
    assert_eq!(
        super::validate_standard_source_origins(&stored_unit, &origins, pending),
        Err(super::StandardLibraryCheckError::SourceMismatch)
    );
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
                     document TEXT,\
                     payload BYTES\
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
fn rejects_non_public_large_object_aliases_at_their_type_spans() {
    for spelling in ["CLOB", "BLOB"] {
        let source =
            format!("CREATE SCHEMA demo; CREATE TYPE demo.item AS OBJECT (value {spelling});");
        let source_bundle =
            SourceBundle::new([SourceUnit::new("types.orna", source.as_str())]).unwrap();
        let report = check(&source_bundle, &empty_catalogue());

        assert_eq!(report.diagnostics().len(), 1, "{spelling}");
        let diagnostic = &report.diagnostics()[0];
        assert_eq!(diagnostic.code(), DiagnosticCode::UnknownQualifiedName);
        assert_eq!(
            diagnostic.message(),
            format!("unknown type name {}", spelling.to_lowercase())
        );
        assert_eq!(diagnostic.location().logical_path(), "types.orna");
        let start = source.find(spelling).expect("type spelling is present");
        assert_eq!(diagnostic.location().span().start(), start);
        assert_eq!(diagnostic.location().span().end(), start + spelling.len());
        assert_no_checked_bundle(&report);
    }
}

#[test]
fn resolves_required_unique_references_with_forward_targets_and_replay_ids() {
    let source = "CREATE SCHEMA tasks; CREATE SCHEMA people; \
            CREATE TYPE tasks.assignment AS OBJECT (owner REF people.owner UNIQUE NOT NULL); \
            CREATE TYPE people.owner AS OBJECT ();";

    let report = check(&bundle([("unique.orna", source)]), &empty_catalogue());

    assert!(report.diagnostics().is_empty());
    let checked = report.checked_bundle().unwrap();
    let assignment = &checked.object_types()[0];
    let owner = &checked.object_types()[1];
    let field = &assignment.fields()[0];
    assert!(assignment.id().is_provisional());
    assert!(field.id().is_provisional());
    assert_eq!(field.semantic_type(), SemanticType::reference(owner.id()));
    assert!(!field.nullable());
    assert!(field.unique());

    let owner_id = TypeId::from_bytes([3; 16]);
    let assignment_id = TypeId::from_bytes([4; 16]);
    let owner_field = FieldId::from_bytes([5; 16]);
    let base = catalogue(
        vec![schema(1, &["people"]), schema(2, &["tasks"])],
        vec![
            object_type(
                4,
                &["tasks", "assignment"],
                vec![FieldDefinition::new(
                    owner_field,
                    "owner",
                    0,
                    ResolvedType::reference(owner_id),
                    false,
                    true,
                    None,
                    None,
                )],
            ),
            object_type(3, &["people", "owner"], Vec::new()),
        ],
        Vec::new(),
    );
    let replay = check(&bundle([("unique.orna", source)]), &base);

    assert!(replay.diagnostics().is_empty());
    let assignment = &replay.checked_bundle().unwrap().object_types()[0];
    let field = &assignment.fields()[0];
    assert_eq!(assignment.id().existing(), Some(assignment_id));
    assert_eq!(field.id().existing(), Some(owner_field));
    assert_eq!(
        field.semantic_type(),
        SemanticType::reference(CheckedTypeId::Existing(owner_id))
    );
}

#[test]
fn resolves_nullable_and_required_unique_text_with_required_unique_reference() {
    let source = "CREATE SCHEMA people; CREATE SCHEMA crm; \
            CREATE TYPE crm.contact AS OBJECT (\
                email TEXT UNIQUE,\
                name CHARACTER LARGE OBJECT NOT NULL UNIQUE,\
                owner REF people.owner NOT NULL UNIQUE\
            ); \
            CREATE TYPE people.owner AS OBJECT ();";

    let report = check(&bundle([("unique_text.orna", source)]), &empty_catalogue());

    assert!(report.diagnostics().is_empty());
    let fields = report.checked_bundle().unwrap().object_types()[0].fields();
    assert_eq!(fields.len(), 3);
    assert_eq!(
        fields[0].semantic_type(),
        SemanticType::scalar(StandardScalar::CharacterLargeObject)
    );
    assert!(fields[0].nullable());
    assert!(fields[0].unique());
    assert_eq!(
        fields[1].semantic_type(),
        SemanticType::scalar(StandardScalar::CharacterLargeObject)
    );
    assert!(!fields[1].nullable());
    assert!(fields[1].unique());
    assert!(matches!(
        fields[2].semantic_type(),
        SemanticType::Reference { .. }
    ));
    assert!(!fields[2].nullable());
    assert!(fields[2].unique());
}

#[test]
fn rejects_unique_fields_outside_the_required_reference_shape() {
    for spelling in LEGACY_CANONICAL_SCALAR_SPELLINGS
        .iter()
        .copied()
        .filter(|spelling| *spelling != "CHARACTER LARGE OBJECT")
    {
        let source = format!(
            "CREATE SCHEMA demo; CREATE TYPE demo.item AS OBJECT (value {} UNIQUE);",
            spelling
        );
        let bundle = SourceBundle::new([SourceUnit::new("unique.orna", source.clone())]).unwrap();
        let report = check(&bundle, &empty_catalogue());

        assert_eq!(report.diagnostics().len(), 1, "{source}");
        let diagnostic = &report.diagnostics()[0];
        assert_eq!(diagnostic.code(), DiagnosticCode::TypeMismatch);
        assert_eq!(diagnostic.code().as_str(), "ORNA0201");
        assert_eq!(
            diagnostic.message(),
            "UNIQUE is only available for TEXT fields or REF fields that are NOT NULL"
        );
        assert_eq!(diagnostic.location().logical_path(), "unique.orna");
        let start = source.find("value").unwrap();
        assert_eq!(diagnostic.location().span().start(), start);
        assert_eq!(
            diagnostic.location().span().end(),
            start + "value ".len() + spelling.len() + " UNIQUE".len()
        );
        assert_no_checked_bundle(&report);
    }

    let source = "CREATE SCHEMA people; CREATE SCHEMA tasks; \
            CREATE TYPE tasks.assignment AS OBJECT (owner REF people.owner UNIQUE); \
            CREATE TYPE people.owner AS OBJECT ();";
    let report = check(&bundle([("unique.orna", source)]), &empty_catalogue());

    assert_eq!(report.diagnostics().len(), 1);
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(diagnostic.code(), DiagnosticCode::TypeMismatch);
    assert_eq!(diagnostic.code().as_str(), "ORNA0201");
    assert_eq!(
        diagnostic.message(),
        "UNIQUE is only available for TEXT fields or REF fields that are NOT NULL"
    );
    let start = source.find("owner REF").unwrap();
    assert_eq!(diagnostic.location().span().start(), start);
    assert_eq!(
        diagnostic.location().span().end(),
        start + "owner REF people.owner UNIQUE".len()
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn unique_field_validation_preserves_existing_field_diagnostic_precedence() {
    let source = "CREATE SCHEMA demo; CREATE TYPE demo.item AS OBJECT (\
            repeated TEXT, repeated TEXT UNIQUE,\
            missing REF demo.missing UNIQUE,\
            scalar_target REF TEXT UNIQUE,\
            deleted TEXT UNIQUE ON DELETE RESTRICT,\
            defaulted INT UNIQUE DEFAULT TRUE\
        );";
    let report = check(&bundle([("unique.orna", source)]), &empty_catalogue());

    let expected = [
        (
            DiagnosticCode::DuplicateDefinition,
            "duplicate field definition repeated in demo.item",
        ),
        (
            DiagnosticCode::UnknownQualifiedName,
            "unknown object type demo.missing",
        ),
        (
            DiagnosticCode::InvalidReferenceTarget,
            "REF target text is a scalar type",
        ),
        (
            DiagnosticCode::TypeMismatch,
            "ON DELETE is only valid for REF fields",
        ),
        (
            DiagnosticCode::TypeMismatch,
            "UNIQUE is only available for TEXT fields or REF fields that are NOT NULL",
        ),
        (
            DiagnosticCode::TypeMismatch,
            "default constant does not match the field type and nullability",
        ),
    ];
    assert_eq!(report.diagnostics().len(), expected.len());
    for (diagnostic, (code, message)) in report.diagnostics().iter().zip(expected) {
        assert_eq!(diagnostic.code(), code);
        assert_eq!(diagnostic.message(), message);
    }
    assert_no_checked_bundle(&report);
}

#[test]
fn required_unique_reference_preserves_set_null_diagnostic_precedence() {
    let source = "CREATE SCHEMA people; CREATE SCHEMA tasks; \
            CREATE TYPE tasks.assignment AS OBJECT (\
                owner REF people.owner NOT NULL UNIQUE ON DELETE SET NULL\
            ); \
            CREATE TYPE people.owner AS OBJECT ();";
    let report = check(&bundle([("unique.orna", source)]), &empty_catalogue());

    assert_eq!(report.diagnostics().len(), 1);
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(diagnostic.code(), DiagnosticCode::TypeMismatch);
    assert_eq!(diagnostic.code().as_str(), "ORNA0201");
    assert_eq!(
        diagnostic.message(),
        "ON DELETE SET NULL requires a nullable field"
    );
    assert_eq!(diagnostic.location().logical_path(), "unique.orna");
    let start = source.find("owner REF").unwrap();
    assert_eq!(diagnostic.location().span().start(), start);
    assert_eq!(
        diagnostic.location().span().end(),
        start + "owner REF people.owner NOT NULL UNIQUE ON DELETE SET NULL".len()
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn unique_text_or_required_reference_support_is_closed_to_accepted_shapes() {
    let type_id = CheckedTypeId::Existing(TypeId::from_bytes([1; 16]));

    assert!(super::supports_unique_text_or_required_reference(
        SemanticType::reference(type_id),
        false
    ));
    assert!(!super::supports_unique_text_or_required_reference(
        SemanticType::reference(type_id),
        true
    ));
    assert!(super::supports_unique_text_or_required_reference(
        SemanticType::scalar(StandardScalar::CharacterLargeObject),
        true
    ));
    assert!(super::supports_unique_text_or_required_reference(
        SemanticType::scalar(StandardScalar::CharacterLargeObject),
        false
    ));
    assert!(!super::supports_unique_text_or_required_reference(
        SemanticType::Named(type_id),
        false
    ));
    for scalar in StandardScalar::ALL {
        assert_eq!(
            super::supports_unique_text_or_required_reference(SemanticType::scalar(scalar), false),
            scalar == StandardScalar::CharacterLargeObject
        );
    }
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
fn resolves_unique_text_selected_query_with_separate_plan_and_evidence() {
    let source = "CREATE SCHEMA people; \
            CREATE TYPE people.person AS OBJECT (email TEXT UNIQUE, name TEXT); \
            CREATE SERVER FUNCTION people.by_email(p_email TEXT) \
            RETURNS ROWS (name TEXT) \
            SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE \
            AS SELECT p.name FROM people.person p WHERE p.email = p_email;";
    let report = check(&bundle([("unique_text.orna", source)]), &empty_catalogue());

    assert!(report.diagnostics().is_empty());
    let checked = report.checked_bundle().unwrap();
    let person = &checked.object_types()[0];
    let email = &person.fields()[0];
    let function = &checked.server_functions()[0];
    let plan = function
        .unique_text_selected_query_plan()
        .expect("fixture has a unique-Text-selected SELECT body");
    assert!(function.query_plan().is_none());
    assert!(function.distinct_query_plan().is_none());
    assert!(function.identity_selected_query_plan().is_none());
    assert!(function.mutation_plan().is_none());
    assert!(function.delete_plan().is_none());
    assert_eq!(plan.scan().object_type(), person.id());
    assert_eq!(plan.selector().scan_object_type(), person.id());
    assert_eq!(plan.selector().field_owner(), person.id());
    assert_eq!(plan.selector().field(), email.id());
    assert_eq!(plan.selector().parameter_owner(), function.id());
    assert_eq!(plan.selector().parameter(), function.parameters()[0].id());
    assert_eq!(
        plan.selector().text_type().semantic_type(),
        SemanticType::scalar(StandardScalar::CharacterLargeObject)
    );
    assert!(plan.selector().field_nullable());
    assert!(plan.selector().parameter_required_non_null());

    let selector_field_start = source.rfind("p.email").unwrap() + 2;
    let parameter_start = source.rfind("p_email").unwrap();
    assert!(function.references().iter().any(|reference| {
        reference.kind() == DefinitionReferenceKind::QueryField
            && reference.target()
                == CheckedDefinitionReferenceTarget::Field {
                    owner: person.id(),
                    field: email.id(),
                }
            && reference.location().span().start() == selector_field_start
    }));
    assert!(function.references().iter().any(|reference| {
        reference.kind() == DefinitionReferenceKind::ParameterRead
            && reference.target()
                == CheckedDefinitionReferenceTarget::Parameter {
                    owner: function.id(),
                    parameter: function.parameters()[0].id(),
                }
            && reference.location().span().start() == parameter_start
    }));
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
    let return_target_start =
        source.find("RETURNS ROWS (task REF tasks.task").unwrap() + "RETURNS ROWS (task REF ".len();
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
    for (reference, (kind, target, start, length)) in function.references().iter().zip(expected) {
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
    let object_reference_start = query_start + source[query_start..].find("REF(t)").unwrap() + 4;
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
    for (reference, (kind, target, start, length)) in function.references().iter().zip(expected) {
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
            WHERE t.assignee.active;";
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
    let ExpressionKind::FieldPath { steps, .. } = plan.projections()[1].kind() else {
        panic!("second DISTINCT projection must be a field path");
    };
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0].owner(), task.id());
    assert_eq!(steps[0].field(), assignee.id());
    assert_eq!(steps[1].owner(), person.id());
    assert_eq!(steps[1].field(), active.id());
    let selection = plan.selection().expect("fixture has a direct predicate");
    let ExpressionKind::FieldPath { steps, .. } = selection.kind() else {
        panic!("direct DISTINCT predicate must be a field path");
    };
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0].owner(), task.id());
    assert_eq!(steps[0].field(), assignee.id());
    assert_eq!(steps[1].owner(), person.id());
    assert_eq!(steps[1].field(), active.id());
    assert_eq!(
        selection.value_type().semantic_type(),
        SemanticType::scalar(StandardScalar::Boolean)
    );
    assert!(selection.value_type().nullable());

    let query_start = source.find("SELECT DISTINCT").unwrap();
    let query_object_start = query_start + source[query_start..].find("tasks.task").unwrap();
    let projection_reference_start =
        query_start + source[query_start..].find("REF(t)").unwrap() + "REF(".len();
    let assignee_starts = source
        .match_indices("t.assignee.active")
        .map(|(start, _)| start)
        .collect::<Vec<_>>();
    assert_eq!(assignee_starts.len(), 2);
    let completed_start = source.find("t.completed").unwrap();
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
            assignee_starts[0] + "t.".len(),
            "assignee".len(),
        ),
        (
            DefinitionReferenceKind::QueryField,
            CheckedDefinitionReferenceTarget::Field {
                owner: person.id(),
                field: active.id(),
            },
            assignee_starts[0] + "t.assignee.".len(),
            "active".len(),
        ),
        (
            DefinitionReferenceKind::QueryField,
            CheckedDefinitionReferenceTarget::Field {
                owner: task.id(),
                field: completed.id(),
            },
            completed_start + "t.".len(),
            "completed".len(),
        ),
        (
            DefinitionReferenceKind::QueryField,
            CheckedDefinitionReferenceTarget::Field {
                owner: task.id(),
                field: assignee.id(),
            },
            assignee_starts[1] + "t.".len(),
            "assignee".len(),
        ),
        (
            DefinitionReferenceKind::QueryField,
            CheckedDefinitionReferenceTarget::Field {
                owner: person.id(),
                field: active.id(),
            },
            assignee_starts[1] + "t.assignee.".len(),
            "active".len(),
        ),
    ];

    assert_eq!(function.references().len(), expected.len());
    for (reference, (kind, target, start, length)) in function.references().iter().zip(expected) {
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
fn accepts_single_return_select_at_the_declared_return() {
    let source = "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (name TEXT); \
            CREATE SERVER FUNCTION people.find() RETURNS TEXT AS SELECT p.name FROM people.person p;";
    let report = check(&bundle([("functions.orna", source)]), &empty_catalogue());

    assert_eq!(report.diagnostics(), &[]);
    let checked = report.checked_bundle().unwrap();
    let functions = checked.server_functions();
    assert_eq!(functions.len(), 1);
    let function = &functions[0];
    assert!(matches!(
        function.return_type(),
        super::CheckedServerFunctionReturn::Single {
            semantic_type: super::SemanticType::Scalar(StandardScalar::CharacterLargeObject),
            ..
        }
    ));
    let query = function.query_plan().expect("scalar SELECT query plan");
    assert_eq!(query.projections().len(), 1);
    assert_eq!(
        query.projections()[0].value_type().semantic_type(),
        super::SemanticType::Scalar(StandardScalar::CharacterLargeObject)
    );
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
    assert_eq!(
        diagnostics[0].message(),
        "duplicate server function definition people.find"
    );
    assert_eq!(diagnostics.len(), 1);
    assert_no_checked_bundle(&report);
}

#[test]
fn preserves_server_header_and_duplicate_diagnostic_order() {
    let source = "CREATE SCHEMA people;\
            CREATE SERVER FUNCTION people.find() RETURNS TEXT TRANSACTION MANUAL AS SELECT TRUE FROM people.person p;\
            CREATE SERVER FUNCTION people.find() RETURNS TEXT AS SELECT FALSE FROM people.person p;";
    let report = check(&bundle([("functions.orna", source)]), &empty_catalogue());

    assert_eq!(report.diagnostics().len(), 2);
    assert_eq!(
        report.diagnostics()[0].message(),
        "SERVER functions do not yet support TRANSACTION MANUAL"
    );
    assert_eq!(
        report.diagnostics()[0].location().span().start(),
        source.find("CREATE SERVER").unwrap()
    );
    assert_eq!(
        report.diagnostics()[1].message(),
        "duplicate server function definition people.find"
    );
    assert_eq!(
        report.diagnostics()[1].location().span().start(),
        source.rfind("people.find").unwrap()
    );
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
                let start = source.find("p_owner REF tasks.person").unwrap() + "p_owner REF ".len();
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
    let prefix = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT NOT NULL); ";
    let body = "AS DELETE FROM tasks.task AS removed WHERE REF(removed) = p_task RETURNING TRUE;";
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
    let source_bundle = SourceBundle::new([SourceUnit::new("functions.orna", &source)]).unwrap();
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
    let prefix = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT NOT NULL); ";
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
        diagnostic.location().span().start() == wrong_modes.rfind("CREATE SERVER FUNCTION").unwrap()
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
            (
                DiagnosticCode::DomainIncompatible,
                "SELECT DISTINCT SERVER functions require zero declared parameters",
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
            CREATE TYPE tasks.task AS OBJECT (completed BOOL NOT NULL, title TEXT); \
            CREATE SERVER FUNCTION tasks.values(p_flag BOOL) RETURNS ROWS (completed BOOL) \
            SECURITY DEFINER TRANSACTION ATOMIC VOLATILITY IMMUTABLE \
            AS SELECT DISTINCT t.completed FROM tasks.task t WHERE t.title;";
    let report = check(
        &bundle([("distinct_semantic.orna", semantic_source)]),
        &empty_catalogue(),
    );
    assert_no_checked_bundle(&report);
    assert_eq!(report.diagnostics().len(), 1);
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(diagnostic.code(), DiagnosticCode::TypeMismatch);
    assert_eq!(diagnostic.message(), "WHERE requires a BOOLEAN expression");
    assert_eq!(
        diagnostic.location().logical_path(),
        "distinct_semantic.orna"
    );
    let predicate_start = semantic_source.rfind("t.title").unwrap();
    assert_eq!(diagnostic.location().span().start(), predicate_start);
    assert_eq!(
        diagnostic.location().span().end(),
        predicate_start + "t.title".len()
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
fn resolves_server_stream_element_and_preserves_checked_shape() {
    let source = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); \
            CREATE SERVER FUNCTION tasks.events() RETURNS STREAM<TEXT> \
            AS SELECT t.title FROM tasks.task t;";
    let report = check(&bundle([("stream.orna", source)]), &empty_catalogue());
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let function = &report.checked_bundle().unwrap().server_functions()[0];
    let super::CheckedServerFunctionReturn::Stream {
        semantic_type,
        standard_value_type,
        ..
    } = function.return_type()
    else {
        panic!("expected a checked STREAM return");
    };
    assert_eq!(
        *semantic_type,
        SemanticType::scalar(StandardScalar::CharacterLargeObject)
    );
    assert_eq!(*standard_value_type, None);
    assert!(function.return_columns().is_empty());
}

#[test]
fn discovers_stream_resource_target_with_resolved_element_type() {
    let source = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); \
            CREATE SERVER FUNCTION tasks.events() RETURNS STREAM<TEXT> \
            AS SELECT t.title FROM tasks.task t; \
            CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.read() RETURNS STREAM<TEXT> IS \
            BEGIN RETURN AWAIT std.data.stream_resource(target => tasks.events, arguments => std.call.args()); END;";
    let report = check(
        &bundle([("stream-resource.orna", source)]),
        &empty_catalogue(),
    );
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let client = &report.checked_bundle().unwrap().client_functions()[0];
    let super::CheckedClientFunctionBody::Expression {
        expression: return_expression,
    } = client.body()
    else {
        panic!("expected a checked CLIENT expression body");
    };
    let super::CheckedClientExpression::Await {
        expression,
        location: await_location,
    } = return_expression
    else {
        panic!("expected AWAIT expression");
    };
    let await_text =
        "AWAIT std.data.stream_resource(target => tasks.events, arguments => std.call.args())";
    let await_start = source
        .find(await_text)
        .expect("await expression is present");
    assert_eq!(await_location.logical_path(), "stream-resource.orna");
    assert_eq!(await_location.span().start(), await_start);
    assert_eq!(await_location.span().end(), await_start + await_text.len());
    let super::CheckedClientExpression::Resource { operation } = expression.as_ref() else {
        panic!("expected stream resource expression");
    };
    assert_eq!(
        operation.kind(),
        orna_artifact::client_plan::ResourceKind::Stream
    );
    assert_eq!(
        operation.result_type(),
        SemanticType::scalar(StandardScalar::CharacterLargeObject)
    );
    let resource_text =
        "std.data.stream_resource(target => tasks.events, arguments => std.call.args())";
    let resource_start = source
        .find(resource_text)
        .expect("resource constructor is present");
    assert_eq!(operation.location().logical_path(), "stream-resource.orna");
    assert_eq!(operation.location().span().start(), resource_start);
    assert_eq!(
        operation.location().span().end(),
        resource_start + resource_text.len()
    );
}

#[test]
fn stream_await_requires_optional_list_return_and_local_shape() {
    let valid = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); \
            CREATE SERVER FUNCTION tasks.events() RETURNS STREAM<TEXT> \
            AS SELECT t.title FROM tasks.task t; \
            CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.read() RETURNS STREAM<TEXT> IS \
            LET rows std.data.StreamResource<TEXT> := std.data.stream_resource(target => tasks.events, arguments => std.call.args()); \
            BEGIN RETURN AWAIT rows; END;";
    let report = check(
        &bundle([("stream-await-valid.orna", valid)]),
        &empty_catalogue(),
    );
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );

    let invalid_return = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); \
            CREATE SERVER FUNCTION tasks.events() RETURNS STREAM<TEXT> \
            AS SELECT t.title FROM tasks.task t; \
            CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.read() RETURNS TEXT IS \
            LET rows std.data.StreamResource<TEXT> := std.data.stream_resource(target => tasks.events, arguments => std.call.args()); \
            BEGIN RETURN AWAIT rows; END;";
    let report = check(
        &bundle([("stream-await-return.orna", invalid_return)]),
        &empty_catalogue(),
    );
    assert_eq!(report.diagnostics().len(), 1, "{:?}", report.diagnostics());
    assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);

    let invalid_assignment = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); \
            CREATE SERVER FUNCTION tasks.events() RETURNS STREAM<TEXT> \
            AS SELECT t.title FROM tasks.task t; \
            CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.read() RETURNS STREAM<TEXT> IS \
            LET rows TEXT := std.data.stream_resource(target => tasks.events, arguments => std.call.args()); \
            BEGIN RETURN AWAIT rows; END;";
    let report = check(
        &bundle([("stream-await-assignment.orna", invalid_assignment)]),
        &empty_catalogue(),
    );
    assert_eq!(report.diagnostics().len(), 1, "{:?}", report.diagnostics());
    assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);
}

#[test]
fn rejects_server_stream_queries_with_multiple_projected_columns() {
    let source = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT, done BOOL); \
            CREATE SERVER FUNCTION tasks.events() RETURNS STREAM<TEXT> \
            AS SELECT t.title, t.done FROM tasks.task t;";
    let report = check(&bundle([("stream-shape.orna", source)]), &empty_catalogue());
    assert_eq!(report.diagnostics().len(), 1, "{:?}", report.diagnostics());
    assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);
    assert_eq!(
        report.diagnostics()[0].message(),
        "SELECT returns 2 columns, but RETURNS STREAM<T> declares one element"
    );
    assert_no_checked_bundle(&report);
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

#[test]
fn checks_client_boolean_constant_with_exact_model_and_literal_location() {
    let source = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.enabled() RETURNS BOOL RETURN tRuE;";
    let report = check(&bundle([("client.orna", source)]), &empty_catalogue());

    assert!(report.diagnostics().is_empty());
    let checked = report.checked_bundle().unwrap();
    assert!(checked.server_functions().is_empty());
    let function = &checked.client_functions()[0];
    assert_eq!(function.name().to_string(), "examples.enabled");
    assert_eq!(function.domain(), FunctionDomain::Client);
    assert!(function.id().is_provisional());
    assert!(function.parameters().is_empty());
    assert_eq!(
        function.return_type(),
        SemanticType::scalar(StandardScalar::Boolean)
    );
    assert_eq!(function.security(), FunctionSecurity::Invoker);
    assert_eq!(function.transaction(), None);
    assert_eq!(function.volatility(), FunctionVolatility::Immutable);
    assert!(function.references().is_empty());
    assert_eq!(
        function.location().span().start(),
        source.find("CREATE CLIENT").unwrap()
    );
    assert_eq!(function.location().span().end(), source.len());
    let literal_start = source.find("tRuE").unwrap();
    let (value, literal_location) = function.boolean_body().unwrap();
    assert!(value);
    assert_eq!(literal_location.logical_path(), "client.orna");
    assert_eq!(literal_location.span().start(), literal_start);
    assert_eq!(literal_location.span().end(), literal_start + 4);
}

#[test]
fn rejects_client_integer_literals_outside_i32_range_and_accepts_boundary() {
    let out_of_range = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.value() RETURNS INTEGER AS 2147483648;";
    let report = check(&bundle([("client.orna", out_of_range)]), &empty_catalogue());
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);
    assert_eq!(
        report.diagnostics()[0].message(),
        "CLIENT integer literal is outside the INTEGER range"
    );
    assert_eq!(
        report.diagnostics()[0].location().span().start(),
        out_of_range.find("2147483648").unwrap()
    );
    assert_no_checked_bundle(&report);

    let in_range = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.value() RETURNS INTEGER AS 2147483647;";
    let report = check(&bundle([("client.orna", in_range)]), &empty_catalogue());
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let function = &report.checked_bundle().unwrap().client_functions()[0];
    assert!(matches!(
        function.body(),
        CheckedClientFunctionBody::Expression {
            expression: CheckedClientExpression::Integer {
                value: 2_147_483_647,
                ..
            }
        }
    ));
}

#[test]
fn rejects_out_of_range_control_flow_literals() {
    let source = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.value() RETURNS INTEGER IS \
            BEGIN IF TRUE THEN RETURN 2147483648; ELSE RETURN 0; END IF; END;";
    let report = check(&bundle([("client.orna", source)]), &empty_catalogue());

    assert_eq!(report.diagnostics().len(), 1, "{:?}", report.diagnostics());
    assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);
    assert_eq!(
        report.diagnostics()[0].message(),
        "CLIENT integer literal is outside the INTEGER range"
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn accepts_let_declarations_inside_while_bodies() {
    let source = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.value() RETURNS INTEGER IS \
            BEGIN \
                LET index INTEGER := 0; \
                WHILE index < 2 LOOP \
                    LET item INTEGER := index; \
                    index := item + 1; \
                END LOOP; \
                RETURN index; \
            END;";
    let report = check(&bundle([("client.orna", source)]), &empty_catalogue());

    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    assert!(report.checked_bundle().is_some());
}

fn validate_capability_text(
    capability: &str,
    declared_parameters: &[&str],
) -> Vec<crate::CompilerDiagnostic> {
    let source = format!(
        "CREATE CLIENT FUNCTION examples.f() RETURNS BOOLEAN REQUIRES CAPABILITY {capability} RETURN TRUE;"
    );
    let parsed = parse(&source);
    assert!(parsed.diagnostics().is_empty(), "source: {source}");
    let declaration = &parsed.client_functions()[0];
    let mut diagnostics = Vec::new();
    validate_client_capability(
        &declaration.capabilities[0],
        declared_parameters.iter().copied(),
        "capability.orna",
        &declaration.span,
        &mut diagnostics,
    );
    diagnostics
}

#[test]
fn validates_closed_client_capability_vocabulary_and_argument_shapes() {
    for capability in [
        "std.fs.read('/tmp/input')",
        "std.fs.write('/tmp/output')",
        "std.net.connect('db.internal')",
        "std.secret.use('database-password')",
    ] {
        assert!(
            validate_capability_text(capability, &[]).is_empty(),
            "capability: {capability}"
        );
    }
    assert!(validate_capability_text("std.fs.read(p_file)", &["p_file"]).is_empty());
}

#[test]
fn rejects_invalid_client_capability_names_counts_arguments_and_references() {
    for capability in [
        "std.net.call('db.internal')",
        "std.fs.read()",
        "std.fs.read('/tmp/a', '/tmp/b')",
        "std.fs.read(42)",
    ] {
        let diagnostics = validate_capability_text(capability, &[]);
        assert_eq!(diagnostics.len(), 1, "capability: {capability}");
        assert_eq!(
            diagnostics[0].code(),
            DiagnosticCode::CapabilityRequirement,
            "capability: {capability}"
        );
    }

    let diagnostics = validate_capability_text("std.fs.read(p_file)", &[]);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].code(), DiagnosticCode::CapabilityRequirement);
    assert!(diagnostics[0].message().contains("undeclared parameter"));
}

#[test]
fn rejects_capabilities_on_accepted_client_boolean_bodies() {
    let source = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.read() \
            RETURNS BOOLEAN REQUIRES CAPABILITY std.fs.read('/tmp/input') RETURN TRUE;";
    let report = check(&bundle([("client.orna", source)]), &empty_catalogue());

    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::CapabilityRequirement
    );
    assert_eq!(
        report.diagnostics()[0].message(),
        "accepted CLIENT function bodies must not declare capabilities"
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn rejects_client_parameter_defaults_before_expression_lowering() {
    let source = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.identity(p TEXT DEFAULT 'fallback') RETURNS TEXT AS p;";
    let report = check(&bundle([("client.orna", source)]), &empty_catalogue());

    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);
    assert_eq!(
        report.diagnostics()[0].message(),
        "CLIENT function parameters do not yet support default values"
    );
    assert_eq!(
        report.diagnostics()[0].location().span().start(),
        source.find("'fallback'").unwrap()
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn accepts_external_client_parameters_and_capabilities() {
    let source = "CREATE SCHEMA examples; \
            CREATE EXTERNAL CLIENT FUNCTION examples.connect(p_host TEXT) \
            RETURNS TEXT \
            RUNTIME CONTRACT 'std.net.connect@1' \
            REQUIRES CAPABILITY std.net.connect(p_host);";
    let report = check(&bundle([("client.orna", source)]), &empty_catalogue());

    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let function = &report.checked_bundle().unwrap().client_functions()[0];
    assert_eq!(function.parameters().len(), 1);
    assert_eq!(function.capabilities().len(), 1);
    assert_eq!(function.capabilities()[0].name(), "std.net.connect");
    assert_eq!(
        function.capabilities()[0].argument(),
        &super::CheckedClientCapabilityArgument::Parameter("p_host".to_owned())
    );
    assert!(matches!(
        function.body(),
        CheckedClientFunctionBody::ExternalContract { identity, .. }
            if identity == "std.net.connect@1"
    ));
}

#[test]
fn checks_client_state_slots_and_rejects_state_shape_type_errors() {
    let valid = "CREATE SCHEMA examples; \
            CREATE CLIENT FUNCTION examples.state() RETURNS TEXT IS \
            STATE filter TEXT SCOPE LOCAL DEFAULT ''; \
            STATE selected TEXT SCOPE SESSION DEFAULT NULL; \
            STATE count INTEGER; \
            STATE total BIGINT; \
            STATE ratio FLOAT; \
            STATE payload BYTES; \
            BEGIN RETURN 'ready'; END;";
    let report = check(&bundle([("client.orna", valid)]), &empty_catalogue());
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let function = &report.checked_bundle().unwrap().client_functions()[0];
    let CheckedClientFunctionBody::StateBlock { states, .. } = function.body() else {
        panic!("expected checked CLIENT state block");
    };
    assert_eq!(states.len(), 6);
    assert_eq!(states[3].name(), "total");
    assert_eq!(states[4].name(), "ratio");
    assert_eq!(states[5].name(), "payload");
    assert!(matches!(
        states[0].default(),
        CheckedStateDefault::Expression(_)
    ));
    assert!(matches!(states[1].default(), CheckedStateDefault::Null));
    assert!(matches!(states[2].default(), CheckedStateDefault::Unset));
    assert!(matches!(states[3].default(), CheckedStateDefault::Unset));
    assert!(matches!(states[4].default(), CheckedStateDefault::Unset));
    assert!(matches!(states[5].default(), CheckedStateDefault::Unset));

    let sealed_session = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.state() RETURNS BOOLEAN IS STATE snapshot sys.inspect.snapshot SCOPE SESSION; BEGIN RETURN TRUE; END;";
    let report = check(
        &bundle([("client.orna", sealed_session)]),
        &empty_catalogue(),
    );
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::DomainIncompatible
    );
    assert!(
        report.diagnostics()[0]
            .message()
            .contains("sealed sys.inspect carriers are transient")
    );

    let sealed_user = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.state() RETURNS BOOLEAN IS STATE snapshot_options sys.inspect.snapshot_options SCOPE USER; BEGIN RETURN TRUE; END;";

    let report = check(&bundle([("client.orna", sealed_user)]), &empty_catalogue());
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::DomainIncompatible
    );
    assert!(
        report.diagnostics()[0]
            .message()
            .contains("sealed sys.inspect carriers are transient")
    );
    let sealed_local = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.state() RETURNS BOOLEAN IS STATE snapshot sys.inspect.snapshot SCOPE LOCAL; BEGIN RETURN TRUE; END;";
    let report = check(&bundle([("client.orna", sealed_local)]), &empty_catalogue());
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::DomainIncompatible
    );
    assert!(
        report.diagnostics()[0]
            .message()
            .contains("sealed sys.inspect carriers are transient")
    );

    let duplicate = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.state() RETURNS TEXT IS \
            STATE value TEXT; STATE value INTEGER; BEGIN RETURN 'ready'; END;";
    let report = check(&bundle([("client.orna", duplicate)]), &empty_catalogue());
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::DuplicateDefinition
    );
    assert!(
        report.diagnostics()[0]
            .message()
            .contains("duplicate state definition")
    );

    let bad_default = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.state() RETURNS TEXT IS \
            STATE value TEXT DEFAULT 1; BEGIN RETURN 'ready'; END;";

    let report = check(&bundle([("client.orna", bad_default)]), &empty_catalogue());
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);
    assert_eq!(
        report.diagnostics()[0].message(),
        "this CLIENT state default must have the declared state type"
    );

    let bad_return = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.state() RETURNS TEXT IS \
            STATE value TEXT; BEGIN RETURN 1; END;";
    let report = check(&bundle([("client.orna", bad_return)]), &empty_catalogue());
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);
    assert_eq!(
        report.diagnostics()[0].message(),
        "this CLIENT function must return the declared value type"
    );
}

#[test]
fn rejects_opaque_values_in_client_state() {
    let standard =
        check_standard_library_source(&verified_standard_library_with_action_for_test()).unwrap();
    let base = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&base, &standard).unwrap();
    let source = "CREATE SCHEMA examples; \
            CREATE CLIENT FUNCTION examples.state() RETURNS INTEGER IS \
            STATE action std.Action; \
            BEGIN RETURN 1; END;";
    let report = check_standard_application(&bundle([("state.orna", source)]), &context);

    assert_eq!(report.diagnostics().len(), 1, "{:?}", report.diagnostics());
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::DomainIncompatible,
        "{:?}",
        report.diagnostics()
    );
    assert_eq!(
        report.diagnostics()[0].message(),
        "opaque CLIENT values are transient and cannot be stored in state"
    );
    assert!(report.preparation_view().is_none());
}
#[test]
fn rejects_inspector_expressions_in_state_defaults_and_returns() {
    let cases = [
        (
            "CREATE SCHEMA devtools; CREATE CLIENT FUNCTION devtools.state(p_target REF sys.inspect.invocation) RETURNS BOOLEAN IS \
                    STATE snapshot TEXT SCOPE LOCAL DEFAULT sys.inspect.snapshot(p_target => p_target); \
                    BEGIN RETURN TRUE; END;",
            "CLIENT state defaults do not support Inspector expressions",
        ),
        (
            "CREATE SCHEMA devtools; CREATE CLIENT FUNCTION devtools.state(p_target REF sys.inspect.invocation) RETURNS sys.inspect.snapshot IS \
                    STATE value TEXT; BEGIN RETURN sys.inspect.snapshot(p_target => p_target); END;",
            "CLIENT state blocks do not support Inspector expressions",
        ),
    ];

    for (source, message) in cases {
        let report = check(
            &bundle([("inspector-state.orna", source)]),
            &empty_catalogue(),
        );
        assert_eq!(report.diagnostics().len(), 1, "{:?}", report.diagnostics());
        assert_eq!(
            report.diagnostics()[0].code(),
            DiagnosticCode::DomainIncompatible
        );
        assert_eq!(report.diagnostics()[0].message(), message);
        assert_no_checked_bundle(&report);
    }
}

#[test]
fn accepts_parameters_on_client_state_blocks() {
    let source = "CREATE SCHEMA examples; \
            CREATE CLIENT FUNCTION examples.state(p TEXT) RETURNS TEXT IS \
            STATE value TEXT; BEGIN RETURN p; END;";
    let report = check(&bundle([("client.orna", source)]), &empty_catalogue());
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let function = &report.checked_bundle().unwrap().client_functions()[0];
    assert_eq!(function.parameters().len(), 1);
    assert!(matches!(
        function.body(),
        CheckedClientFunctionBody::StateBlock { states, .. } if states.len() == 1
    ));
}

#[test]
fn keeps_empty_no_state_client_blocks_as_expression_bodies() {
    let source = "CREATE SCHEMA examples;             CREATE CLIENT FUNCTION examples.identity(p TEXT) RETURNS TEXT IS             BEGIN RETURN p; END;";
    let report = check(&bundle([("client.orna", source)]), &empty_catalogue());
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let function = &report.checked_bundle().unwrap().client_functions()[0];
    let CheckedClientFunctionBody::Expression { expression } = function.body() else {
        panic!("expected an expression CLIENT body");
    };
    assert!(matches!(
        expression,
        super::CheckedClientExpression::ParameterRead { .. }
    ));
}

#[test]
fn accepts_procedural_client_statements_without_state_declarations() {
    let source = "CREATE SCHEMA examples; \
            CREATE CLIENT FUNCTION examples.procedural() RETURNS TEXT IS \
            BEGIN LET value := 'first'; value := 'second'; RETURN value; END;";
    let report = check(&bundle([("client.orna", source)]), &empty_catalogue());
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let function = &report.checked_bundle().unwrap().client_functions()[0];
    let CheckedClientFunctionBody::Procedural {
        locals,
        statements,
        return_expression,
    } = function.body()
    else {
        panic!("expected checked procedural CLIENT body");
    };
    assert_eq!(locals.len(), 1);
    assert_eq!(locals[0].ordinal(), 0);
    assert_eq!(statements.len(), 2);
    assert!(matches!(
        statements[0],
        super::CheckedClientStatement::Let { local: 0, .. }
    ));
    assert!(matches!(
        statements[1],
        super::CheckedClientStatement::Assignment { local: 0, .. }
    ));
    assert!(matches!(
        return_expression,
        super::CheckedClientExpression::LocalRead { local: 0, .. }
    ));
}

#[test]
fn accepts_procedural_scalar_resource_local_await() {
    let base = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes([0x41; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x42; 16]),
            QualifiedSemanticName::new(["tasks"]).unwrap(),
        )],
        Vec::new(),
        vec![FunctionDefinition::new(
            FunctionId::from_bytes([0x43; 16]),
            QualifiedSemanticName::new(["tasks", "find"]).unwrap(),
            FunctionDomain::Server,
            vec![parameter(
                0x44,
                "p_name",
                0,
                ResolvedType::Scalar(StandardScalar::CharacterLargeObject),
            )],
            FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::CharacterLargeObject)),
            FunctionRevisionId::from_bytes([0x45; 16]),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        )],
    )
    .unwrap();
    let source = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.find(p_name TEXT) RETURNS TEXT IS \
            LET rows std.data.Resource<TEXT> := std.data.resource(target => tasks.find, arguments => std.call.args(p_name => p_name)); \
            BEGIN RETURN AWAIT rows; END;";
    let report = check(&bundle([("resource.orna", source)]), &base);
    assert!(
        report.diagnostics().is_empty(),
        "{:#?}",
        report.diagnostics()
    );
    let function = &report
        .checked_bundle()
        .expect("resource source checks")
        .client_functions()[0];
    let CheckedClientFunctionBody::Procedural {
        locals,
        statements,
        return_expression,
    } = function.body()
    else {
        panic!("expected a checked procedural CLIENT body");
    };
    assert_eq!(locals.len(), 1);
    assert_eq!(locals[0].ordinal(), 0);
    assert_eq!(
        locals[0].kind(),
        super::CheckedClientLocalKind::Resource(orna_artifact::client_plan::ResourceKind::Scalar)
    );
    assert_eq!(statements.len(), 1);
    let super::CheckedClientStatement::Let {
        local: 0,
        expression: resource_expression,
    } = &statements[0]
    else {
        panic!("resource local must be initialized by a LET");
    };
    let super::CheckedClientExpression::Resource { operation } = resource_expression else {
        panic!("resource local initializer must be a resource constructor");
    };
    assert_eq!(
        operation.kind(),
        orna_artifact::client_plan::ResourceKind::Scalar
    );
    assert_eq!(
        operation.target(),
        super::CheckedFunctionId::Existing(FunctionId::from_bytes([0x43; 16]))
    );
    let resource_text =
        "std.data.resource(target => tasks.find, arguments => std.call.args(p_name => p_name))";
    let resource_start = source
        .find(resource_text)
        .expect("resource constructor is present");
    assert_eq!(operation.location().logical_path(), "resource.orna");
    assert_eq!(operation.location().span().start(), resource_start);
    assert_eq!(
        operation.location().span().end(),
        resource_start + resource_text.len()
    );
    let argument_location = match &operation.arguments()[0].1 {
        super::CheckedClientExpression::ParameterRead { location, .. } => location,
        _ => panic!("resource argument must retain its parameter-read span"),
    };
    let argument_start = source
        .rfind("p_name")
        .expect("argument parameter read is present");
    assert_eq!(argument_location.logical_path(), "resource.orna");
    assert_eq!(argument_location.span().start(), argument_start);
    assert_eq!(
        argument_location.span().end(),
        argument_start + "p_name".len()
    );

    let super::CheckedClientExpression::Await {
        expression: awaited_expression,
        location: await_location,
    } = return_expression
    else {
        panic!("procedural return must await the resource local");
    };
    let super::CheckedClientExpression::LocalRead {
        local: 0,
        location: local_location,
    } = awaited_expression.as_ref()
    else {
        panic!("await operand must read the resource local");
    };
    let await_text = "AWAIT rows";
    let await_start = source
        .find(await_text)
        .expect("await expression is present");
    assert_eq!(await_location.logical_path(), "resource.orna");
    assert_eq!(await_location.span().start(), await_start);
    assert_eq!(await_location.span().end(), await_start + await_text.len());
    let local_start = source.rfind("rows").expect("await local read is present");
    assert_eq!(local_location.logical_path(), "resource.orna");
    assert_eq!(local_location.span().start(), local_start);
    assert_eq!(local_location.span().end(), local_start + "rows".len());
}

#[test]
fn accepts_scalar_resource_assignment_await_with_exact_spans_and_call_provenance() {
    let target_id = FunctionId::from_bytes([0x43; 16]);
    let base = catalogue(
        vec![schema(0x42, &["tasks"])],
        Vec::new(),
        vec![FunctionDefinition::new(
            target_id,
            QualifiedSemanticName::new(["tasks", "find"]).unwrap(),
            FunctionDomain::Server,
            vec![parameter(
                0x44,
                "p_name",
                0,
                ResolvedType::Scalar(StandardScalar::CharacterLargeObject),
            )],
            FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::CharacterLargeObject)),
            FunctionRevisionId::from_bytes([0x45; 16]),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        )],
    );
    let source = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.find(p_name TEXT) RETURNS TEXT IS \
            LET value TEXT := 'initial'; \
            BEGIN value := AWAIT std.data.resource(target => tasks.find, arguments => std.call.args(p_name => p_name)); RETURN value; END;";
    let report = check(&bundle([("resource-assignment.orna", source)]), &base);
    assert!(
        report.diagnostics().is_empty(),
        "{:#?}",
        report.diagnostics()
    );
    let function = &report
        .checked_bundle()
        .expect("resource assignment source checks")
        .client_functions()[0];
    let CheckedClientFunctionBody::Procedural { statements, .. } = function.body() else {
        panic!("expected a checked procedural CLIENT body");
    };
    assert_eq!(statements.len(), 2);
    let super::CheckedClientStatement::Assignment {
        local: 0,
        expression: assignment_expression,
    } = &statements[1]
    else {
        panic!("second procedural statement must assign the existing local");
    };
    let CheckedClientExpression::Await {
        expression: awaited_expression,
        location: await_location,
    } = assignment_expression
    else {
        panic!("assignment RHS must retain its AWAIT expression");
    };
    let await_text = "AWAIT std.data.resource(target => tasks.find, arguments => std.call.args(p_name => p_name))";
    let await_start = source
        .find(await_text)
        .expect("assignment AWAIT is present");
    assert_eq!(await_location.logical_path(), "resource-assignment.orna");
    assert_eq!(await_location.span().start(), await_start);
    assert_eq!(await_location.span().end(), await_start + await_text.len());

    let CheckedClientExpression::Resource { operation } = awaited_expression.as_ref() else {
        panic!("assignment AWAIT operand must retain its resource operation");
    };
    let resource_text =
        "std.data.resource(target => tasks.find, arguments => std.call.args(p_name => p_name))";
    let resource_start = source
        .find(resource_text)
        .expect("resource constructor is present");
    assert_eq!(
        operation.location().logical_path(),
        "resource-assignment.orna"
    );
    assert_eq!(operation.location().span().start(), resource_start);
    assert_eq!(
        operation.location().span().end(),
        resource_start + resource_text.len()
    );
    let argument_location = match &operation.arguments()[0].1 {
        CheckedClientExpression::ParameterRead { location, .. } => location,
        _ => panic!("resource argument must retain its parameter-read span"),
    };
    let argument_start = source
        .rfind("p_name")
        .expect("argument parameter read is present");
    assert_eq!(argument_location.logical_path(), "resource-assignment.orna");
    assert_eq!(argument_location.span().start(), argument_start);
    assert_eq!(
        argument_location.span().end(),
        argument_start + "p_name".len()
    );

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
    assert_eq!(
        call_reference.location().logical_path(),
        "resource-assignment.orna"
    );
    assert_eq!(call_reference.location().span().start(), resource_start);
    assert_eq!(
        call_reference.location().span().end(),
        resource_start + resource_text.len()
    );
}

#[test]
fn rejects_resource_local_as_action_argument() {
    let resource_target_id = FunctionId::from_bytes([0x71; 16]);
    let action_target_id = FunctionId::from_bytes([0x72; 16]);
    let action_parameter_id = ParameterId::from_bytes([0x73; 16]);
    let integer_type = ResolvedType::Scalar(StandardScalar::Integer);
    let base = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes([0x74; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x75; 16]),
            QualifiedSemanticName::new(["tasks"]).unwrap(),
        )],
        Vec::new(),
        vec![
            FunctionDefinition::new(
                resource_target_id,
                QualifiedSemanticName::new(["tasks", "find"]).unwrap(),
                FunctionDomain::Server,
                Vec::new(),
                FunctionReturn::Single(integer_type),
                FunctionRevisionId::from_bytes([0x76; 16]),
                FunctionSecurity::Invoker,
                None,
                FunctionVolatility::Stable,
            ),
            FunctionDefinition::new(
                action_target_id,
                QualifiedSemanticName::new(["tasks", "run"]).unwrap(),
                FunctionDomain::Client,
                vec![ParameterDefinition::new(
                    action_parameter_id,
                    "p_value",
                    0,
                    integer_type,
                    None,
                )],
                FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Integer)),
                FunctionRevisionId::from_bytes([0x77; 16]),
                FunctionSecurity::Invoker,
                None,
                FunctionVolatility::Immutable,
            ),
        ],
    )
    .unwrap();
    let standard =
        check_standard_library_source(&verified_standard_library_with_action_for_test()).unwrap();
    let context = StandardApplicationCheckContext::try_new(&base, &standard).unwrap();
    let source = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.run() RETURNS std.Action IS \
            LET rows std.data.Resource<INTEGER> := std.data.resource(target => tasks.find, arguments => std.call.args()); \
            BEGIN RETURN std.action.call(target => tasks.run, arguments => std.call.args(p_value => rows)); END;";
    let report = check_standard_application(&bundle([("action-resource.orna", source)]), &context);
    assert_eq!(report.diagnostics().len(), 1, "{:?}", report.diagnostics());
    assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);
    assert_eq!(
        report.diagnostics()[0].message(),
        "std.action.call argument for parameter p_value is not ORV3-encodable"
    );
    assert!(report.checked_bundle().is_none());
}

#[test]
fn rejects_bare_as_and_state_return_await_but_accepts_procedural_await() {
    let base = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes([0x51; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x52; 16]),
            QualifiedSemanticName::new(["tasks"]).unwrap(),
        )],
        Vec::new(),
        vec![FunctionDefinition::new(
            FunctionId::from_bytes([0x53; 16]),
            QualifiedSemanticName::new(["tasks", "find"]).unwrap(),
            FunctionDomain::Server,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::CharacterLargeObject)),
            FunctionRevisionId::from_bytes([0x54; 16]),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        )],
    )
    .unwrap();
    let source = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.bare() RETURNS TEXT AS \
            AWAIT std.data.resource(target => tasks.find, arguments => std.call.args()); \
            CREATE CLIENT FUNCTION ui.procedural() RETURNS TEXT IS \
            LET value std.data.Resource<TEXT> := std.data.resource(target => tasks.find, arguments => std.call.args()); \
            BEGIN RETURN AWAIT value; END; \
            CREATE CLIENT FUNCTION ui.stateful() RETURNS TEXT IS \
            STATE value TEXT; BEGIN RETURN AWAIT std.data.resource(target => tasks.find, arguments => std.call.args()); END;";
    let report = check(&bundle([("await-positions.orna", source)]), &base);
    assert_eq!(report.diagnostics().len(), 2, "{:?}", report.diagnostics());
    let await_starts = [
        source.find("AWAIT").unwrap(),
        source.rfind("AWAIT").unwrap(),
    ];
    for (diagnostic, start) in report.diagnostics().iter().zip(await_starts) {
        assert_eq!(diagnostic.code(), DiagnosticCode::UnexpectedToken);
        assert_eq!(diagnostic.message(), "expected a CLIENT expression");
        assert_eq!(diagnostic.location().logical_path(), "await-positions.orna");
        assert_eq!(diagnostic.location().span().start(), start);
        assert_eq!(diagnostic.location().span().end(), start + "AWAIT".len());
    }
    assert_no_checked_bundle(&report);

    let procedural = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.procedural() RETURNS TEXT IS \
            LET value TEXT := AWAIT std.data.resource(target => tasks.find, arguments => std.call.args()); \
            BEGIN value := AWAIT std.data.resource(target => tasks.find, arguments => std.call.args()); RETURN value; END;";
    let report = check(&bundle([("await-procedural.orna", procedural)]), &base);
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    assert!(report.checked_bundle().is_some());
}

#[test]
fn rejects_scalar_and_stream_resource_descriptor_mismatches() {
    let base = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes([0x61; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x62; 16]),
            QualifiedSemanticName::new(["tasks"]).unwrap(),
        )],
        Vec::new(),
        vec![
            FunctionDefinition::new(
                FunctionId::from_bytes([0x63; 16]),
                QualifiedSemanticName::new(["tasks", "find"]).unwrap(),
                FunctionDomain::Server,
                Vec::new(),
                FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::CharacterLargeObject)),
                FunctionRevisionId::from_bytes([0x64; 16]),
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::ReadOnly),
                FunctionVolatility::Stable,
            ),
            FunctionDefinition::new(
                FunctionId::from_bytes([0x65; 16]),
                QualifiedSemanticName::new(["tasks", "events"]).unwrap(),
                FunctionDomain::Server,
                Vec::new(),
                FunctionReturn::Stream(ResolvedType::Scalar(StandardScalar::CharacterLargeObject)),
                FunctionRevisionId::from_bytes([0x66; 16]),
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::ReadOnly),
                FunctionVolatility::Stable,
            ),
        ],
    )
    .unwrap();
    let source = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.scalar() RETURNS TEXT IS \
            LET value std.data.Resource<INTEGER> := std.data.resource(target => tasks.find, arguments => std.call.args()); \
            BEGIN RETURN AWAIT value; END; \
            CREATE CLIENT FUNCTION ui.stream() RETURNS STREAM<TEXT> IS \
            LET rows std.data.StreamResource<INTEGER> := std.data.stream_resource(target => tasks.events, arguments => std.call.args()); \
            BEGIN RETURN AWAIT rows; END;";
    let report = check(
        &bundle([("resource-descriptor-mismatch.orna", source)]),
        &base,
    );
    assert_eq!(report.diagnostics().len(), 2, "{:?}", report.diagnostics());
    assert!(report.diagnostics().iter().all(|diagnostic| {
        diagnostic.code() == DiagnosticCode::TypeMismatch
            && diagnostic.message().contains("descriptor does not match")
    }));
    assert_no_checked_bundle(&report);
}

#[test]
fn rejects_state_block_pre_begin_let_locals_in_parser_shape() {
    let source = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.mixed() RETURNS BOOLEAN IS \
            STATE value TEXT; LET other TEXT := 'x'; BEGIN RETURN TRUE; END;";
    let report = check(&bundle([("state-shape.orna", source)]), &empty_catalogue());
    assert!(
        report.diagnostics().iter().any(|diagnostic| {
            diagnostic.code() == DiagnosticCode::UnexpectedToken
                && diagnostic.message() == "CLIENT state blocks cannot contain pre-BEGIN LET locals"
        }),
        "{:?}",
        report.diagnostics()
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn rejects_state_blocks_mixed_with_procedural_declarations() {
    let source = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.mixed() RETURNS BOOLEAN IS STATE value TEXT; BEGIN LET other := 'x'; RETURN TRUE; END;";
    let report = check(&bundle([("client.orna", source)]), &empty_catalogue());
    assert!(
        report.diagnostics().iter().any(|diagnostic| {
            diagnostic.code() == DiagnosticCode::UnexpectedToken
                && diagnostic.message()
                    == "CLIENT state blocks accept only a single RETURN statement"
        }),
        "{:?}",
        report.diagnostics()
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn rejects_procedural_unknown_locals_types_and_await_operands() {
    let unknown = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.bad() RETURNS TEXT IS BEGIN missing := 'x'; RETURN 'ok'; END;";
    let report = check(&bundle([("client.orna", unknown)]), &empty_catalogue());
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::UnknownQualifiedName
    );

    let wrong_type = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.bad() RETURNS INTEGER IS BEGIN LET value INTEGER := 'wrong'; RETURN value; END;";
    let report = check(&bundle([("client.orna", wrong_type)]), &empty_catalogue());
    assert!(
        report
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code() == DiagnosticCode::TypeMismatch)
    );

    let bad_await = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.bad() RETURNS TEXT IS BEGIN LET value := AWAIT 1; RETURN value; END;";
    let report = check(&bundle([("client.orna", bad_await)]), &empty_catalogue());
    assert!(
        report
            .diagnostics()
            .iter()
            .any(
                |diagnostic| diagnostic.code() == DiagnosticCode::DomainIncompatible
                    && diagnostic.message().contains("AWAIT requires")
            )
    );
}

#[test]
fn rejects_expression_returns_the_local_evaluator_cannot_execute() {
    let source = "CREATE SCHEMA examples; \
            CREATE TYPE examples.item AS OBJECT (); \
            CREATE CLIENT FUNCTION examples.read(p_item REF examples.item) \
            RETURNS REF examples.item AS p_item;";
    let report = check(&bundle([("client.orna", source)]), &empty_catalogue());

    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);
    assert_eq!(
        report.diagnostics()[0].message(),
        "this CLIENT function return type is not supported by the local evaluator"
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn checks_client_false_and_reuses_active_id_with_quoted_formatting() {
    let changed_source = "CREATE SCHEMA examples;\nCREATE CLIENT FUNCTION \"examples\".\"enabled\"() RETURNS BOOL RETURN false;";
    let existing_function = FunctionDefinition::new(
        FunctionId::from_bytes([8; 16]),
        QualifiedSemanticName::new(["examples", "enabled"]).unwrap(),
        FunctionDomain::Client,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Boolean)),
        FunctionRevisionId::from_bytes([9; 16]),
        FunctionSecurity::Invoker,
        None,
        FunctionVolatility::Immutable,
    );
    let existing_report = check(
        &bundle([("client.orna", changed_source)]),
        &catalogue(
            vec![schema(1, &["examples"])],
            Vec::new(),
            vec![existing_function],
        ),
    );
    assert!(existing_report.diagnostics().is_empty());
    let changed = &existing_report.checked_bundle().unwrap().client_functions()[0];
    assert_eq!(
        changed.id().existing(),
        Some(FunctionId::from_bytes([8; 16]))
    );
    assert_eq!(changed.name().to_string(), "examples.enabled");
    assert!(!changed.boolean_body().unwrap().0);
}

#[test]
fn rejects_client_shape_in_deterministic_order_and_whole_bundle() {
    let source = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.bad(a TEXT) RETURNS TEXT RETURN TRUE; CREATE CLIENT FUNCTION examples.good() RETURNS BOOLEAN RETURN FALSE;";
    let report = check(&bundle([("client.orna", source)]), &empty_catalogue());

    assert_eq!(report.diagnostics().len(), 2);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::DomainIncompatible
    );
    assert_eq!(
        report.diagnostics()[0].message(),
        "this CLIENT function cannot declare parameters yet"
    );
    assert_eq!(
        report.diagnostics()[0].location().span().start(),
        source.find("(a TEXT)").unwrap()
    );
    assert_eq!(
        report.diagnostics()[0].location().span().end(),
        source.find("(a TEXT)").unwrap() + "(a TEXT)".len()
    );
    assert_eq!(report.diagnostics()[1].code(), DiagnosticCode::TypeMismatch);
    assert_eq!(
        report.diagnostics()[1].message(),
        "this CLIENT function must return BOOLEAN"
    );
    assert_eq!(
        report.diagnostics()[1].location().span().start(),
        source.find("RETURNS TEXT").unwrap() + "RETURNS ".len()
    );
    assert_eq!(
        report.diagnostics()[1].location().span().end(),
        source.find("RETURNS TEXT").unwrap() + "RETURNS TEXT".len()
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn rejects_client_server_duplicates_and_active_domain_changes_at_name() {
    let duplicate_source = "CREATE SCHEMA examples; CREATE TYPE examples.flag AS OBJECT (value BOOLEAN); CREATE SERVER FUNCTION examples.enabled() RETURNS ROWS (value BOOLEAN) AS SELECT f.value FROM examples.flag f; CREATE CLIENT FUNCTION examples.ENABLED() RETURNS BOOLEAN RETURN TRUE;";
    let duplicate = check(
        &bundle([("client.orna", duplicate_source)]),
        &empty_catalogue(),
    );
    assert_eq!(duplicate.diagnostics().len(), 1);
    assert_eq!(
        duplicate.diagnostics()[0].code(),
        DiagnosticCode::DuplicateDefinition
    );
    assert_eq!(
        duplicate.diagnostics()[0].message(),
        "duplicate function definition examples.enabled"
    );
    let duplicate_name = duplicate_source.rfind("examples.ENABLED").unwrap();
    assert_eq!(
        duplicate.diagnostics()[0].location().span().start(),
        duplicate_name
    );
    assert_eq!(
        duplicate.diagnostics()[0].location().span().end(),
        duplicate_name + "examples.ENABLED".len()
    );
    assert_no_checked_bundle(&duplicate);

    let reverse_source = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.enabled() RETURNS BOOLEAN RETURN TRUE; CREATE SERVER FUNCTION examples.ENABLED() RETURNS TEXT TRANSACTION MANUAL AS SELECT TRUE FROM examples.flag f;";
    let reverse_duplicate = check(
        &bundle([("client.orna", reverse_source)]),
        &empty_catalogue(),
    );
    assert_eq!(reverse_duplicate.diagnostics().len(), 1);
    assert_eq!(
        reverse_duplicate.diagnostics()[0].code(),
        DiagnosticCode::DuplicateDefinition
    );
    assert_eq!(
        reverse_duplicate.diagnostics()[0].message(),
        "duplicate function definition examples.enabled"
    );
    let reverse_name = reverse_source.rfind("examples.ENABLED").unwrap();
    assert_eq!(
        reverse_duplicate.diagnostics()[0].location().span().start(),
        reverse_name
    );
    assert_eq!(
        reverse_duplicate.diagnostics()[0].location().span().end(),
        reverse_name + "examples.ENABLED".len()
    );
    assert_no_checked_bundle(&reverse_duplicate);

    let base = catalogue(
        vec![schema(1, &["examples"])],
        Vec::new(),
        vec![server_function(
            8,
            &["examples", "enabled"],
            Vec::new(),
            vec![rows_column(
                "value",
                0,
                ResolvedType::scalar(StandardScalar::Boolean),
            )],
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Stable,
        )],
    );
    let changed = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.enabled() RETURNS BOOLEAN RETURN TRUE;";
    let report = check(&bundle([("client.orna", changed)]), &base);
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::DomainIncompatible
    );
    assert_eq!(
        report.diagnostics()[0].message(),
        "this function is already declared as a SERVER function"
    );
    assert_eq!(
        report.diagnostics()[0].location().span().start(),
        changed.find("examples.enabled").unwrap()
    );
    assert_eq!(
        report.diagnostics()[0].location().span().end(),
        changed.find("examples.enabled").unwrap() + "examples.enabled".len()
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn assigns_function_ids_in_shared_source_order_across_domains() {
    let source = "CREATE SCHEMA examples; CREATE TYPE examples.flag AS OBJECT (value BOOLEAN); CREATE CLIENT FUNCTION examples.enabled() RETURNS BOOLEAN RETURN TRUE; CREATE SERVER FUNCTION examples.read() RETURNS ROWS (value BOOLEAN) AS SELECT f.value FROM examples.flag f;";
    let report = check(&bundle([("client.orna", source)]), &empty_catalogue());

    assert!(report.diagnostics().is_empty());
    let checked = report.checked_bundle().unwrap();
    assert_eq!(
        checked.client_functions()[0].id().to_string(),
        "provisional:function:0"
    );
    assert_eq!(
        checked.server_functions()[0].id().to_string(),
        "provisional:function:1"
    );
}

#[test]
fn rejects_client_duplicates_with_normalised_names() {
    let source = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.enabled() RETURNS BOOLEAN RETURN TRUE; CREATE CLIENT FUNCTION examples.ENABLED(p_value TEXT) RETURNS TEXT RETURN FALSE;";
    let report = check(&bundle([("client.orna", source)]), &empty_catalogue());

    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::DuplicateDefinition
    );
    assert_eq!(
        report.diagnostics()[0].message(),
        "duplicate client function definition examples.enabled"
    );
    let duplicate_name = source.rfind("examples.ENABLED").unwrap();
    assert_eq!(
        report.diagnostics()[0].location().span().start(),
        duplicate_name
    );
    assert_eq!(
        report.diagnostics()[0].location().span().end(),
        duplicate_name + "examples.ENABLED".len()
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn rejects_active_client_to_server_domain_change_at_the_function_name() {
    let base = catalogue(
        vec![schema(1, &["examples"])],
        Vec::new(),
        vec![FunctionDefinition::new(
            FunctionId::from_bytes([8; 16]),
            QualifiedSemanticName::new(["examples", "enabled"]).unwrap(),
            FunctionDomain::Client,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Boolean)),
            FunctionRevisionId::from_bytes([9; 16]),
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        )],
    );
    let source = "CREATE SCHEMA examples; CREATE TYPE examples.flag AS OBJECT (value BOOLEAN); CREATE SERVER FUNCTION examples.enabled() RETURNS ROWS (value BOOLEAN) AS SELECT f.value FROM examples.flag f;";
    let report = check(&bundle([("client.orna", source)]), &base);

    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::DomainIncompatible
    );
    assert_eq!(
        report.diagnostics()[0].message(),
        "this function is already declared as a CLIENT function"
    );
    assert_eq!(
        report.diagnostics()[0].location().span().start(),
        source.find("examples.enabled").unwrap()
    );
    assert_eq!(
        report.diagnostics()[0].location().span().end(),
        source.find("examples.enabled").unwrap() + "examples.enabled".len()
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn reports_non_boolean_client_returns_at_the_written_return_shape() {
    let source = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.ui() RETURNS UI RETURN TRUE; CREATE CLIENT FUNCTION examples.rows() RETURNS ROWS () RETURN FALSE;";
    let report = check(&bundle([("client.orna", source)]), &empty_catalogue());

    assert_eq!(report.diagnostics().len(), 2);
    for diagnostic in report.diagnostics() {
        assert_eq!(diagnostic.code(), DiagnosticCode::TypeMismatch);
        assert_eq!(
            diagnostic.message(),
            "this CLIENT function must return BOOLEAN"
        );
    }
    assert_eq!(
        report.diagnostics()[0].location().span().start(),
        source.find("UI").unwrap()
    );
    assert_eq!(
        report.diagnostics()[0].location().span().end(),
        source.find("UI").unwrap() + "UI".len()
    );
    assert_eq!(
        report.diagnostics()[1].location().span().start(),
        source.find("ROWS ()").unwrap()
    );
    assert_eq!(
        report.diagnostics()[1].location().span().end(),
        source.find("ROWS ()").unwrap() + "ROWS ()".len()
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn client_boolean_return_spellings_are_closed_to_boolean_and_bool() {
    let cases = [
        (
            "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.value() RETURNS \"BOOLEAN\" RETURN TRUE;",
            "\"BOOLEAN\"",
        ),
        (
            "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.value() RETURNS boolean_alias RETURN TRUE;",
            "boolean_alias",
        ),
        (
            "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.value() RETURNS std.BOOLEAN RETURN TRUE;",
            "std.BOOLEAN",
        ),
    ];
    for (source, spelling) in cases {
        let report = check(&bundle([("client.orna", source)]), &empty_catalogue());

        assert_eq!(report.diagnostics().len(), 1, "spelling: {spelling}");
        assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);
        assert_eq!(
            report.diagnostics()[0].message(),
            "this CLIENT function must return BOOLEAN"
        );
        let start = source.find(spelling).unwrap();
        assert_eq!(
            report.diagnostics()[0].location().logical_path(),
            "client.orna"
        );
        assert_eq!(report.diagnostics()[0].location().span().start(), start);
        assert_eq!(
            report.diagnostics()[0].location().span().end(),
            start + spelling.len()
        );
        assert_no_checked_bundle(&report);
    }
}

#[test]
fn rejects_protected_type_source_in_global_category_order() {
    let z_source = "EXPORT TYPE app.source TO PRELUDE AS SECOND;\n\
            CREATE TYPE app.second AS VALUE PRIMITIVE KERNEL CONTRACT 'app.second@1' IMMUTABLE PERSISTABLE;\n\
            EXPORT TYPE app.source AS app.second_binding;\n\
            CREATE SCHEMA std;";
    let a_source = "EXPORT TYPE app.source TO PRELUDE AS FIRST;\n\
            CREATE TYPE app.first AS VALUE PRIMITIVE KERNEL CONTRACT 'app.first@1' IMMUTABLE TRANSIENT;\n\
            EXPORT TYPE app.source AS app.first_binding;\n\
            CREATE TYPE StD.first AS OBJECT ();";
    let report = check(
        &bundle([("z.orna", z_source), ("a.orna", a_source)]),
        &empty_catalogue(),
    );

    assert_eq!(report.diagnostics().len(), 8);
    let expected = [
        (
            "z.orna",
            "only the standard library can export a type to the prelude",
            z_source.find("TO PRELUDE").unwrap(),
            "TO PRELUDE".len(),
        ),
        (
            "z.orna",
            "KERNEL CONTRACT is available only to the standard library",
            z_source.find("KERNEL CONTRACT").unwrap(),
            "KERNEL CONTRACT".len(),
        ),
        (
            "z.orna",
            "qualified type exports are available only to the standard library",
            z_source.find("app.second_binding").unwrap(),
            "app.second_binding".len(),
        ),
        (
            "z.orna",
            "the std namespace is owned by the standard library",
            z_source.find("std").unwrap(),
            "std".len(),
        ),
        (
            "a.orna",
            "only the standard library can export a type to the prelude",
            a_source.find("TO PRELUDE").unwrap(),
            "TO PRELUDE".len(),
        ),
        (
            "a.orna",
            "KERNEL CONTRACT is available only to the standard library",
            a_source.find("KERNEL CONTRACT").unwrap(),
            "KERNEL CONTRACT".len(),
        ),
        (
            "a.orna",
            "qualified type exports are available only to the standard library",
            a_source.find("app.first_binding").unwrap(),
            "app.first_binding".len(),
        ),
        (
            "a.orna",
            "the std namespace is owned by the standard library",
            a_source.find("StD.first").unwrap(),
            "StD.first".len(),
        ),
    ];
    for (diagnostic, (path, message, start, length)) in report.diagnostics().iter().zip(expected) {
        assert_eq!(diagnostic.code(), DiagnosticCode::DomainIncompatible);
        assert_eq!(diagnostic.message(), message);
        assert_eq!(diagnostic.location().logical_path(), path);
        assert_eq!(diagnostic.location().span().start(), start);
        assert_eq!(diagnostic.location().span().end(), start + length);
    }
    assert_no_checked_bundle(&report);
}

#[test]
fn rejects_opaque_value_declarations_outside_the_standard_library() {
    let source =
        "CREATE TYPE app.token AS VALUE OPAQUE KERNEL CONTRACT 'app.token@1' IMMUTABLE TRANSIENT;";
    let report = check(&bundle([("opaque.orna", source)]), &empty_catalogue());

    assert_eq!(report.diagnostics().len(), 1);
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(diagnostic.code(), DiagnosticCode::DomainIncompatible);
    assert_eq!(
        diagnostic.message(),
        "KERNEL CONTRACT is available only to the standard library"
    );
    assert_eq!(diagnostic.location().logical_path(), "opaque.orna");
    assert_eq!(
        diagnostic.location().span().start(),
        source.find("KERNEL CONTRACT").unwrap()
    );
    assert_eq!(
        diagnostic.location().span().end(),
        source.find("KERNEL CONTRACT").unwrap() + "KERNEL CONTRACT".len()
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn syntax_errors_precede_protected_primitive_and_export_diagnostics() {
    let source = "CREATE TYPE std.broken AS VALUE PRIMITIVE KERNEL CONTRACT 'std.broken@1' IMMUTABLE;\n\
            CREATE TYPE app.value AS VALUE PRIMITIVE KERNEL CONTRACT 'app.value@1' IMMUTABLE PERSISTABLE;\n\
            EXPORT TYPE app.value AS app.binding;\n\
            EXPORT TYPE app.value TO PRELUDE AS VALUE;";
    let report = check(&bundle([("precedence.orna", source)]), &empty_catalogue());

    assert_eq!(report.diagnostics().len(), 1);
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(diagnostic.code(), DiagnosticCode::UnexpectedToken);
    assert_eq!(
        diagnostic.message(),
        "expected PERSISTABLE or TRANSIENT after IMMUTABLE"
    );
    assert_eq!(diagnostic.location().logical_path(), "precedence.orna");
    assert_eq!(
        diagnostic.location().span().start(),
        source.find(";").unwrap()
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn protects_quoted_std_but_not_uppercase_quoted_std() {
    let source = "CREATE SCHEMA \"std\"; CREATE SCHEMA \"STD\";";
    let report = check(&bundle([("quoted.orna", source)]), &empty_catalogue());

    assert_eq!(report.diagnostics().len(), 1);
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(diagnostic.code(), DiagnosticCode::DomainIncompatible);
    assert_eq!(
        diagnostic.message(),
        "the std namespace is owned by the standard library"
    );
    assert_eq!(
        diagnostic.location().span().start(),
        source.find("\"std\"").unwrap()
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn rejects_every_std_owner_form_at_its_complete_name() {
    let source = "CREATE SCHEMA std;\n\
            CREATE TYPE std.object AS OBJECT ();\n\
            CREATE TYPE std.primitive AS VALUE PRIMITIVE KERNEL CONTRACT 'app.contract@1' IMMUTABLE PERSISTABLE;\n\
            ALTER TYPE std.object RENAME FIELD old TO new;\n\
            CREATE SERVER FUNCTION std.server() RETURNS ROWS (value BOOLEAN) AS SELECT o.value FROM std.object o;\n\
            CREATE CLIENT FUNCTION std.client() RETURNS BOOLEAN RETURN TRUE;\n\
            EXPORT TYPE app.value AS std.binding;";
    let report = check(&bundle([("owners.orna", source)]), &empty_catalogue());

    let expected = [
        (
            "std",
            source.find("CREATE SCHEMA std").unwrap() + "CREATE SCHEMA ".len(),
        ),
        (
            "std.object",
            source.find("CREATE TYPE std.object").unwrap() + "CREATE TYPE ".len(),
        ),
        (
            "std.primitive",
            source.find("CREATE TYPE std.primitive").unwrap() + "CREATE TYPE ".len(),
        ),
        (
            "std.object",
            source.find("ALTER TYPE std.object").unwrap() + "ALTER TYPE ".len(),
        ),
        (
            "std.server",
            source.find("CREATE SERVER FUNCTION std.server").unwrap()
                + "CREATE SERVER FUNCTION ".len(),
        ),
        (
            "std.client",
            source.find("CREATE CLIENT FUNCTION std.client").unwrap()
                + "CREATE CLIENT FUNCTION ".len(),
        ),
        (
            "std.binding",
            source.find("AS std.binding").unwrap() + "AS ".len(),
        ),
    ];
    assert_eq!(report.diagnostics().len(), expected.len());
    for (diagnostic, (name, start)) in report.diagnostics().iter().zip(expected) {
        assert_eq!(diagnostic.code(), DiagnosticCode::DomainIncompatible);
        assert_eq!(
            diagnostic.message(),
            "the std namespace is owned by the standard library"
        );
        assert_eq!(diagnostic.location().span().start(), start);
        assert_eq!(diagnostic.location().span().end(), start + name.len());
    }
    assert_no_checked_bundle(&report);
}

const STD_INVOKE_SOURCE: &str = "CREATE SCHEMA std.invoke;\nCREATE SERVER FUNCTION std.invoke.echo(\n    p_value INTEGER\n)\nRETURNS INTEGER\nSECURITY INVOKER\nTRANSACTION READ ONLY\nVOLATILITY STABLE\nAS\n    SELECT p_value;";
/// The exact retained V2 `std/types.orna` source: the retained
/// `orna.std/1`-shape type declarations for the fixed INTEGER value type.
const STANDARD_V2_TYPES_SOURCE: &str = "CREATE SCHEMA std;CREATE SCHEMA std.types;CREATE TYPE std.types.INTEGER AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.integer@1' IMMUTABLE PERSISTABLE;EXPORT TYPE std.types.INTEGER AS std.INTEGER;EXPORT TYPE std.INTEGER TO PRELUDE AS INTEGER;";

fn standard_parameter_echo_declaration(source: &str) -> ServerFunctionDeclaration {
    let report =
        parse_bundle(&SourceBundle::new([SourceUnit::new("std/invoke.orna", source)]).unwrap());
    assert!(
        report.diagnostics().is_empty(),
        "unexpected parse diagnostics: {:?}",
        report.diagnostics()
    );
    assert_eq!(report.units().len(), 1);
    assert_eq!(report.units()[0].parsed().server_functions().len(), 1);
    report.units()[0].parsed().server_functions()[0].clone()
}

#[allow(clippy::too_many_arguments)]
fn parameter_echo_catalogue(
    with_schema: bool,
    with_integer: bool,
    with_function: bool,
    with_parameter: bool,
) -> CatalogueSnapshot {
    let mut schemas = vec![SchemaDefinition::new(
        SchemaId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x02]),
        QualifiedSemanticName::new(["std", "types"]).unwrap(),
    )];
    if with_schema {
        schemas.push(SchemaDefinition::new(
            STD_INVOKE_SCHEMA_ID,
            QualifiedSemanticName::new(["std", "invoke"]).unwrap(),
        ));
    }
    let value_types = if with_integer {
        vec![ValueTypeDefinition::primitive(
            STD_INTEGER_TYPE_ID,
            QualifiedSemanticName::new(["std", "types", "integer"]).unwrap(),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Persistable,
            "orna.kernel.value.integer@1",
        )]
    } else {
        Vec::new()
    };
    let bindings = if with_integer {
        vec![
            TypeBinding::prelude(
                PreludeTypeName::new(["integer"]).unwrap(),
                STD_INTEGER_TYPE_ID,
            )
            .unwrap(),
        ]
    } else {
        Vec::new()
    };
    let functions = if with_function {
        let parameters = if with_parameter {
            vec![ParameterDefinition::new(
                STD_INVOKE_ECHO_PARAMETER_ID,
                "p_value",
                0,
                ResolvedType::scalar(StandardScalar::Integer),
                None,
            )]
        } else {
            Vec::new()
        };
        vec![FunctionDefinition::new(
            STD_INVOKE_ECHO_FUNCTION_ID,
            QualifiedSemanticName::new(["std", "invoke", "echo"]).unwrap(),
            FunctionDomain::Server,
            parameters,
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Integer)),
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        )]
    } else {
        Vec::new()
    };
    CatalogueSnapshot::new_with_functions_and_types(
        CatalogueRevisionId::from_bytes([0x21; 16]),
        schemas,
        vec![],
        value_types,
        bindings,
        functions,
    )
    .unwrap()
}

fn standard_parameter_echo_catalogue() -> CatalogueSnapshot {
    parameter_echo_catalogue(true, true, true, true)
}

fn standard_parameter_echo_origins(source: &str) -> Vec<DefinitionOrigin> {
    let declaration = standard_parameter_echo_declaration(source);
    let span = |start: usize, end: usize| {
        SourceOrigin::new(
            STD_INVOKE_SOURCE_UNIT_ID,
            u32::try_from(start).unwrap(),
            u32::try_from(end).unwrap(),
        )
        .unwrap()
    };
    let mut origins = vec![DefinitionOrigin::new(
        DefinitionIdentity::Function(STD_INVOKE_ECHO_FUNCTION_ID),
        span(declaration.span.start, declaration.span.end),
    )];
    if let Some(parameter) = declaration.parameters.first() {
        origins.push(DefinitionOrigin::new(
            DefinitionIdentity::Parameter {
                owner: STD_INVOKE_ECHO_FUNCTION_ID,
                parameter: STD_INVOKE_ECHO_PARAMETER_ID,
            },
            span(parameter.span.start, parameter.span.end),
        ));
    }
    origins
}

fn check_echo(source: &str) -> Result<CheckedStandardParameterEcho, StandardLibraryCheckError> {
    let declaration = standard_parameter_echo_declaration(source);
    let catalogue = standard_parameter_echo_catalogue();
    let origins = standard_parameter_echo_origins(source);
    check_standard_parameter_echo(&declaration, &catalogue, &origins, STD_INTEGER_TYPE_ID)
}

#[test]
fn checks_the_exact_standard_parameter_echo_declaration_and_artifact() {
    let checked = check_echo(STD_INVOKE_SOURCE).unwrap();

    assert_eq!(checked.function_id(), STD_INVOKE_ECHO_FUNCTION_ID);
    assert_eq!(checked.parameter_id(), STD_INVOKE_ECHO_PARAMETER_ID);
    assert_eq!(checked.revision_id(), STD_INVOKE_ECHO_FUNCTION_REVISION_ID);

    let artifact = checked.artifact();
    assert_eq!(artifact.kind(), ExecutableArtifactKind::Server);
    assert_eq!(artifact.format(), "orna.server-parameter-echo");
    assert_eq!(artifact.version(), 1);
    let payload = artifact.payload();
    assert_eq!(payload.len(), 44);
    assert_eq!(&payload[0..8], b"ORNAPE\0\0");
    assert_eq!(&payload[8..12], &[0, 0, 0, 1]);
    assert_eq!(
        &payload[12..28],
        &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10]
    );
    assert_eq!(
        &payload[28..44],
        &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x02]
    );
    assert_eq!(
        artifact.content_hash(),
        artifact_payload_digest(payload).unwrap()
    );

    let decoded =
        ServerParameterEcho::decode(payload, STD_INVOKE_ECHO_PARAMETER_ID, STD_INTEGER_TYPE_ID)
            .unwrap();
    assert_eq!(decoded.parameter(), STD_INVOKE_ECHO_PARAMETER_ID);
    assert_eq!(decoded.value_type(), STD_INTEGER_TYPE_ID);

    let references = checked.references();
    assert_eq!(references.len(), 3);
    let parameter_integer_start = STD_INVOKE_SOURCE.find("INTEGER").unwrap();
    let result_integer_start = STD_INVOKE_SOURCE.rfind("INTEGER").unwrap();
    let body_p_value_start = STD_INVOKE_SOURCE.rfind("p_value").unwrap();
    for reference in references {
        assert_eq!(reference.source_function(), STD_INVOKE_ECHO_FUNCTION_ID);
        assert_eq!(
            reference.source_revision(),
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID
        );
        assert_eq!(
            reference.source_origin().source_unit(),
            STD_INVOKE_SOURCE_UNIT_ID
        );
    }
    assert_eq!(references[0].ordinal(), 0);
    assert_eq!(references[0].kind(), DefinitionReferenceKind::NamedType);
    assert_eq!(
        references[0].target(),
        DefinitionReferenceTarget::ValueType(STD_INTEGER_TYPE_ID)
    );
    assert_eq!(
        references[0].source_origin().byte_start(),
        parameter_integer_start as u32
    );
    assert_eq!(
        references[0].source_origin().byte_end(),
        parameter_integer_start as u32 + 7
    );
    assert_eq!(
        &STD_INVOKE_SOURCE[parameter_integer_start..parameter_integer_start + 7],
        "INTEGER"
    );
    assert_eq!(references[1].ordinal(), 1);
    assert_eq!(references[1].kind(), DefinitionReferenceKind::NamedType);
    assert_eq!(
        references[1].target(),
        DefinitionReferenceTarget::ValueType(STD_INTEGER_TYPE_ID)
    );
    assert_eq!(
        references[1].source_origin().byte_start(),
        result_integer_start as u32
    );
    assert_eq!(
        references[1].source_origin().byte_end(),
        result_integer_start as u32 + 7
    );
    assert_eq!(
        &STD_INVOKE_SOURCE[result_integer_start..result_integer_start + 7],
        "INTEGER"
    );
    assert_eq!(references[2].ordinal(), 2);
    assert_eq!(references[2].kind(), DefinitionReferenceKind::ParameterRead);
    assert_eq!(
        references[2].target(),
        DefinitionReferenceTarget::Parameter {
            owner: STD_INVOKE_ECHO_FUNCTION_ID,
            parameter: STD_INVOKE_ECHO_PARAMETER_ID,
        }
    );
    assert_eq!(
        references[2].source_origin().byte_start(),
        body_p_value_start as u32
    );
    assert_eq!(
        references[2].source_origin().byte_end(),
        body_p_value_start as u32 + 7
    );
    assert_eq!(
        &STD_INVOKE_SOURCE[body_p_value_start..body_p_value_start + 7],
        "p_value"
    );
}

#[test]
fn standard_parameter_echo_rejects_every_name_variation() {
    for source in [
        STD_INVOKE_SOURCE.replacen("std.invoke.echo", "std.invoke.other", 1),
        STD_INVOKE_SOURCE.replacen("std.invoke.echo", "std.types.echo", 1),
        STD_INVOKE_SOURCE.replacen("std.invoke.echo", "app.echo", 1),
    ] {
        let error = check_echo(&source).unwrap_err();
        assert!(
            matches!(error, StandardLibraryCheckError::UnexpectedName { .. }),
            "unexpected rejection: {error}"
        );
    }
}

#[test]
fn standard_parameter_echo_rejects_missing_extra_and_different_parameters() {
    let missing =
        check_echo(&STD_INVOKE_SOURCE.replacen("(\n    p_value INTEGER\n)", "()", 1)).unwrap_err();
    assert!(matches!(
        missing,
        StandardLibraryCheckError::UnexpectedParameterCount { actual: 0 }
    ));
    let extra = check_echo(&STD_INVOKE_SOURCE.replacen(
        "(\n    p_value INTEGER\n)",
        "(\n    p_value INTEGER,\n    p_other INTEGER\n)",
        1,
    ))
    .unwrap_err();
    assert!(matches!(
        extra,
        StandardLibraryCheckError::UnexpectedParameterCount { actual: 2 }
    ));
    let renamed = check_echo(&STD_INVOKE_SOURCE.replacen("p_value", "p_other", 1)).unwrap_err();
    assert!(matches!(
        renamed,
        StandardLibraryCheckError::UnexpectedParameterName { actual }
            if actual == "p_other"
    ));
}

#[test]
fn standard_parameter_echo_rejects_parameter_default() {
    let error = check_echo(&STD_INVOKE_SOURCE.replacen(
        "p_value INTEGER\n)",
        "p_value INTEGER DEFAULT 0\n)",
        1,
    ))
    .unwrap_err();
    assert!(matches!(error, StandardLibraryCheckError::ParameterDefault));
}

#[test]
fn standard_parameter_echo_rejects_non_integer_parameter_type() {
    let error = check_echo(&STD_INVOKE_SOURCE.replacen("p_value INTEGER", "p_value BOOLEAN", 1))
        .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::UnexpectedParameterType
    ));
}

#[test]
fn standard_parameter_echo_rejects_rows_and_non_integer_results() {
    let rows = check_echo(&STD_INVOKE_SOURCE.replacen(
        "RETURNS INTEGER\n",
        "RETURNS ROWS (value INTEGER)\n",
        1,
    ))
    .unwrap_err();
    assert!(matches!(
        rows,
        StandardLibraryCheckError::UnexpectedResultShape
    ));
    let boolean = check_echo(&STD_INVOKE_SOURCE.replacen("RETURNS INTEGER", "RETURNS BOOLEAN", 1))
        .unwrap_err();
    assert!(matches!(
        boolean,
        StandardLibraryCheckError::UnexpectedResultType
    ));
}

#[test]
fn standard_parameter_echo_rejects_missing_clauses() {
    let missing_security =
        check_echo(&STD_INVOKE_SOURCE.replacen("SECURITY INVOKER\n", "", 1)).unwrap_err();
    assert!(matches!(
        missing_security,
        StandardLibraryCheckError::MissingSecurity
    ));
    let missing_transaction =
        check_echo(&STD_INVOKE_SOURCE.replacen("TRANSACTION READ ONLY\n", "", 1)).unwrap_err();
    assert!(matches!(
        missing_transaction,
        StandardLibraryCheckError::MissingTransaction
    ));
    let missing_volatility =
        check_echo(&STD_INVOKE_SOURCE.replacen("VOLATILITY STABLE\n", "", 1)).unwrap_err();
    assert!(matches!(
        missing_volatility,
        StandardLibraryCheckError::MissingVolatility
    ));
}

#[test]
fn standard_parameter_echo_rejects_different_clause_values() {
    let definer =
        check_echo(&STD_INVOKE_SOURCE.replacen("SECURITY INVOKER", "SECURITY DEFINER", 1))
            .unwrap_err();
    assert!(matches!(
        definer,
        StandardLibraryCheckError::UnexpectedSecurity {
            actual: SyntaxFunctionSecurity::Definer
        }
    ));
    for (spelling, expected) in [
        ("ATOMIC", SyntaxFunctionTransaction::Atomic),
        ("MANUAL", SyntaxFunctionTransaction::Manual),
    ] {
        let error = check_echo(&STD_INVOKE_SOURCE.replacen(
            "TRANSACTION READ ONLY",
            &format!("TRANSACTION {spelling}"),
            1,
        ))
        .unwrap_err();
        assert!(
            matches!(
                error,
                StandardLibraryCheckError::UnexpectedTransaction { actual }
                    if actual == expected
            ),
            "unexpected rejection: {error}"
        );
    }
    for (spelling, expected) in [
        ("IMMUTABLE", SyntaxFunctionVolatility::Immutable),
        ("VOLATILE", SyntaxFunctionVolatility::Volatile),
    ] {
        let error = check_echo(&STD_INVOKE_SOURCE.replacen(
            "VOLATILITY STABLE",
            &format!("VOLATILITY {spelling}"),
            1,
        ))
        .unwrap_err();
        assert!(
            matches!(
                error,
                StandardLibraryCheckError::UnexpectedVolatility { actual }
                    if actual == expected
            ),
            "unexpected rejection: {error}"
        );
    }
}

#[test]
fn standard_parameter_echo_rejects_capability_clause() {
    let error = check_echo(&STD_INVOKE_SOURCE.replacen(
        "AS\n    SELECT",
        "REQUIRES CAPABILITY std.invoke.audit\nAS\n    SELECT",
        1,
    ))
    .unwrap_err();
    assert!(matches!(error, StandardLibraryCheckError::CapabilityClause));
}

#[test]
fn standard_parameter_echo_rejects_wrong_body_identifier_and_other_bodies() {
    let wrong_identifier =
        check_echo(&STD_INVOKE_SOURCE.replacen("SELECT p_value", "SELECT p_other", 1)).unwrap_err();
    assert!(matches!(
        wrong_identifier,
        StandardLibraryCheckError::UnexpectedBodyIdentifier { actual }
            if actual == "p_other"
    ));
    let other_body = check_echo(&STD_INVOKE_SOURCE.replacen(
        "SELECT p_value;",
        "SELECT i.value FROM std.items i;",
        1,
    ))
    .unwrap_err();
    assert!(matches!(
        other_body,
        StandardLibraryCheckError::UnexpectedBody
    ));
}

#[test]
fn standard_parameter_echo_rejects_missing_fixed_catalogue_identities() {
    let declaration = standard_parameter_echo_declaration(STD_INVOKE_SOURCE);
    let origins = standard_parameter_echo_origins(STD_INVOKE_SOURCE);

    let missing_schema = check_standard_parameter_echo(
        &declaration,
        &parameter_echo_catalogue(false, true, false, false),
        &origins,
        STD_INTEGER_TYPE_ID,
    )
    .unwrap_err();
    assert!(matches!(
        missing_schema,
        StandardLibraryCheckError::MissingSchema
    ));

    // Without the fixed INTEGER value type, `INTEGER` cannot resolve in
    // this catalogue, so the closed rejection is the parameter-type error.
    let missing_integer = check_standard_parameter_echo(
        &declaration,
        &parameter_echo_catalogue(true, false, true, true),
        &origins,
        STD_INTEGER_TYPE_ID,
    )
    .unwrap_err();
    assert!(matches!(
        missing_integer,
        StandardLibraryCheckError::UnexpectedParameterType
    ));

    let missing_function = check_standard_parameter_echo(
        &declaration,
        &parameter_echo_catalogue(true, true, false, false),
        &origins,
        STD_INTEGER_TYPE_ID,
    )
    .unwrap_err();
    assert!(matches!(
        missing_function,
        StandardLibraryCheckError::MissingFunction
    ));

    let missing_parameter = check_standard_parameter_echo(
        &declaration,
        &parameter_echo_catalogue(true, true, true, false),
        &origins,
        STD_INTEGER_TYPE_ID,
    )
    .unwrap_err();
    assert!(matches!(
        missing_parameter,
        StandardLibraryCheckError::MissingParameter
    ));
}

#[test]
fn standard_parameter_echo_rejects_missing_origins() {
    let declaration = standard_parameter_echo_declaration(STD_INVOKE_SOURCE);
    let catalogue = standard_parameter_echo_catalogue();
    let origins = standard_parameter_echo_origins(STD_INVOKE_SOURCE);

    let without_function = origins
        .iter()
        .filter(|origin| {
            origin.identity() != DefinitionIdentity::Function(STD_INVOKE_ECHO_FUNCTION_ID)
        })
        .cloned()
        .collect::<Vec<_>>();
    let error = check_standard_parameter_echo(
        &declaration,
        &catalogue,
        &without_function,
        STD_INTEGER_TYPE_ID,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::MissingFunctionOrigin
    ));

    let without_parameter = origins
        .iter()
        .filter(|origin| {
            origin.identity()
                != DefinitionIdentity::Parameter {
                    owner: STD_INVOKE_ECHO_FUNCTION_ID,
                    parameter: STD_INVOKE_ECHO_PARAMETER_ID,
                }
        })
        .cloned()
        .collect::<Vec<_>>();
    let error = check_standard_parameter_echo(
        &declaration,
        &catalogue,
        &without_parameter,
        STD_INTEGER_TYPE_ID,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::MissingParameterOrigin
    ));
}

const STD_JSON_ENCODE_SOURCE: &str = "CREATE SCHEMA std.json;\nCREATE SERVER FUNCTION std.json.encode(\n    p_value std.json.Value\n)\nRETURNS std.io.ByteStream\nSECURITY INVOKER\nTRANSACTION READ ONLY\nVOLATILITY STABLE\nAS\n    SELECT p_value;";
const STD_TERMINAL_PRESENT_TABLE_SOURCE: &str = "CREATE SCHEMA std.data;\nCREATE SERVER FUNCTION std.terminal.present_table(\n    p_rows std.data.Rows\n)\nRETURNS std.terminal.Document\nSECURITY INVOKER\nTRANSACTION READ ONLY\nVOLATILITY STABLE\nAS\n    SELECT p_rows;";
/// The fixed ADR 0057 `std/present.orna` source-unit identity: `...05`.
const STD_PRESENT_SOURCE_UNIT_ID: SourceUnitId =
    SourceUnitId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x05]);

fn presenter_declaration(source: &str) -> ServerFunctionDeclaration {
    let report =
        parse_bundle(&SourceBundle::new([SourceUnit::new("std/present.orna", source)]).unwrap());
    assert!(
        report.diagnostics().is_empty(),
        "unexpected parse diagnostics: {:?}",
        report.diagnostics()
    );
    assert_eq!(report.units().len(), 1);
    assert_eq!(report.units()[0].parsed().server_functions().len(), 1);
    report.units()[0].parsed().server_functions()[0].clone()
}

#[derive(Clone, Copy)]
enum PresenterKind {
    JsonEncode,
    TerminalPresentTable,
}

#[allow(clippy::too_many_arguments)]
fn presenter_catalogue(
    kind: PresenterKind,
    with_schema: bool,
    with_value_types: bool,
    with_function: bool,
    with_parameter: bool,
    client_domain: bool,
) -> CatalogueSnapshot {
    let (
        schema_id,
        function_id,
        parameter_id,
        revision_id,
        function_parts,
        parameter_name,
        parameter_type_id,
        result_type_id,
    ) = match kind {
        PresenterKind::JsonEncode => (
            STD_JSON_SCHEMA_ID,
            STD_JSON_ENCODE_FUNCTION_ID,
            STD_JSON_ENCODE_PARAMETER_ID,
            STD_JSON_ENCODE_FUNCTION_REVISION_ID,
            ["std", "json", "encode"],
            "p_value",
            STD_JSON_VALUE_TYPE_ID,
            STD_IO_BYTE_STREAM_TYPE_ID,
        ),
        PresenterKind::TerminalPresentTable => (
            STD_TERMINAL_SCHEMA_ID,
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
            STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
            ["std", "terminal", "present_table"],
            "p_rows",
            STD_DATA_ROWS_TYPE_ID,
            STD_TERMINAL_DOCUMENT_TYPE_ID,
        ),
    };
    // When the presenter schema is absent, it still must exist for the
    // value types that live in its namespace, so it moves to a foreign
    // identity and the fixed-identity lookup misses.
    let presenter_schema_id = if with_schema {
        schema_id
    } else {
        SchemaId::from_bytes([0x99; 16])
    };
    let schemas = [
        (STD_TERMINAL_SCHEMA_ID, ["std", "terminal"]),
        (STD_IO_SCHEMA_ID, ["std", "io"]),
        (STD_DATA_SCHEMA_ID, ["std", "data"]),
        (STD_JSON_SCHEMA_ID, ["std", "json"]),
    ]
    .into_iter()
    .map(|(id, parts)| {
        let id = if id == schema_id {
            presenter_schema_id
        } else {
            id
        };
        SchemaDefinition::new(id, QualifiedSemanticName::new(parts).unwrap())
    })
    .collect();
    let value_types = if with_value_types {
        vec![
            ValueTypeDefinition::opaque(
                STD_TERMINAL_DOCUMENT_TYPE_ID,
                QualifiedSemanticName::new(["std", "terminal", "document"]).unwrap(),
                "orna.std.value.terminal-document@1",
            ),
            ValueTypeDefinition::opaque(
                STD_IO_BYTE_STREAM_TYPE_ID,
                QualifiedSemanticName::new(["std", "io", "bytestream"]).unwrap(),
                "orna.std.value.byte-stream@1",
            ),
            ValueTypeDefinition::opaque(
                STD_DATA_ROWS_TYPE_ID,
                QualifiedSemanticName::new(["std", "data", "rows"]).unwrap(),
                "orna.std.value.rows@1",
            ),
            ValueTypeDefinition::opaque(
                STD_JSON_VALUE_TYPE_ID,
                QualifiedSemanticName::new(["std", "json", "value"]).unwrap(),
                "orna.std.value.json@1",
            ),
        ]
    } else {
        Vec::new()
    };
    let functions = if with_function {
        let parameters = if with_parameter {
            vec![ParameterDefinition::new(
                parameter_id,
                parameter_name,
                0,
                ResolvedType::Named(parameter_type_id),
                None,
            )]
        } else {
            Vec::new()
        };
        vec![FunctionDefinition::new(
            function_id,
            QualifiedSemanticName::new(function_parts).unwrap(),
            if client_domain {
                FunctionDomain::Client
            } else {
                FunctionDomain::Server
            },
            parameters,
            FunctionReturn::Single(ResolvedType::Named(result_type_id)),
            revision_id,
            FunctionSecurity::Invoker,
            if client_domain {
                None
            } else {
                Some(FunctionTransaction::ReadOnly)
            },
            FunctionVolatility::Stable,
        )]
    } else {
        Vec::new()
    };
    CatalogueSnapshot::new_with_functions_and_types(
        CatalogueRevisionId::from_bytes([0x21; 16]),
        schemas,
        vec![],
        value_types,
        vec![],
        functions,
    )
    .unwrap()
}

fn json_encode_catalogue(
    with_schema: bool,
    with_value_types: bool,
    with_function: bool,
    with_parameter: bool,
    client_domain: bool,
) -> CatalogueSnapshot {
    presenter_catalogue(
        PresenterKind::JsonEncode,
        with_schema,
        with_value_types,
        with_function,
        with_parameter,
        client_domain,
    )
}

fn present_table_catalogue(
    with_schema: bool,
    with_value_types: bool,
    with_function: bool,
    with_parameter: bool,
    client_domain: bool,
) -> CatalogueSnapshot {
    presenter_catalogue(
        PresenterKind::TerminalPresentTable,
        with_schema,
        with_value_types,
        with_function,
        with_parameter,
        client_domain,
    )
}

fn presenter_origins(
    source: &str,
    function_id: FunctionId,
    parameter_id: ParameterId,
) -> Vec<DefinitionOrigin> {
    let declaration = presenter_declaration(source);
    let span = |start: usize, end: usize| {
        SourceOrigin::new(
            STD_PRESENT_SOURCE_UNIT_ID,
            u32::try_from(start).unwrap(),
            u32::try_from(end).unwrap(),
        )
        .unwrap()
    };
    let mut origins = vec![DefinitionOrigin::new(
        DefinitionIdentity::Function(function_id),
        span(declaration.span.start, declaration.span.end),
    )];
    if let Some(parameter) = declaration.parameters.first() {
        origins.push(DefinitionOrigin::new(
            DefinitionIdentity::Parameter {
                owner: function_id,
                parameter: parameter_id,
            },
            span(parameter.span.start, parameter.span.end),
        ));
    }
    origins
}

fn check_json_encode(source: &str) -> Result<CheckedStandardJsonEncode, StandardLibraryCheckError> {
    let declaration = presenter_declaration(source);
    let catalogue = json_encode_catalogue(true, true, true, true, false);
    let origins = presenter_origins(
        source,
        STD_JSON_ENCODE_FUNCTION_ID,
        STD_JSON_ENCODE_PARAMETER_ID,
    );
    check_standard_json_encode(&declaration, &catalogue, &origins, STD_JSON_VALUE_TYPE_ID)
}

fn check_present_table(
    source: &str,
) -> Result<CheckedStandardTerminalPresentTable, StandardLibraryCheckError> {
    let declaration = presenter_declaration(source);
    let catalogue = present_table_catalogue(true, true, true, true, false);
    let origins = presenter_origins(
        source,
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
    );
    check_standard_terminal_present_table(&declaration, &catalogue, &origins, STD_DATA_ROWS_TYPE_ID)
}

#[test]
fn checks_the_exact_json_encode_presenter_declaration() {
    let checked = check_json_encode(STD_JSON_ENCODE_SOURCE).unwrap();
    assert_eq!(checked.function_id(), STD_JSON_ENCODE_FUNCTION_ID);
    assert_eq!(checked.parameter_id(), STD_JSON_ENCODE_PARAMETER_ID);
    assert_eq!(checked.revision_id(), STD_JSON_ENCODE_FUNCTION_REVISION_ID);
}
#[test]
fn rejects_a_tampered_json_executable_record() {
    let declaration = presenter_declaration(STD_JSON_ENCODE_SOURCE);
    let catalogue = json_encode_catalogue(true, true, true, true, false);
    let origins = presenter_origins(
        STD_JSON_ENCODE_SOURCE,
        STD_JSON_ENCODE_FUNCTION_ID,
        STD_JSON_ENCODE_PARAMETER_ID,
    );
    let stored_unit = stored_v2_unit(
        STD_PRESENT_SOURCE_UNIT_ID,
        0,
        "std/present.orna",
        STD_JSON_ENCODE_SOURCE,
    );
    let expected =
        expected_standard_json_executable(&declaration, &catalogue, &origins, &stored_unit)
            .expect("the canonical JSON executable is valid");
    let revision = expected.revision();
    let tampered_revision = FunctionRevisionRecord::new(
        revision.function(),
        revision.id(),
        revision.revision_number() + 1,
        revision.declaration_origin(),
        revision.declaration_content_hash(),
        revision.semantic_hash(),
        revision.language_version(),
        revision.artifact().clone(),
    )
    .expect("the tampered revision remains structurally valid")
    .with_semantic_hash_version(revision.semantic_hash_version());
    let tampered = StandardExecutable::new(
        expected.function(),
        tampered_revision,
        expected.references().to_vec(),
    )
    .expect("the tampered executable remains structurally valid");

    let error = reconcile_standard_json_executable(
        &tampered,
        &declaration,
        &catalogue,
        &origins,
        &stored_unit,
    )
    .expect_err("the checker must reject a tampered JSON executable");
    assert!(matches!(
        error,
        StandardLibraryCheckError::ExecutableMismatch
    ));
}

#[test]
fn checks_the_exact_terminal_present_table_presenter_declaration() {
    let checked = check_present_table(STD_TERMINAL_PRESENT_TABLE_SOURCE).unwrap();
    assert_eq!(
        checked.function_id(),
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID
    );
    assert_eq!(
        checked.parameter_id(),
        STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID
    );
    assert_eq!(
        checked.revision_id(),
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID
    );
}

#[test]
fn presenter_rejects_every_name_variation() {
    for source in [
        STD_JSON_ENCODE_SOURCE.replacen("std.json.encode", "std.json.other", 1),
        STD_JSON_ENCODE_SOURCE.replacen("std.json.encode", "std.io.encode", 1),
        STD_JSON_ENCODE_SOURCE.replacen("std.json.encode", "app.encode", 1),
    ] {
        let error = check_json_encode(&source).unwrap_err();
        assert!(
            matches!(
                error,
                StandardLibraryCheckError::PresenterUnexpectedName { .. }
            ),
            "unexpected rejection: {error}"
        );
    }
    for source in [
        STD_TERMINAL_PRESENT_TABLE_SOURCE.replacen(
            "std.terminal.present_table",
            "std.terminal.render",
            1,
        ),
        STD_TERMINAL_PRESENT_TABLE_SOURCE.replacen(
            "std.terminal.present_table",
            "std.data.present_table",
            1,
        ),
        STD_TERMINAL_PRESENT_TABLE_SOURCE.replacen("std.terminal.present_table", "app.table", 1),
    ] {
        let error = check_present_table(&source).unwrap_err();
        assert!(
            matches!(
                error,
                StandardLibraryCheckError::PresenterUnexpectedName { .. }
            ),
            "unexpected rejection: {error}"
        );
    }
}

#[test]
fn presenter_rejects_missing_extra_and_different_parameters() {
    let missing = check_json_encode(&STD_JSON_ENCODE_SOURCE.replacen(
        "(\n    p_value std.json.Value\n)",
        "()",
        1,
    ))
    .unwrap_err();
    assert!(matches!(
        missing,
        StandardLibraryCheckError::PresenterUnexpectedParameterCount { actual: 0 }
    ));
    let extra = check_json_encode(&STD_JSON_ENCODE_SOURCE.replacen(
        "(\n    p_value std.json.Value\n)",
        "(\n    p_value std.json.Value,\n    p_extra std.json.Value\n)",
        1,
    ))
    .unwrap_err();
    assert!(matches!(
        extra,
        StandardLibraryCheckError::PresenterUnexpectedParameterCount { actual: 2 }
    ));
    let renamed =
        check_json_encode(&STD_JSON_ENCODE_SOURCE.replacen("p_value", "p_other", 1)).unwrap_err();
    assert!(matches!(
        renamed,
        StandardLibraryCheckError::PresenterUnexpectedParameterName { expected, actual }
            if expected == "p_value" && actual == "p_other"
    ));
    let rows_renamed =
        check_present_table(&STD_TERMINAL_PRESENT_TABLE_SOURCE.replacen("p_rows", "p_other", 1))
            .unwrap_err();
    assert!(matches!(
        rows_renamed,
        StandardLibraryCheckError::PresenterUnexpectedParameterName { expected, actual }
            if expected == "p_rows" && actual == "p_other"
    ));
}

#[test]
fn presenter_rejects_parameter_default() {
    let error = check_json_encode(&STD_JSON_ENCODE_SOURCE.replacen(
        "p_value std.json.Value\n)",
        "p_value std.json.Value DEFAULT 0\n)",
        1,
    ))
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::PresenterParameterDefault
    ));
}

#[test]
fn presenter_rejects_wrong_parameter_and_result_types() {
    let wrong_parameter =
        check_json_encode(&STD_JSON_ENCODE_SOURCE.replacen("std.json.Value", "BOOLEAN", 1))
            .unwrap_err();
    assert!(matches!(
        wrong_parameter,
        StandardLibraryCheckError::PresenterUnexpectedParameterType { .. }
    ));
    let rows = check_json_encode(&STD_JSON_ENCODE_SOURCE.replacen(
        "RETURNS std.io.ByteStream\n",
        "RETURNS ROWS (value std.json.Value)\n",
        1,
    ))
    .unwrap_err();
    assert!(matches!(
        rows,
        StandardLibraryCheckError::PresenterUnexpectedResultShape
    ));
    let wrong_result = check_json_encode(&STD_JSON_ENCODE_SOURCE.replacen(
        "RETURNS std.io.ByteStream",
        "RETURNS std.terminal.Document",
        1,
    ))
    .unwrap_err();
    assert!(matches!(
        wrong_result,
        StandardLibraryCheckError::PresenterUnexpectedResultType { .. }
    ));
    let table_wrong_parameter = check_present_table(&STD_TERMINAL_PRESENT_TABLE_SOURCE.replacen(
        "std.data.Rows",
        "std.terminal.Document",
        1,
    ))
    .unwrap_err();
    assert!(matches!(
        table_wrong_parameter,
        StandardLibraryCheckError::PresenterUnexpectedParameterType { .. }
    ));
    let table_wrong_result = check_present_table(&STD_TERMINAL_PRESENT_TABLE_SOURCE.replacen(
        "RETURNS std.terminal.Document",
        "RETURNS std.io.ByteStream",
        1,
    ))
    .unwrap_err();
    assert!(matches!(
        table_wrong_result,
        StandardLibraryCheckError::PresenterUnexpectedResultType { .. }
    ));
}

#[test]
fn presenter_rejects_missing_clauses() {
    let missing_security =
        check_json_encode(&STD_JSON_ENCODE_SOURCE.replacen("SECURITY INVOKER\n", "", 1))
            .unwrap_err();
    assert!(matches!(
        missing_security,
        StandardLibraryCheckError::PresenterMissingSecurity
    ));
    let missing_transaction =
        check_json_encode(&STD_JSON_ENCODE_SOURCE.replacen("TRANSACTION READ ONLY\n", "", 1))
            .unwrap_err();
    assert!(matches!(
        missing_transaction,
        StandardLibraryCheckError::PresenterMissingTransaction
    ));
    let missing_volatility =
        check_json_encode(&STD_JSON_ENCODE_SOURCE.replacen("VOLATILITY STABLE\n", "", 1))
            .unwrap_err();
    assert!(matches!(
        missing_volatility,
        StandardLibraryCheckError::PresenterMissingVolatility
    ));
    let table_missing_security = check_present_table(&STD_TERMINAL_PRESENT_TABLE_SOURCE.replacen(
        "SECURITY INVOKER\n",
        "",
        1,
    ))
    .unwrap_err();
    assert!(matches!(
        table_missing_security,
        StandardLibraryCheckError::PresenterMissingSecurity
    ));
    let table_missing_transaction = check_present_table(
        &STD_TERMINAL_PRESENT_TABLE_SOURCE.replacen("TRANSACTION READ ONLY\n", "", 1),
    )
    .unwrap_err();
    assert!(matches!(
        table_missing_transaction,
        StandardLibraryCheckError::PresenterMissingTransaction
    ));
    let table_missing_volatility = check_present_table(
        &STD_TERMINAL_PRESENT_TABLE_SOURCE.replacen("VOLATILITY STABLE\n", "", 1),
    )
    .unwrap_err();
    assert!(matches!(
        table_missing_volatility,
        StandardLibraryCheckError::PresenterMissingVolatility
    ));
}

#[test]
fn presenter_rejects_different_clause_values() {
    let definer = check_json_encode(&STD_JSON_ENCODE_SOURCE.replacen(
        "SECURITY INVOKER",
        "SECURITY DEFINER",
        1,
    ))
    .unwrap_err();
    assert!(matches!(
        definer,
        StandardLibraryCheckError::PresenterUnexpectedSecurity {
            actual: SyntaxFunctionSecurity::Definer
        }
    ));
    for (spelling, expected) in [
        ("ATOMIC", SyntaxFunctionTransaction::Atomic),
        ("MANUAL", SyntaxFunctionTransaction::Manual),
    ] {
        let error = check_json_encode(&STD_JSON_ENCODE_SOURCE.replacen(
            "TRANSACTION READ ONLY",
            &format!("TRANSACTION {spelling}"),
            1,
        ))
        .unwrap_err();
        assert!(
            matches!(
                error,
                StandardLibraryCheckError::PresenterUnexpectedTransaction { actual }
                    if actual == expected
            ),
            "unexpected rejection: {error}"
        );
    }
    for (spelling, expected) in [
        ("IMMUTABLE", SyntaxFunctionVolatility::Immutable),
        ("VOLATILE", SyntaxFunctionVolatility::Volatile),
    ] {
        let error = check_present_table(&STD_TERMINAL_PRESENT_TABLE_SOURCE.replacen(
            "VOLATILITY STABLE",
            &format!("VOLATILITY {spelling}"),
            1,
        ))
        .unwrap_err();
        assert!(
            matches!(
                error,
                StandardLibraryCheckError::PresenterUnexpectedVolatility { actual }
                    if actual == expected
            ),
            "unexpected rejection: {error}"
        );
    }
}

#[test]
fn presenter_rejects_capability_clause() {
    let error = check_json_encode(&STD_JSON_ENCODE_SOURCE.replacen(
        "AS\n    SELECT",
        "REQUIRES CAPABILITY std.invoke.audit\nAS\n    SELECT",
        1,
    ))
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::PresenterCapabilityClause
    ));
    let table_error = check_present_table(&STD_TERMINAL_PRESENT_TABLE_SOURCE.replacen(
        "AS\n    SELECT",
        "REQUIRES CAPABILITY std.invoke.audit\nAS\n    SELECT",
        1,
    ))
    .unwrap_err();
    assert!(matches!(
        table_error,
        StandardLibraryCheckError::PresenterCapabilityClause
    ));
}

#[test]
fn presenter_rejects_wrong_body_identifier_and_other_bodies() {
    let wrong_identifier =
        check_json_encode(&STD_JSON_ENCODE_SOURCE.replacen("SELECT p_value", "SELECT p_other", 1))
            .unwrap_err();
    assert!(matches!(
        wrong_identifier,
        StandardLibraryCheckError::PresenterUnexpectedBodyIdentifier { expected, actual }
            if expected == "p_value" && actual == "p_other"
    ));
    let table_wrong_identifier = check_present_table(&STD_TERMINAL_PRESENT_TABLE_SOURCE.replacen(
        "SELECT p_rows",
        "SELECT p_other",
        1,
    ))
    .unwrap_err();
    assert!(matches!(
        table_wrong_identifier,
        StandardLibraryCheckError::PresenterUnexpectedBodyIdentifier { expected, actual }
            if expected == "p_rows" && actual == "p_other"
    ));
    let other_body = check_json_encode(&STD_JSON_ENCODE_SOURCE.replacen(
        "SELECT p_value;",
        "SELECT i.value FROM std.items i;",
        1,
    ))
    .unwrap_err();
    assert!(matches!(
        other_body,
        StandardLibraryCheckError::PresenterUnexpectedBody
    ));
    let table_other_body = check_present_table(&STD_TERMINAL_PRESENT_TABLE_SOURCE.replacen(
        "SELECT p_rows;",
        "SELECT i.value FROM std.items i;",
        1,
    ))
    .unwrap_err();
    assert!(matches!(
        table_other_body,
        StandardLibraryCheckError::PresenterUnexpectedBody
    ));
}

#[test]
fn presenter_rejects_missing_fixed_catalogue_identities() {
    let declaration = presenter_declaration(STD_JSON_ENCODE_SOURCE);
    let origins = presenter_origins(
        STD_JSON_ENCODE_SOURCE,
        STD_JSON_ENCODE_FUNCTION_ID,
        STD_JSON_ENCODE_PARAMETER_ID,
    );

    let missing_schema = check_standard_json_encode(
        &declaration,
        &json_encode_catalogue(false, true, true, true, false),
        &origins,
        STD_JSON_VALUE_TYPE_ID,
    )
    .unwrap_err();
    assert!(matches!(
        missing_schema,
        StandardLibraryCheckError::PresenterMissingSchema
    ));

    // Without the fixed value types, `std.json.Value` cannot resolve in
    // this catalogue, so the closed rejection is the parameter-type error.
    let missing_value_types = check_standard_json_encode(
        &declaration,
        &json_encode_catalogue(true, false, true, true, false),
        &origins,
        STD_JSON_VALUE_TYPE_ID,
    )
    .unwrap_err();
    assert!(matches!(
        missing_value_types,
        StandardLibraryCheckError::PresenterUnexpectedParameterType { .. }
    ));

    let missing_function = check_standard_json_encode(
        &declaration,
        &json_encode_catalogue(true, true, false, false, false),
        &origins,
        STD_JSON_VALUE_TYPE_ID,
    )
    .unwrap_err();
    assert!(matches!(
        missing_function,
        StandardLibraryCheckError::PresenterMissingFunction
    ));

    let missing_parameter = check_standard_json_encode(
        &declaration,
        &json_encode_catalogue(true, true, true, false, false),
        &origins,
        STD_JSON_VALUE_TYPE_ID,
    )
    .unwrap_err();
    assert!(matches!(
        missing_parameter,
        StandardLibraryCheckError::PresenterMissingParameter
    ));
}

#[test]
fn presenter_rejects_client_domain_function() {
    let declaration = presenter_declaration(STD_JSON_ENCODE_SOURCE);
    let origins = presenter_origins(
        STD_JSON_ENCODE_SOURCE,
        STD_JSON_ENCODE_FUNCTION_ID,
        STD_JSON_ENCODE_PARAMETER_ID,
    );
    let error = check_standard_json_encode(
        &declaration,
        &json_encode_catalogue(true, true, true, true, true),
        &origins,
        STD_JSON_VALUE_TYPE_ID,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::PresenterUnexpectedDomain {
            actual: FunctionDomain::Client
        }
    ));

    let table_declaration = presenter_declaration(STD_TERMINAL_PRESENT_TABLE_SOURCE);
    let table_origins = presenter_origins(
        STD_TERMINAL_PRESENT_TABLE_SOURCE,
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
    );
    let table_error = check_standard_terminal_present_table(
        &table_declaration,
        &present_table_catalogue(true, true, true, true, true),
        &table_origins,
        STD_DATA_ROWS_TYPE_ID,
    )
    .unwrap_err();
    assert!(matches!(
        table_error,
        StandardLibraryCheckError::PresenterUnexpectedDomain {
            actual: FunctionDomain::Client
        }
    ));
}

#[test]
fn presenter_rejects_missing_origins() {
    let declaration = presenter_declaration(STD_JSON_ENCODE_SOURCE);
    let catalogue = json_encode_catalogue(true, true, true, true, false);
    let origins = presenter_origins(
        STD_JSON_ENCODE_SOURCE,
        STD_JSON_ENCODE_FUNCTION_ID,
        STD_JSON_ENCODE_PARAMETER_ID,
    );

    let without_function = origins
        .iter()
        .filter(|origin| {
            origin.identity() != DefinitionIdentity::Function(STD_JSON_ENCODE_FUNCTION_ID)
        })
        .cloned()
        .collect::<Vec<_>>();
    let error = check_standard_json_encode(
        &declaration,
        &catalogue,
        &without_function,
        STD_JSON_VALUE_TYPE_ID,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::PresenterMissingFunctionOrigin
    ));

    let without_parameter = origins
        .iter()
        .filter(|origin| {
            origin.identity()
                != DefinitionIdentity::Parameter {
                    owner: STD_JSON_ENCODE_FUNCTION_ID,
                    parameter: STD_JSON_ENCODE_PARAMETER_ID,
                }
        })
        .cloned()
        .collect::<Vec<_>>();
    let error = check_standard_json_encode(
        &declaration,
        &catalogue,
        &without_parameter,
        STD_JSON_VALUE_TYPE_ID,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::PresenterMissingParameterOrigin
    ));

    let table_declaration = presenter_declaration(STD_TERMINAL_PRESENT_TABLE_SOURCE);
    let table_catalogue = present_table_catalogue(true, true, true, true, false);
    let table_origins = presenter_origins(
        STD_TERMINAL_PRESENT_TABLE_SOURCE,
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
    );
    let table_without_function = table_origins
        .iter()
        .filter(|origin| {
            origin.identity()
                != DefinitionIdentity::Function(STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID)
        })
        .cloned()
        .collect::<Vec<_>>();
    let error = check_standard_terminal_present_table(
        &table_declaration,
        &table_catalogue,
        &table_without_function,
        STD_DATA_ROWS_TYPE_ID,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::PresenterMissingFunctionOrigin
    ));
}

#[test]
fn presenter_rejects_origin_source_unit_mismatch() {
    let declaration = presenter_declaration(STD_JSON_ENCODE_SOURCE);
    let catalogue = json_encode_catalogue(true, true, true, true, false);
    let origins = presenter_origins(
        STD_JSON_ENCODE_SOURCE,
        STD_JSON_ENCODE_FUNCTION_ID,
        STD_JSON_ENCODE_PARAMETER_ID,
    );
    let moved = origins
        .iter()
        .map(|origin| {
            if origin.identity()
                == (DefinitionIdentity::Parameter {
                    owner: STD_JSON_ENCODE_FUNCTION_ID,
                    parameter: STD_JSON_ENCODE_PARAMETER_ID,
                })
            {
                DefinitionOrigin::new(
                    origin.identity(),
                    SourceOrigin::new(
                        SourceUnitId::from_bytes([0x42; 16]),
                        origin.source().byte_start(),
                        origin.source().byte_end(),
                    )
                    .unwrap(),
                )
            } else {
                origin.clone()
            }
        })
        .collect::<Vec<_>>();
    let error =
        check_standard_json_encode(&declaration, &catalogue, &moved, STD_JSON_VALUE_TYPE_ID)
            .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::OriginSourceUnitMismatch
    ));
}

#[test]
fn application_checker_accepts_scalar_server_select_and_retains_return_identity() {
    let source = "CREATE SCHEMA app;\nCREATE TYPE app.item AS OBJECT (value INTEGER);\nCREATE SERVER FUNCTION app.echo()\nRETURNS INTEGER\nSECURITY INVOKER\nTRANSACTION READ ONLY\nVOLATILITY STABLE\nAS\n    SELECT i.value FROM app.item i;";
    let report = check(&bundle([("app.orna", source)]), &empty_catalogue());

    assert_eq!(report.diagnostics(), &[]);
    let function = &report.checked_bundle().unwrap().server_functions()[0];
    assert!(matches!(
        function.return_type(),
        super::CheckedServerFunctionReturn::Single {
            semantic_type: SemanticType::Scalar(StandardScalar::Integer),
            standard_value_type: None,
            ..
        }
    ));
}

#[test]
fn scalar_server_select_rejects_projection_count_mismatch() {
    let source = "CREATE SCHEMA app; CREATE TYPE app.item AS OBJECT (value INTEGER); CREATE SERVER FUNCTION app.echo() RETURNS INTEGER SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT i.value, i.value FROM app.item i;";
    let report = check(&bundle([("app.orna", source)]), &empty_catalogue());

    assert!(
        report
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code() == DiagnosticCode::TypeMismatch)
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn scalar_server_select_rejects_projection_type_mismatch() {
    let source = "CREATE SCHEMA app; CREATE TYPE app.item AS OBJECT (value INTEGER); CREATE SERVER FUNCTION app.echo() RETURNS TEXT SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT i.value FROM app.item i;";
    let report = check(&bundle([("app.orna", source)]), &empty_catalogue());

    assert!(
        report
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code() == DiagnosticCode::TypeMismatch)
    );
    assert_no_checked_bundle(&report);
}

fn stored_v2_unit(id: SourceUnitId, ordinal: u32, path: &str, content: &str) -> StoredSourceUnit {
    StoredSourceUnit::new(
        id,
        ordinal,
        path,
        content,
        source_unit_content_digest(content).unwrap(),
    )
    .unwrap()
}

fn standard_v2_types_catalogue() -> CatalogueSnapshot {
    let integer = ValueTypeDefinition::primitive(
        STD_INTEGER_TYPE_ID,
        QualifiedSemanticName::new(["std", "types", "integer"]).unwrap(),
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Persistable,
        "orna.kernel.value.integer@1",
    );
    let qualified = TypeBinding::qualified(
        QualifiedSemanticName::new(["std", "integer"]).unwrap(),
        integer.id(),
    )
    .unwrap();
    let prelude =
        TypeBinding::prelude(PreludeTypeName::new(["integer"]).unwrap(), integer.id()).unwrap();
    CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([0x21; 16]),
        vec![
            SchemaDefinition::new(
                SchemaId::from_bytes([1; 16]),
                QualifiedSemanticName::new(["std"]).unwrap(),
            ),
            SchemaDefinition::new(
                SchemaId::from_bytes([2; 16]),
                QualifiedSemanticName::new(["std", "types"]).unwrap(),
            ),
        ],
        vec![],
        vec![integer],
        vec![qualified, prelude],
    )
    .unwrap()
}

fn standard_v2_catalogue(with_invoke: bool) -> CatalogueSnapshot {
    let catalogue = standard_v2_types_catalogue();
    if !with_invoke {
        return catalogue;
    }
    let echo = FunctionDefinition::new(
        STD_INVOKE_ECHO_FUNCTION_ID,
        QualifiedSemanticName::new(["std", "invoke", "echo"]).unwrap(),
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            STD_INVOKE_ECHO_PARAMETER_ID,
            "p_value",
            0,
            ResolvedType::scalar(StandardScalar::Integer),
            None,
        )],
        FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Integer)),
        STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    let mut schemas = catalogue.schemas().to_vec();
    schemas.push(SchemaDefinition::new(
        STD_INVOKE_SCHEMA_ID,
        QualifiedSemanticName::new(["std", "invoke"]).unwrap(),
    ));
    CatalogueSnapshot::new_with_functions_and_types(
        CatalogueRevisionId::from_bytes([0x21; 16]),
        schemas,
        vec![],
        catalogue.value_types().to_vec(),
        catalogue.type_bindings().to_vec(),
        vec![echo],
    )
    .unwrap()
}

fn standard_v2_types_origins(
    catalogue: &CatalogueSnapshot,
    parsed: &ParsedSourceUnit,
) -> Vec<DefinitionOrigin> {
    let origin = |identity: DefinitionIdentity, span: &SourceSpan| -> DefinitionOrigin {
        DefinitionOrigin::new(
            identity,
            SourceOrigin::new(
                STD_TYPES_SOURCE_UNIT_ID,
                u32::try_from(span.start).unwrap(),
                u32::try_from(span.end).unwrap(),
            )
            .unwrap(),
        )
    };
    let mut origins = Vec::new();
    for declaration in parsed.parsed().schemas() {
        let name = unquoted_semantic_name(&declaration.name).unwrap();
        let definition = catalogue.schema_by_name(&name).unwrap();
        origins.push(origin(
            DefinitionIdentity::Schema(definition.id()),
            &declaration.span,
        ));
    }
    for declaration in parsed.parsed().primitive_value_types() {
        let name = unquoted_semantic_name(&declaration.name).unwrap();
        let definition = catalogue.value_type_by_name(&name).unwrap();
        origins.push(origin(
            DefinitionIdentity::ValueType(definition.id()),
            &declaration.span,
        ));
    }
    for declaration in parsed.parsed().type_exports() {
        let target = match &declaration.target {
            TypeExportTarget::Qualified { name } => {
                TypeLookupName::qualified(unquoted_semantic_name(name).unwrap())
            }
            TypeExportTarget::Prelude { words, .. } => {
                TypeLookupName::prelude(unquoted_prelude_name(words).unwrap())
            }
        };
        let binding = catalogue.type_binding_by_name(&target).unwrap();
        origins.push(origin(
            DefinitionIdentity::TypeBinding(binding.id()),
            &declaration.span,
        ));
    }
    origins
}

fn standard_v2_invoke_origins(source: &str) -> Vec<DefinitionOrigin> {
    let report =
        parse_bundle(&SourceBundle::new([SourceUnit::new("std/invoke.orna", source)]).unwrap());
    assert!(report.diagnostics().is_empty());
    let parsed = &report.units()[0];
    let schema_span = &parsed.parsed().schemas()[0].span;
    let mut origins = standard_parameter_echo_origins(source);
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::Schema(STD_INVOKE_SCHEMA_ID),
        SourceOrigin::new(
            STD_INVOKE_SOURCE_UNIT_ID,
            u32::try_from(schema_span.start).unwrap(),
            u32::try_from(schema_span.end).unwrap(),
        )
        .unwrap(),
    ));
    origins
}

fn standard_v2_executable(
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
) -> StandardExecutable {
    let checked = check_echo(STD_INVOKE_SOURCE).unwrap();
    let function = catalogue
        .function_by_id(STD_INVOKE_ECHO_FUNCTION_ID)
        .unwrap();
    let function_origin = origins
        .iter()
        .find(|origin| {
            origin.identity() == DefinitionIdentity::Function(STD_INVOKE_ECHO_FUNCTION_ID)
        })
        .unwrap()
        .source();
    let declaration_content_hash = function_declaration_digest(
        &STD_INVOKE_SOURCE.as_bytes()
            [function_origin.byte_start() as usize..function_origin.byte_end() as usize],
    )
    .unwrap();
    let semantic = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        function,
        "orna.language/1",
        checked.artifact(),
        &[],
        checked.references(),
    )
    .unwrap();
    let revision = FunctionRevisionRecord::new(
        checked.function_id(),
        checked.revision_id(),
        STD_INVOKE_ECHO_REVISION_NUMBER,
        function_origin,
        declaration_content_hash,
        semantic,
        "orna.language/1",
        checked.artifact().clone(),
    )
    .unwrap()
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    StandardExecutable::new(
        checked.function_id(),
        revision,
        checked.references().to_vec(),
    )
    .unwrap()
}

fn standard_v2_units() -> (StoredSourceUnit, StoredSourceUnit) {
    (
        stored_v2_unit(
            STD_TYPES_SOURCE_UNIT_ID,
            0,
            "std/types.orna",
            STANDARD_V2_TYPES_SOURCE,
        ),
        stored_v2_unit(
            STD_INVOKE_SOURCE_UNIT_ID,
            1,
            "std/invoke.orna",
            STD_INVOKE_SOURCE,
        ),
    )
}

/// The compiled canonical V2 standard-library digest for the exact test
/// inputs (`STANDARD_V2_TYPES_SOURCE`, `STD_INVOKE_SOURCE`, the fixed
/// identities, catalogue, executable, and origins). Computed by the
/// canonical encoder.
const STANDARD_V2_CANONICAL_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    115, 202, 159, 209, 255, 174, 218, 69, 195, 114, 168, 108, 210, 7, 50, 127, 176, 149, 134, 145,
    229, 113, 139, 179, 237, 228, 75, 75, 94, 20, 52, 52,
]);

fn standard_v2_source(units: Vec<StoredSourceUnit>) -> StoredSourceRevision {
    let bundle_hash = source_bundle_digest(&units).unwrap();
    StoredSourceRevision::new(
        SourceBundleId::from_bytes([0x41; 16]),
        SourceRevisionId::from_bytes([0x42; 16]),
        Some(SourceRevisionId::from_bytes([0x43; 16])),
        units,
        bundle_hash,
        source_revision_record_digest(
            SourceBundleId::from_bytes([0x41; 16]),
            Some(SourceRevisionId::from_bytes([0x43; 16])),
            bundle_hash,
        )
        .unwrap(),
    )
    .unwrap()
}

fn build_standard_v2_snapshot(
    units: Vec<StoredSourceUnit>,
    catalogue: CatalogueSnapshot,
    origins: Vec<DefinitionOrigin>,
    executables: Vec<StandardExecutable>,
    digest: Sha256Digest,
) -> StandardLibrarySnapshot {
    StandardLibrarySnapshot::new_with_executables(
        StandardLibraryRevisionId::from_bytes([0x44; 16]),
        StandardLibraryDigestVersion::Version2,
        standard_v2_source(units),
        "orna.language/1",
        catalogue,
        executables,
        origins,
        digest,
    )
    .unwrap()
}

fn standard_v2_parts() -> (
    Vec<StoredSourceUnit>,
    CatalogueSnapshot,
    Vec<DefinitionOrigin>,
    Vec<StandardExecutable>,
) {
    let (types_unit, invoke_unit) = standard_v2_units();
    let catalogue = standard_v2_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    let executable = standard_v2_executable(&catalogue, &origins);
    (
        vec![types_unit, invoke_unit],
        catalogue,
        origins,
        vec![executable],
    )
}

/// Runs the V2 source reconcile directly on raw stored facts, without the
/// separate digest-verification gate.
fn check_v2_parts(
    units: Vec<StoredSourceUnit>,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    executables: &[StandardExecutable],
) -> Result<(StandardSourceFamilies, CheckedStandardExecutable), StandardLibraryCheckError> {
    check_standard_library_source_v2_parts(
        &standard_v2_source(units),
        catalogue,
        origins,
        executables,
    )
}

fn verified_standard_v2_snapshot() -> VerifiedStandardLibrarySnapshot {
    let (units, catalogue, origins, executables) = standard_v2_parts();
    verify_standard_library_v2_snapshot(build_standard_v2_snapshot(
        units,
        catalogue,
        origins,
        executables,
        STANDARD_V2_CANONICAL_DIGEST,
    ))
    .unwrap()
}

#[test]
fn reconciles_the_exact_v2_standard_executable_bundle() {
    let verified = verified_standard_v2_snapshot();
    let checked = check_standard_library_source(&verified).unwrap();

    assert_eq!(checked.schemas().len(), 2);
    assert_eq!(checked.value_types().len(), 1);
    assert_eq!(checked.type_bindings().len(), 2);

    let executable = checked.checked_executable().unwrap();
    assert_eq!(executable.function_id(), STD_INVOKE_ECHO_FUNCTION_ID);
    assert_eq!(executable.parameter_ids(), &[STD_INVOKE_ECHO_PARAMETER_ID]);
    assert_eq!(
        executable.revision_id(),
        STD_INVOKE_ECHO_FUNCTION_REVISION_ID
    );
    assert_eq!(
        executable.revision_number(),
        STD_INVOKE_ECHO_REVISION_NUMBER
    );
    assert_eq!(
        executable.semantic_hash_version(),
        FunctionSemanticHashVersion::Version2
    );
    assert_eq!(executable.language_version(), "orna.language/1");

    let stored = &verified.executables()[0];
    assert_eq!(executable.function_id(), stored.function());
    assert_eq!(executable.revision_id(), stored.revision().id());
    assert_eq!(
        executable.revision_number(),
        stored.revision().revision_number()
    );
    assert_eq!(
        executable.semantic_hash_version(),
        stored.revision().semantic_hash_version()
    );
    assert_eq!(
        executable.language_version(),
        stored.revision().language_version()
    );
    assert_eq!(executable.artifact(), stored.revision().artifact());
    assert_eq!(executable.references(), stored.references());
    assert_eq!(
        executable.declaration_origin(),
        stored.revision().declaration_origin()
    );
    assert_eq!(
        executable.declaration_content_hash(),
        stored.revision().declaration_content_hash()
    );
    assert_eq!(
        executable.semantic_hash(),
        stored.revision().semantic_hash()
    );

    assert_eq!(
        executable.schema_origin().source_unit(),
        STD_INVOKE_SOURCE_UNIT_ID
    );
    assert_eq!(
        executable.function_origin(),
        executable.declaration_origin()
    );
    assert_eq!(
        executable.parameter_origins()[0].source_unit(),
        STD_INVOKE_SOURCE_UNIT_ID
    );
    let stored_schema_origin = verified
        .origins()
        .iter()
        .find(|origin| origin.identity() == DefinitionIdentity::Schema(STD_INVOKE_SCHEMA_ID))
        .unwrap()
        .source();
    assert_eq!(executable.schema_origin(), stored_schema_origin);
    assert_eq!(verified.origins().len(), 8);
    assert_eq!(
        &STD_INVOKE_SOURCE[executable.schema_origin().byte_start() as usize
            ..executable.schema_origin().byte_end() as usize],
        "CREATE SCHEMA std.invoke;"
    );
}

#[test]
fn version_one_keeps_the_type_only_contract_without_executable_facts() {
    let verified = verified_standard_library_for_relational_test();
    let checked = check_standard_library_source(&verified).unwrap();
    assert!(checked.checked_executable().is_none());
    assert_eq!(checked.schemas().len(), 2);
    assert_eq!(checked.value_types().len(), 1);
    assert_eq!(checked.type_bindings().len(), 2);
}

#[test]
fn rejects_v1_source_unit_identity_mutations() {
    let verified = verified_standard_library_for_relational_test();
    assert!(check_standard_library_source(&verified).is_ok());
    let stored = &verified.source().units()[0];

    for (label, id, logical_path) in [
        (
            "stable source-unit id",
            SourceUnitId::from_bytes([0x55; 16]),
            stored.logical_path(),
        ),
        ("logical path", stored.id(), "std/renamed.orna"),
    ] {
        let mutated = verified_v1_with_source_unit_identity(&verified, id, logical_path, 0);
        let error = check_standard_library_source(&mutated).unwrap_err();
        assert!(
            matches!(error, StandardLibraryCheckError::SourceMismatch),
            "{label}: unexpected rejection: {error}"
        );
    }

    let ordinal = StoredSourceUnit::new(
        STANDARD_SOURCE_UNIT_ID,
        1,
        stored.logical_path(),
        stored.content(),
        stored.content_hash(),
    )
    .unwrap();
    assert!(matches!(
        check_standard_library_source_v1_identity(&ordinal),
        Err(StandardLibraryCheckError::SourceMismatch)
    ));
}

fn verified_v1_with_source_unit_identity(
    verified: &VerifiedStandardLibrarySnapshot,
    id: SourceUnitId,
    logical_path: &str,
    ordinal: u32,
) -> VerifiedStandardLibrarySnapshot {
    let stored = &verified.source().units()[0];
    let unit = StoredSourceUnit::new(
        id,
        ordinal,
        logical_path,
        stored.content(),
        stored.content_hash(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
    let source = StoredSourceRevision::new(
        verified.source().bundle(),
        verified.source().id(),
        verified.source().parent(),
        vec![unit],
        bundle_hash,
        source_revision_record_digest(
            verified.source().bundle(),
            verified.source().parent(),
            bundle_hash,
        )
        .unwrap(),
    )
    .unwrap();
    let origins = verified
        .origins()
        .iter()
        .map(|origin| {
            let source_origin = origin.source();
            let source_unit = if source_origin.source_unit() == stored.id() {
                id
            } else {
                source_origin.source_unit()
            };
            DefinitionOrigin::new(
                origin.identity(),
                SourceOrigin::new(
                    source_unit,
                    source_origin.byte_start(),
                    source_origin.byte_end(),
                )
                .unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let provisional = StandardLibrarySnapshot::new(
        verified.revision(),
        verified.digest_version(),
        source,
        verified.language_version(),
        verified.catalogue().clone(),
        origins,
        Sha256Digest::from_bytes([0; 32]),
    )
    .unwrap();
    let digest = calculate_standard_library_digest(&provisional).unwrap();
    verify_standard_library_snapshot(
        StandardLibrarySnapshot::new(
            provisional.revision(),
            provisional.digest_version(),
            provisional.source().clone(),
            provisional.language_version(),
            provisional.catalogue().clone(),
            provisional.origins().to_vec(),
            digest,
        )
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn rejects_a_v2_bundle_with_the_wrong_unit_identity() {
    let (_, invoke_unit) = standard_v2_units();
    let catalogue = standard_v2_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    let executable = standard_v2_executable(&catalogue, &origins);
    let types_unit = stored_v2_unit(
        SourceUnitId::from_bytes([0x77; 16]),
        0,
        "std/types.orna",
        STANDARD_V2_TYPES_SOURCE,
    );
    let error = check_v2_parts(
        vec![types_unit, invoke_unit],
        &catalogue,
        &origins,
        &[executable],
    )
    .unwrap_err();
    assert!(
        matches!(error, StandardLibraryCheckError::SourceMismatch),
        "unexpected rejection: {error}"
    );
}

#[test]
fn rejects_a_v2_bundle_with_swapped_unit_order() {
    let catalogue = standard_v2_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    let executable = standard_v2_executable(&catalogue, &origins);
    let types_unit = stored_v2_unit(
        STD_TYPES_SOURCE_UNIT_ID,
        1,
        "std/types.orna",
        STANDARD_V2_TYPES_SOURCE,
    );
    let invoke_unit = stored_v2_unit(
        STD_INVOKE_SOURCE_UNIT_ID,
        0,
        "std/invoke.orna",
        STD_INVOKE_SOURCE,
    );
    let error = check_v2_parts(
        vec![invoke_unit, types_unit],
        &catalogue,
        &origins,
        &[executable],
    )
    .unwrap_err();
    assert!(
        matches!(error, StandardLibraryCheckError::SourceMismatch),
        "unexpected rejection: {error}"
    );
}

#[test]
fn rejects_a_v2_bundle_with_the_wrong_logical_path() {
    let (types_unit, _) = standard_v2_units();
    let catalogue = standard_v2_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    let executable = standard_v2_executable(&catalogue, &origins);
    let invoke_unit = stored_v2_unit(
        STD_INVOKE_SOURCE_UNIT_ID,
        1,
        "std/invocation.orna",
        STD_INVOKE_SOURCE,
    );
    let error = check_v2_parts(
        vec![types_unit, invoke_unit],
        &catalogue,
        &origins,
        &[executable],
    )
    .unwrap_err();
    assert!(
        matches!(error, StandardLibraryCheckError::SourceMismatch),
        "unexpected rejection: {error}"
    );
}

#[test]
fn rejects_a_v2_bundle_with_a_missing_or_extra_unit() {
    let (types_unit, _) = standard_v2_units();
    let catalogue = standard_v2_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    let executable = standard_v2_executable(&catalogue, &origins);
    let error = check_v2_parts(
        vec![types_unit],
        &catalogue,
        &origins,
        std::slice::from_ref(&executable),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::SourceUnitCount { actual: 1 }
    ));

    let (types_unit, invoke_unit) = standard_v2_units();
    let extra = stored_v2_unit(
        SourceUnitId::from_bytes([0x78; 16]),
        2,
        "std/extra.orna",
        "CREATE SCHEMA std.extra;",
    );
    let error = check_v2_parts(
        vec![types_unit, invoke_unit, extra],
        &catalogue,
        &origins,
        &[executable],
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::SourceUnitCount { actual: 3 }
    ));
}

#[test]
fn rejects_a_byte_modified_invoke_unit_closed() {
    let (types_unit, _) = standard_v2_units();
    let catalogue = standard_v2_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    let executable = standard_v2_executable(&catalogue, &origins);

    // Whitespace-only modification: tokens identical, declaration byte
    // ranges shift, so the stored origins and declaration content hash no
    // longer agree with the retained source.
    let modified = STD_INVOKE_SOURCE.replacen("RETURNS INTEGER", "RETURNS  INTEGER", 1);
    assert_ne!(modified, STD_INVOKE_SOURCE);
    let invoke_unit = stored_v2_unit(STD_INVOKE_SOURCE_UNIT_ID, 1, "std/invoke.orna", &modified);
    let error = check_v2_parts(
        vec![types_unit.clone(), invoke_unit],
        &catalogue,
        &origins,
        std::slice::from_ref(&executable),
    )
    .unwrap_err();
    assert!(
        matches!(error, StandardLibraryCheckError::SourceMismatch),
        "unexpected rejection: {error}"
    );

    // Semantic modification: the echo shape itself is rejected.
    let modified = STD_INVOKE_SOURCE.replacen("p_value INTEGER", "p_value BIGINT", 1);
    let invoke_unit = stored_v2_unit(STD_INVOKE_SOURCE_UNIT_ID, 1, "std/invoke.orna", &modified);
    let error = check_v2_parts(
        vec![types_unit, invoke_unit],
        &catalogue,
        &origins,
        &[executable],
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::UnexpectedParameterType
    ));
}

#[test]
fn rejects_a_v2_bundle_with_the_wrong_source_or_catalogue_names() {
    let (types_unit, _) = standard_v2_units();
    let catalogue = standard_v2_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    let executable = standard_v2_executable(&catalogue, &origins);

    // Source schema renamed.
    let invoke_unit = stored_v2_unit(
        STD_INVOKE_SOURCE_UNIT_ID,
        1,
        "std/invoke.orna",
        &STD_INVOKE_SOURCE.replacen("CREATE SCHEMA std.invoke;", "CREATE SCHEMA std.other;", 1),
    );
    let error = check_v2_parts(
        vec![types_unit.clone(), invoke_unit],
        &catalogue,
        &origins,
        std::slice::from_ref(&executable),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::SchemaNameMismatch { .. }
    ));

    // Source function renamed.
    let invoke_unit = stored_v2_unit(
        STD_INVOKE_SOURCE_UNIT_ID,
        1,
        "std/invoke.orna",
        &STD_INVOKE_SOURCE.replacen("std.invoke.echo(", "std.invoke.echo2(", 1),
    );
    let error = check_v2_parts(
        vec![types_unit.clone(), invoke_unit],
        &catalogue,
        &origins,
        std::slice::from_ref(&executable),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::UnexpectedName { .. }
    ));

    // Catalogue schema renamed at the fixed identity (the function name
    // must follow so the catalogue constructor stays valid).
    let mut renamed_schemas = catalogue.schemas().to_vec();
    renamed_schemas[2] = SchemaDefinition::new(
        STD_INVOKE_SCHEMA_ID,
        QualifiedSemanticName::new(["std", "other"]).unwrap(),
    );
    let function = catalogue
        .function_by_id(STD_INVOKE_ECHO_FUNCTION_ID)
        .unwrap();
    let renamed_function = FunctionDefinition::new(
        function.id(),
        QualifiedSemanticName::new(["std", "other", "echo"]).unwrap(),
        function.domain(),
        function.parameters().to_vec(),
        function.return_type().clone(),
        function.current_revision(),
        function.security(),
        function.transaction(),
        function.volatility(),
    );
    let renamed_catalogue = CatalogueSnapshot::new_with_functions_and_types(
        CatalogueRevisionId::from_bytes([0x21; 16]),
        renamed_schemas,
        vec![],
        catalogue.value_types().to_vec(),
        catalogue.type_bindings().to_vec(),
        vec![renamed_function],
    )
    .unwrap();
    let error = check_v2_parts(
        vec![types_unit.clone(), standard_v2_units().1],
        &renamed_catalogue,
        &origins,
        std::slice::from_ref(&executable),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::SchemaNameMismatch { .. }
    ));

    // Catalogue function renamed at the fixed identity.
    let function = catalogue
        .function_by_id(STD_INVOKE_ECHO_FUNCTION_ID)
        .unwrap();
    let renamed_function = FunctionDefinition::new(
        function.id(),
        QualifiedSemanticName::new(["std", "invoke", "other"]).unwrap(),
        function.domain(),
        function.parameters().to_vec(),
        function.return_type().clone(),
        function.current_revision(),
        function.security(),
        function.transaction(),
        function.volatility(),
    );
    let renamed_catalogue = CatalogueSnapshot::new_with_functions_and_types(
        CatalogueRevisionId::from_bytes([0x21; 16]),
        catalogue.schemas().to_vec(),
        vec![],
        catalogue.value_types().to_vec(),
        catalogue.type_bindings().to_vec(),
        vec![renamed_function],
    )
    .unwrap();
    let error = check_v2_parts(
        vec![types_unit.clone(), standard_v2_units().1],
        &renamed_catalogue,
        &origins,
        std::slice::from_ref(&executable),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::FunctionNameMismatch { .. }
    ));

    // Catalogue parameter renamed at the fixed identity.
    let parameter = function
        .parameter_by_id(STD_INVOKE_ECHO_PARAMETER_ID)
        .unwrap();
    let renamed_function = FunctionDefinition::new(
        function.id(),
        function.name().clone(),
        function.domain(),
        vec![ParameterDefinition::new(
            parameter.id(),
            "p_other",
            parameter.ordinal(),
            parameter.resolved_type(),
            parameter.default_expression(),
        )],
        function.return_type().clone(),
        function.current_revision(),
        function.security(),
        function.transaction(),
        function.volatility(),
    );
    let renamed_catalogue = CatalogueSnapshot::new_with_functions_and_types(
        CatalogueRevisionId::from_bytes([0x21; 16]),
        catalogue.schemas().to_vec(),
        vec![],
        catalogue.value_types().to_vec(),
        catalogue.type_bindings().to_vec(),
        vec![renamed_function],
    )
    .unwrap();
    let error = check_v2_parts(
        vec![types_unit, standard_v2_units().1],
        &renamed_catalogue,
        &origins,
        &[executable],
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::ParameterNameMismatch { .. }
    ));
}

#[test]
fn rejects_wrong_v2_origin_ranges_closed() {
    let (types_unit, invoke_unit) = standard_v2_units();
    let catalogue = standard_v2_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let assert_rejected = |error: StandardLibraryCheckError| {
        assert!(
            matches!(error, StandardLibraryCheckError::SourceMismatch),
            "unexpected rejection: {error}"
        );
    };
    let base_origins = standard_v2_types_origins(&catalogue, &parsed_types);

    // Wrong schema origin range.
    let mut origins = base_origins.clone();
    let mut invoke_origins = standard_v2_invoke_origins(STD_INVOKE_SOURCE);
    let schema_origin = invoke_origins
        .iter_mut()
        .find(|origin| origin.identity() == DefinitionIdentity::Schema(STD_INVOKE_SCHEMA_ID))
        .unwrap();
    *schema_origin = DefinitionOrigin::new(
        schema_origin.identity(),
        SourceOrigin::new(
            STD_INVOKE_SOURCE_UNIT_ID,
            schema_origin.source().byte_start() + 1,
            schema_origin.source().byte_end(),
        )
        .unwrap(),
    );
    origins.extend(invoke_origins);
    let executable = standard_v2_executable(&catalogue, &origins);
    assert_rejected(
        check_v2_parts(
            vec![types_unit.clone(), invoke_unit.clone()],
            &catalogue,
            &origins,
            &[executable],
        )
        .unwrap_err(),
    );

    // Wrong function origin range.
    let mut origins = base_origins.clone();
    let mut invoke_origins = standard_v2_invoke_origins(STD_INVOKE_SOURCE);
    let function_origin = invoke_origins
        .iter_mut()
        .find(|origin| {
            origin.identity() == DefinitionIdentity::Function(STD_INVOKE_ECHO_FUNCTION_ID)
        })
        .unwrap();
    *function_origin = DefinitionOrigin::new(
        function_origin.identity(),
        SourceOrigin::new(
            STD_INVOKE_SOURCE_UNIT_ID,
            function_origin.source().byte_start(),
            function_origin.source().byte_end() - 1,
        )
        .unwrap(),
    );
    origins.extend(invoke_origins);
    let executable = standard_v2_executable(&catalogue, &origins);
    assert_rejected(
        check_v2_parts(
            vec![types_unit.clone(), invoke_unit.clone()],
            &catalogue,
            &origins,
            &[executable],
        )
        .unwrap_err(),
    );

    // Wrong parameter origin range.
    let mut origins = base_origins;
    let mut invoke_origins = standard_v2_invoke_origins(STD_INVOKE_SOURCE);
    let parameter_origin = invoke_origins
        .iter_mut()
        .find(|origin| {
            origin.identity()
                == DefinitionIdentity::Parameter {
                    owner: STD_INVOKE_ECHO_FUNCTION_ID,
                    parameter: STD_INVOKE_ECHO_PARAMETER_ID,
                }
        })
        .unwrap();
    *parameter_origin = DefinitionOrigin::new(
        parameter_origin.identity(),
        SourceOrigin::new(
            STD_INVOKE_SOURCE_UNIT_ID,
            parameter_origin.source().byte_start(),
            parameter_origin.source().byte_end() - 1,
        )
        .unwrap(),
    );
    origins.extend(invoke_origins);
    let executable = standard_v2_executable(&catalogue, &origins);
    assert_rejected(
        check_v2_parts(
            vec![types_unit, invoke_unit],
            &catalogue,
            &origins,
            &[executable],
        )
        .unwrap_err(),
    );
}

#[test]
fn rejects_a_wrong_stored_revision_identity_closed() {
    let (types_unit, invoke_unit) = standard_v2_units();
    let catalogue = standard_v2_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    let executable = standard_v2_executable(&catalogue, &origins);

    // Rebuild the executable with a different revision identity.
    let wrong_revision = FunctionRevisionId::from_bytes([0x11; 16]);
    let revision = executable.revision().clone();
    let references = executable
        .references()
        .iter()
        .map(|reference| {
            DefinitionReference::new(
                reference.source_function(),
                wrong_revision,
                reference.ordinal(),
                reference.target(),
                reference.kind(),
                reference.source_origin(),
            )
        })
        .collect::<Vec<_>>();
    let revision = FunctionRevisionRecord::new(
        revision.function(),
        wrong_revision,
        revision.revision_number(),
        revision.declaration_origin(),
        revision.declaration_content_hash(),
        revision.semantic_hash(),
        revision.language_version(),
        revision.artifact().clone(),
    )
    .unwrap()
    .with_semantic_hash_version(revision.semantic_hash_version());
    let executable = StandardExecutable::new(revision.function(), revision, references).unwrap();

    let error = check_v2_parts(
        vec![types_unit, invoke_unit],
        &catalogue,
        &origins,
        &[executable],
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::ExecutableMismatch
    ));
}

#[test]
fn rejects_every_stored_executable_fact_mismatch_closed() {
    let verified = verified_standard_v2_snapshot();
    let checked = check_standard_library_source(&verified)
        .unwrap()
        .checked_executable()
        .unwrap()
        .clone();
    let stored = verified.executables()[0].clone();
    let revision = stored.revision().clone();
    let artifact = revision.artifact().clone();
    let references = stored.references().to_vec();
    let fails = |stored: &StandardExecutable| {
        assert!(
            matches!(
                reconcile_standard_executable(stored, &checked),
                Err(StandardLibraryCheckError::ExecutableMismatch)
            ),
            "expected ExecutableMismatch"
        );
    };

    // Wrong stored function identity.
    let wrong_function = FunctionId::from_bytes([0x55; 16]);
    let mutated = FunctionRevisionRecord::new(
        wrong_function,
        revision.id(),
        revision.revision_number(),
        revision.declaration_origin(),
        revision.declaration_content_hash(),
        revision.semantic_hash(),
        revision.language_version(),
        artifact.clone(),
    )
    .unwrap()
    .with_semantic_hash_version(revision.semantic_hash_version());
    fails(&StandardExecutable::new(wrong_function, mutated, references.clone()).unwrap());

    // Wrong stored revision identity.
    let wrong_revision = FunctionRevisionId::from_bytes([0x66; 16]);
    let mutated = FunctionRevisionRecord::new(
        revision.function(),
        wrong_revision,
        revision.revision_number(),
        revision.declaration_origin(),
        revision.declaration_content_hash(),
        revision.semantic_hash(),
        revision.language_version(),
        artifact.clone(),
    )
    .unwrap()
    .with_semantic_hash_version(revision.semantic_hash_version());
    fails(&StandardExecutable::new(revision.function(), mutated, references.clone()).unwrap());

    // Wrong stored revision number.
    let mutated = FunctionRevisionRecord::new(
        revision.function(),
        revision.id(),
        revision.revision_number() + 1,
        revision.declaration_origin(),
        revision.declaration_content_hash(),
        revision.semantic_hash(),
        revision.language_version(),
        artifact.clone(),
    )
    .unwrap()
    .with_semantic_hash_version(revision.semantic_hash_version());
    fails(&StandardExecutable::new(revision.function(), mutated, references.clone()).unwrap());

    // Wrong stored semantic-hash version.
    let mutated = FunctionRevisionRecord::new(
        revision.function(),
        revision.id(),
        revision.revision_number(),
        revision.declaration_origin(),
        revision.declaration_content_hash(),
        revision.semantic_hash(),
        revision.language_version(),
        artifact.clone(),
    )
    .unwrap()
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version1);
    fails(&StandardExecutable::new(revision.function(), mutated, references.clone()).unwrap());

    // Wrong stored language version.
    let mutated = FunctionRevisionRecord::new(
        revision.function(),
        revision.id(),
        revision.revision_number(),
        revision.declaration_origin(),
        revision.declaration_content_hash(),
        revision.semantic_hash(),
        "orna.language/2",
        artifact.clone(),
    )
    .unwrap()
    .with_semantic_hash_version(revision.semantic_hash_version());
    fails(&StandardExecutable::new(revision.function(), mutated, references.clone()).unwrap());

    // Wrong stored declaration origin.
    let mutated = FunctionRevisionRecord::new(
        revision.function(),
        revision.id(),
        revision.revision_number(),
        SourceOrigin::new(STD_INVOKE_SOURCE_UNIT_ID, 0, 1).unwrap(),
        revision.declaration_content_hash(),
        revision.semantic_hash(),
        revision.language_version(),
        artifact.clone(),
    )
    .unwrap()
    .with_semantic_hash_version(revision.semantic_hash_version());
    fails(&StandardExecutable::new(revision.function(), mutated, references.clone()).unwrap());

    // Wrong stored declaration content hash.
    let mutated = FunctionRevisionRecord::new(
        revision.function(),
        revision.id(),
        revision.revision_number(),
        revision.declaration_origin(),
        Sha256Digest::from_bytes([0x11; 32]),
        revision.semantic_hash(),
        revision.language_version(),
        artifact.clone(),
    )
    .unwrap()
    .with_semantic_hash_version(revision.semantic_hash_version());
    fails(&StandardExecutable::new(revision.function(), mutated, references.clone()).unwrap());

    // Wrong stored semantic hash.
    let mutated = FunctionRevisionRecord::new(
        revision.function(),
        revision.id(),
        revision.revision_number(),
        revision.declaration_origin(),
        revision.declaration_content_hash(),
        Sha256Digest::from_bytes([0x22; 32]),
        revision.language_version(),
        artifact.clone(),
    )
    .unwrap()
    .with_semantic_hash_version(revision.semantic_hash_version());
    fails(&StandardExecutable::new(revision.function(), mutated, references.clone()).unwrap());

    // Wrong stored artifact format.
    let mutated_artifact = ExecutableArtifact::new(
        artifact.kind(),
        "orna.server-parameter-echo2",
        artifact.version(),
        artifact.payload().to_vec(),
        artifact.content_hash(),
    )
    .unwrap();
    let mutated = FunctionRevisionRecord::new(
        revision.function(),
        revision.id(),
        revision.revision_number(),
        revision.declaration_origin(),
        revision.declaration_content_hash(),
        revision.semantic_hash(),
        revision.language_version(),
        mutated_artifact,
    )
    .unwrap()
    .with_semantic_hash_version(revision.semantic_hash_version());
    fails(&StandardExecutable::new(revision.function(), mutated, references.clone()).unwrap());

    // Wrong stored artifact version.
    let mutated_artifact = ExecutableArtifact::new(
        artifact.kind(),
        artifact.format(),
        artifact.version() + 1,
        artifact.payload().to_vec(),
        artifact.content_hash(),
    )
    .unwrap();
    let mutated = FunctionRevisionRecord::new(
        revision.function(),
        revision.id(),
        revision.revision_number(),
        revision.declaration_origin(),
        revision.declaration_content_hash(),
        revision.semantic_hash(),
        revision.language_version(),
        mutated_artifact,
    )
    .unwrap()
    .with_semantic_hash_version(revision.semantic_hash_version());
    fails(&StandardExecutable::new(revision.function(), mutated, references.clone()).unwrap());

    // Wrong stored artifact payload.
    let mut payload = artifact.payload().to_vec();
    let last = payload.last_mut().unwrap();
    *last ^= 0xff;
    let mutated_artifact = ExecutableArtifact::new(
        artifact.kind(),
        artifact.format(),
        artifact.version(),
        payload.clone(),
        artifact_payload_digest(&payload).unwrap(),
    )
    .unwrap();
    let mutated = FunctionRevisionRecord::new(
        revision.function(),
        revision.id(),
        revision.revision_number(),
        revision.declaration_origin(),
        revision.declaration_content_hash(),
        revision.semantic_hash(),
        revision.language_version(),
        mutated_artifact,
    )
    .unwrap()
    .with_semantic_hash_version(revision.semantic_hash_version());
    fails(&StandardExecutable::new(revision.function(), mutated, references.clone()).unwrap());

    // Missing reference.
    fails(
        &StandardExecutable::new(
            revision.function(),
            revision.clone(),
            references[..2].to_vec(),
        )
        .unwrap(),
    );

    // Extra reference.
    let mut extra = references.clone();
    extra.push(DefinitionReference::new(
        STD_INVOKE_ECHO_FUNCTION_ID,
        STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        3,
        DefinitionReferenceTarget::ValueType(STD_INTEGER_TYPE_ID),
        DefinitionReferenceKind::NamedType,
        references[0].source_origin(),
    ));
    fails(&StandardExecutable::new(revision.function(), revision.clone(), extra).unwrap());

    // Reordered references.
    let mut reordered = references.clone();
    reordered.swap(0, 1);
    fails(&StandardExecutable::new(revision.function(), revision.clone(), reordered).unwrap());

    // Wrong reference kind.
    let mut wrong_kind = references.clone();
    wrong_kind[0] = DefinitionReference::new(
        wrong_kind[0].source_function(),
        wrong_kind[0].source_revision(),
        wrong_kind[0].ordinal(),
        wrong_kind[0].target(),
        DefinitionReferenceKind::FunctionCall,
        wrong_kind[0].source_origin(),
    );
    fails(&StandardExecutable::new(revision.function(), revision.clone(), wrong_kind).unwrap());

    // Wrong reference target.
    let mut wrong_target = references.clone();
    wrong_target[1] = DefinitionReference::new(
        wrong_target[1].source_function(),
        wrong_target[1].source_revision(),
        wrong_target[1].ordinal(),
        DefinitionReferenceTarget::ValueType(TypeId::from_bytes([0x77; 16])),
        wrong_target[1].kind(),
        wrong_target[1].source_origin(),
    );
    fails(&StandardExecutable::new(revision.function(), revision.clone(), wrong_target).unwrap());

    // Wrong reference origin.
    let mut wrong_origin = references.clone();
    wrong_origin[2] = DefinitionReference::new(
        wrong_origin[2].source_function(),
        wrong_origin[2].source_revision(),
        wrong_origin[2].ordinal(),
        wrong_origin[2].target(),
        wrong_origin[2].kind(),
        SourceOrigin::new(STD_INVOKE_SOURCE_UNIT_ID, 0, 1).unwrap(),
    );
    fails(&StandardExecutable::new(revision.function(), revision.clone(), wrong_origin).unwrap());
}

/// The exact retained ADR 0058 `std/output.orna` source: the two output
/// schema declarations, the two opaque output value type declarations,
/// and their two qualified exports.
const STANDARD_V3_OUTPUT_SOURCE: &str = "CREATE SCHEMA std.terminal;\nCREATE SCHEMA std.io;\n\nCREATE TYPE std.terminal.Document AS VALUE OPAQUE\n    KERNEL CONTRACT 'orna.std.value.terminal-document@1'\n    IMMUTABLE\n    TRANSIENT;\n\nEXPORT TYPE std.terminal.Document AS std.Document;\n\nCREATE TYPE std.io.ByteStream AS VALUE OPAQUE\n    KERNEL CONTRACT 'orna.std.value.byte-stream@1'\n    IMMUTABLE\n    TRANSIENT;\n\nEXPORT TYPE std.io.ByteStream AS std.ByteStream;";

fn standard_v3_catalogue(with_invoke: bool) -> CatalogueSnapshot {
    let catalogue = standard_v2_catalogue(with_invoke);
    let mut schemas = catalogue.schemas().to_vec();
    schemas.push(SchemaDefinition::new(
        STD_TERMINAL_SCHEMA_ID,
        QualifiedSemanticName::new(["std", "terminal"]).unwrap(),
    ));
    schemas.push(SchemaDefinition::new(
        STD_IO_SCHEMA_ID,
        QualifiedSemanticName::new(["std", "io"]).unwrap(),
    ));
    let mut value_types = catalogue.value_types().to_vec();
    value_types.push(ValueTypeDefinition::opaque(
        STD_TERMINAL_DOCUMENT_TYPE_ID,
        QualifiedSemanticName::new(["std", "terminal", "document"]).unwrap(),
        "orna.std.value.terminal-document@1",
    ));
    value_types.push(ValueTypeDefinition::opaque(
        STD_IO_BYTE_STREAM_TYPE_ID,
        QualifiedSemanticName::new(["std", "io", "bytestream"]).unwrap(),
        "orna.std.value.byte-stream@1",
    ));
    let mut type_bindings = catalogue.type_bindings().to_vec();
    type_bindings.push(
        TypeBinding::qualified(
            QualifiedSemanticName::new(["std", "document"]).unwrap(),
            STD_TERMINAL_DOCUMENT_TYPE_ID,
        )
        .unwrap(),
    );
    type_bindings.push(
        TypeBinding::qualified(
            QualifiedSemanticName::new(["std", "bytestream"]).unwrap(),
            STD_IO_BYTE_STREAM_TYPE_ID,
        )
        .unwrap(),
    );
    CatalogueSnapshot::new_with_functions_and_types(
        CatalogueRevisionId::from_bytes([0x21; 16]),
        schemas,
        vec![],
        value_types,
        type_bindings,
        catalogue.functions().to_vec(),
    )
    .unwrap()
}

fn standard_v3_catalogue_with_output_value_type(
    index: usize,
    definition: ValueTypeDefinition,
) -> CatalogueSnapshot {
    let catalogue = standard_v3_catalogue(true);
    let mut value_types = catalogue.value_types().to_vec();
    value_types[index] = definition;
    CatalogueSnapshot::new_with_functions_and_types(
        catalogue.revision(),
        catalogue.schemas().to_vec(),
        vec![],
        value_types,
        catalogue.type_bindings().to_vec(),
        catalogue.functions().to_vec(),
    )
    .unwrap()
}

fn standard_v3_units() -> (StoredSourceUnit, StoredSourceUnit, StoredSourceUnit) {
    (
        stored_v2_unit(
            STD_TYPES_SOURCE_UNIT_ID,
            0,
            "std/types.orna",
            STANDARD_V2_TYPES_SOURCE,
        ),
        stored_v2_unit(
            STD_INVOKE_SOURCE_UNIT_ID,
            1,
            "std/invoke.orna",
            STD_INVOKE_SOURCE,
        ),
        stored_v2_unit(
            STD_OUTPUT_SOURCE_UNIT_ID,
            2,
            "std/output.orna",
            STANDARD_V3_OUTPUT_SOURCE,
        ),
    )
}

fn standard_v3_output_origins(
    catalogue: &CatalogueSnapshot,
    source: &str,
) -> Vec<DefinitionOrigin> {
    let report =
        parse_bundle(&SourceBundle::new([SourceUnit::new("std/output.orna", source)]).unwrap());
    assert!(report.diagnostics().is_empty(), "{source}");
    let parsed = &report.units()[0];
    let origin = |identity: DefinitionIdentity, span: &SourceSpan| -> DefinitionOrigin {
        DefinitionOrigin::new(
            identity,
            SourceOrigin::new(
                STD_OUTPUT_SOURCE_UNIT_ID,
                u32::try_from(span.start).unwrap(),
                u32::try_from(span.end).unwrap(),
            )
            .unwrap(),
        )
    };
    let document_binding = catalogue.type_binding_by_name(&TypeLookupName::qualified(
        QualifiedSemanticName::new(["std", "document"]).unwrap(),
    ));
    let bytestream_binding = catalogue.type_binding_by_name(&TypeLookupName::qualified(
        QualifiedSemanticName::new(["std", "bytestream"]).unwrap(),
    ));
    let mut origins = Vec::with_capacity(6);
    if let Some(schema) = parsed.parsed().schemas().first() {
        origins.push(origin(
            DefinitionIdentity::Schema(STD_TERMINAL_SCHEMA_ID),
            &schema.span,
        ));
    }
    if let Some(schema) = parsed.parsed().schemas().get(1) {
        origins.push(origin(
            DefinitionIdentity::Schema(STD_IO_SCHEMA_ID),
            &schema.span,
        ));
    }
    if let Some(value_type) = parsed.parsed().opaque_value_types().first() {
        origins.push(origin(
            DefinitionIdentity::ValueType(STD_TERMINAL_DOCUMENT_TYPE_ID),
            &value_type.span,
        ));
    }
    if let (Some(binding), Some(export)) =
        (document_binding, parsed.parsed().type_exports().first())
    {
        origins.push(origin(
            DefinitionIdentity::TypeBinding(binding.id()),
            &export.span,
        ));
    }
    if let Some(value_type) = parsed.parsed().opaque_value_types().get(1) {
        origins.push(origin(
            DefinitionIdentity::ValueType(STD_IO_BYTE_STREAM_TYPE_ID),
            &value_type.span,
        ));
    }
    if let (Some(binding), Some(export)) =
        (bytestream_binding, parsed.parsed().type_exports().get(1))
    {
        origins.push(origin(
            DefinitionIdentity::TypeBinding(binding.id()),
            &export.span,
        ));
    }
    origins
}

fn standard_v3_parts() -> (
    Vec<StoredSourceUnit>,
    CatalogueSnapshot,
    Vec<DefinitionOrigin>,
    Vec<StandardExecutable>,
) {
    let (types_unit, invoke_unit, output_unit) = standard_v3_units();
    let catalogue = standard_v3_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    origins.extend(standard_v3_output_origins(
        &catalogue,
        STANDARD_V3_OUTPUT_SOURCE,
    ));
    let executable = standard_v2_executable(&catalogue, &origins);
    (
        vec![types_unit, invoke_unit, output_unit],
        catalogue,
        origins,
        vec![executable],
    )
}

fn standard_v3_source(units: Vec<StoredSourceUnit>) -> StoredSourceRevision {
    let bundle_hash = source_bundle_digest(&units).unwrap();
    StoredSourceRevision::new(
        SourceBundleId::from_bytes([0x51; 16]),
        SourceRevisionId::from_bytes([0x52; 16]),
        Some(SourceRevisionId::from_bytes([0x53; 16])),
        units,
        bundle_hash,
        source_revision_record_digest(
            SourceBundleId::from_bytes([0x51; 16]),
            Some(SourceRevisionId::from_bytes([0x53; 16])),
            bundle_hash,
        )
        .unwrap(),
    )
    .unwrap()
}

/// Runs the V3 source reconcile directly on raw stored facts, without the
/// separate digest-verification gate.
fn check_v3_parts(
    units: Vec<StoredSourceUnit>,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    executables: &[StandardExecutable],
) -> Result<(StandardSourceFamilies, CheckedStandardExecutable), StandardLibraryCheckError> {
    check_standard_library_source_v3_parts(
        &standard_v3_source(units),
        catalogue,
        origins,
        executables,
    )
}

fn build_standard_v3_snapshot(
    units: Vec<StoredSourceUnit>,
    catalogue: CatalogueSnapshot,
    origins: Vec<DefinitionOrigin>,
    executables: Vec<StandardExecutable>,
    digest: Sha256Digest,
) -> StandardLibrarySnapshot {
    StandardLibrarySnapshot::new_with_executables(
        STANDARD_LIBRARY_V3_REVISION_ID,
        StandardLibraryDigestVersion::Version2,
        standard_v3_source(units),
        "orna.language/1",
        catalogue,
        executables,
        origins,
        digest,
    )
    .unwrap()
}

/// The compiled canonical V3 standard-library digest for the exact test
/// inputs (`STANDARD_V2_TYPES_SOURCE`, `STD_INVOKE_SOURCE`,
/// `STANDARD_V3_OUTPUT_SOURCE`, the fixed identities, catalogue,
/// executable, and origins). Computed by the canonical encoder.
const STANDARD_V3_CANONICAL_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    190, 191, 32, 251, 204, 169, 87, 210, 50, 82, 209, 87, 203, 106, 51, 38, 191, 112, 175, 46, 92,
    50, 161, 93, 72, 2, 203, 116, 173, 102, 221, 131,
]);

fn verified_standard_v3_snapshot() -> VerifiedStandardLibrarySnapshot {
    let (units, catalogue, origins, executables) = standard_v3_parts();
    verify_standard_library_v2_snapshot(build_standard_v3_snapshot(
        units,
        catalogue,
        origins,
        executables,
        STANDARD_V3_CANONICAL_DIGEST,
    ))
    .unwrap()
}

#[test]
fn reconciles_the_exact_v3_standard_output_bundle() {
    let verified = verified_standard_v3_snapshot();
    assert_eq!(verified.revision(), STANDARD_LIBRARY_V3_REVISION_ID);
    assert_eq!(
        verified.digest_version(),
        StandardLibraryDigestVersion::Version2
    );
    let checked = check_standard_library_source(&verified).unwrap();

    assert_eq!(checked.schemas().len(), 2);
    assert_eq!(checked.value_types().len(), 1);
    assert_eq!(checked.type_bindings().len(), 2);

    let executable = checked.checked_executable().unwrap();
    assert_eq!(executable.parameter_ids(), &[STD_INVOKE_ECHO_PARAMETER_ID]);
    assert_eq!(executable.function_id(), STD_INVOKE_ECHO_FUNCTION_ID);
    assert_eq!(
        executable.revision_id(),
        STD_INVOKE_ECHO_FUNCTION_REVISION_ID
    );
    assert_eq!(
        executable.revision_number(),
        STD_INVOKE_ECHO_REVISION_NUMBER
    );
    assert_eq!(
        executable.semantic_hash_version(),
        FunctionSemanticHashVersion::Version2
    );
    assert_eq!(executable.language_version(), "orna.language/1");
    assert_eq!(executable.references().len(), 3);

    let stored = &verified.executables()[0];
    assert_eq!(executable.artifact(), stored.revision().artifact());
    assert_eq!(executable.references(), stored.references());
    assert_eq!(
        executable.declaration_origin(),
        stored.revision().declaration_origin()
    );
    assert_eq!(
        executable.declaration_content_hash(),
        stored.revision().declaration_content_hash()
    );
    assert_eq!(
        executable.semantic_hash(),
        stored.revision().semantic_hash()
    );

    // The retained output unit carries exactly the six output origins at
    // the exact declaration byte ranges.
    let output_origins = verified
        .origins()
        .iter()
        .filter(|origin| origin.source().source_unit() == STD_OUTPUT_SOURCE_UNIT_ID)
        .collect::<Vec<_>>();
    assert_eq!(output_origins.len(), 6);
    let document_origin = output_origins
        .iter()
        .find(|origin| {
            origin.identity() == DefinitionIdentity::ValueType(STD_TERMINAL_DOCUMENT_TYPE_ID)
        })
        .unwrap();
    assert_eq!(
        &STANDARD_V3_OUTPUT_SOURCE[document_origin.source().byte_start() as usize
            ..document_origin.source().byte_end() as usize],
        "CREATE TYPE std.terminal.Document AS VALUE OPAQUE\n    KERNEL CONTRACT 'orna.std.value.terminal-document@1'\n    IMMUTABLE\n    TRANSIENT;"
    );
    assert_eq!(verified.origins().len(), 14);
}

#[test]
fn rejects_a_v3_bundle_with_the_wrong_unit_identity_order_or_path() {
    let (types_unit, invoke_unit, output_unit) = standard_v3_units();
    let catalogue = standard_v3_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    origins.extend(standard_v3_output_origins(
        &catalogue,
        STANDARD_V3_OUTPUT_SOURCE,
    ));
    let executable = standard_v2_executable(&catalogue, &origins);
    let rejects = |units: Vec<StoredSourceUnit>, label: &str| {
        let error = check_v3_parts(
            units,
            &catalogue,
            &origins,
            std::slice::from_ref(&executable),
        )
        .unwrap_err();
        assert!(
            matches!(error, StandardLibraryCheckError::SourceMismatch),
            "{label}: unexpected rejection: {error}"
        );
    };

    rejects(
        vec![
            stored_v2_unit(
                SourceUnitId::from_bytes([0x77; 16]),
                0,
                "std/types.orna",
                STANDARD_V2_TYPES_SOURCE,
            ),
            invoke_unit.clone(),
            output_unit.clone(),
        ],
        "wrong types unit identity",
    );
    rejects(
        vec![
            types_unit.clone(),
            stored_v2_unit(
                STD_INVOKE_SOURCE_UNIT_ID,
                1,
                "std/invoke.orna",
                STD_INVOKE_SOURCE,
            ),
            stored_v2_unit(
                STD_OUTPUT_SOURCE_UNIT_ID,
                2,
                "std/out.orna",
                STANDARD_V3_OUTPUT_SOURCE,
            ),
        ],
        "wrong output unit path",
    );
    rejects(
        vec![
            types_unit,
            stored_v2_unit(
                STD_OUTPUT_SOURCE_UNIT_ID,
                1,
                "std/output.orna",
                STANDARD_V3_OUTPUT_SOURCE,
            ),
            stored_v2_unit(
                STD_INVOKE_SOURCE_UNIT_ID,
                2,
                "std/invoke.orna",
                STD_INVOKE_SOURCE,
            ),
        ],
        "swapped invoke and output units",
    );
}

#[test]
fn rejects_a_v3_bundle_with_a_missing_or_extra_unit() {
    let (types_unit, invoke_unit, output_unit) = standard_v3_units();
    let catalogue = standard_v3_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    origins.extend(standard_v3_output_origins(
        &catalogue,
        STANDARD_V3_OUTPUT_SOURCE,
    ));
    let executable = standard_v2_executable(&catalogue, &origins);

    let error = check_v3_parts(
        vec![types_unit.clone(), invoke_unit.clone()],
        &catalogue,
        &origins,
        std::slice::from_ref(&executable),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::SourceUnitCount { actual: 2 }
    ));

    let extra = stored_v2_unit(
        SourceUnitId::from_bytes([0x78; 16]),
        3,
        "std/extra.orna",
        "CREATE SCHEMA std.extra;",
    );
    let error = check_v3_parts(
        vec![types_unit.clone(), invoke_unit.clone(), output_unit, extra],
        &catalogue,
        &origins,
        &[executable],
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::SourceUnitCount { actual: 4 }
    ));
}

#[test]
fn rejects_every_output_unit_content_variation_closed() {
    let (types_unit, invoke_unit, _output_unit) = standard_v3_units();
    let catalogue = standard_v3_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut base_origins = standard_v2_types_origins(&catalogue, &parsed_types);
    base_origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    let rejects_output = |source: &str, label: &str| {
        let output_unit = stored_v2_unit(STD_OUTPUT_SOURCE_UNIT_ID, 2, "std/output.orna", source);
        let mut origins = base_origins.clone();
        origins.extend(standard_v3_output_origins(&catalogue, source));
        let executable = standard_v2_executable(&catalogue, &origins);
        let error = check_v3_parts(
            vec![types_unit.clone(), invoke_unit.clone(), output_unit],
            &catalogue,
            &origins,
            &[executable],
        )
        .unwrap_err();
        assert!(
            matches!(error, StandardLibraryCheckError::SourceMismatch),
            "{label}: unexpected rejection: {error}"
        );
    };

    rejects_output(
        &STANDARD_V3_OUTPUT_SOURCE.replace("CREATE SCHEMA std.terminal;\n", ""),
        "missing terminal schema",
    );
    rejects_output(
        &STANDARD_V3_OUTPUT_SOURCE
            .replace("CREATE SCHEMA std.terminal;", "CREATE SCHEMA std.term;"),
        "wrong terminal schema name",
    );
    rejects_output(
            &STANDARD_V3_OUTPUT_SOURCE.replace(
                "CREATE TYPE std.terminal.Document AS VALUE OPAQUE\n    KERNEL CONTRACT 'orna.std.value.terminal-document@1'\n    IMMUTABLE\n    TRANSIENT;\n\n",
                "",
            ),
            "missing document type",
        );
    rejects_output(
        &STANDARD_V3_OUTPUT_SOURCE.replace(
            "CREATE TYPE std.terminal.Document",
            "CREATE TYPE std.terminal.Doc",
        ),
        "wrong document type name",
    );
    rejects_output(
        &STANDARD_V3_OUTPUT_SOURCE.replace(
            "'orna.std.value.terminal-document@1'",
            "'orna.std.value.terminal-document@2'",
        ),
        "wrong document contract",
    );
    rejects_output(
        &STANDARD_V3_OUTPUT_SOURCE.replace("AS std.Document;", "AS std.Doc;"),
        "wrong document export target",
    );
    rejects_output(
        &STANDARD_V3_OUTPUT_SOURCE.replace(
            "EXPORT TYPE std.terminal.Document",
            "EXPORT TYPE std.terminal.Doc",
        ),
        "wrong document export source",
    );
    rejects_output(
        &STANDARD_V3_OUTPUT_SOURCE.replace(
            "EXPORT TYPE std.terminal.Document AS std.Document;",
            "EXPORT TYPE std.terminal.Document TO PRELUDE AS Document;",
        ),
        "prelude document export",
    );
    rejects_output(
        &format!("{STANDARD_V3_OUTPUT_SOURCE}\nCREATE SCHEMA std.extra;"),
        "extra schema declaration",
    );
    rejects_output(
            &STANDARD_V3_OUTPUT_SOURCE.replace(
                "EXPORT TYPE std.io.ByteStream AS std.ByteStream;",
                "EXPORT TYPE std.io.ByteStream AS std.ByteStream;\n\nCREATE TYPE std.io.Extra AS VALUE OPAQUE\n    KERNEL CONTRACT 'orna.std.value.extra@1'\n    IMMUTABLE\n    TRANSIENT;",
            ),
            "extra opaque value type declaration",
        );
    rejects_output(
        &format!("{STANDARD_V3_OUTPUT_SOURCE}\nEXPORT TYPE std.io.ByteStream AS std.ByteStream;"),
        "extra export declaration",
    );
    rejects_output(
        &format!(
            "{STANDARD_V3_OUTPUT_SOURCE}\nCREATE TYPE std.extra.Value AS VALUE PRIMITIVE KERNEL CONTRACT 'extra@1' IMMUTABLE TRANSIENT;"
        ),
        "extra primitive value type declaration",
    );
}

#[test]
fn rejects_wrong_v3_output_catalogue_definitions_closed() {
    let (types_unit, invoke_unit, output_unit) = standard_v3_units();
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let rejects_catalogue = |catalogue: CatalogueSnapshot, label: &str| {
        let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
        origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
        origins.extend(standard_v3_output_origins(
            &catalogue,
            STANDARD_V3_OUTPUT_SOURCE,
        ));
        let executable = standard_v2_executable(&catalogue, &origins);
        let error = check_v3_parts(
            vec![types_unit.clone(), invoke_unit.clone(), output_unit.clone()],
            &catalogue,
            &origins,
            &[executable],
        )
        .unwrap_err();
        assert!(
            matches!(
                error,
                StandardLibraryCheckError::SourceMismatch
                    | StandardLibraryCheckError::MissingSchema
            ),
            "{label}: unexpected rejection: {error}"
        );
    };

    // Wrong document kernel contract at the fixed identity.
    rejects_catalogue(
        standard_v3_catalogue_with_output_value_type(
            1,
            ValueTypeDefinition::opaque(
                STD_TERMINAL_DOCUMENT_TYPE_ID,
                QualifiedSemanticName::new(["std", "terminal", "document"]).unwrap(),
                "orna.std.value.terminal-document@2",
            ),
        ),
        "wrong document contract",
    );
    // Document defined as a persistable primitive at the fixed identity,
    // not the opaque IMMUTABLE TRANSIENT output contract.
    rejects_catalogue(
        standard_v3_catalogue_with_output_value_type(
            1,
            ValueTypeDefinition::primitive(
                STD_TERMINAL_DOCUMENT_TYPE_ID,
                QualifiedSemanticName::new(["std", "terminal", "document"]).unwrap(),
                ValueTypeMutability::Immutable,
                ValueTypePersistence::Persistable,
                "orna.std.value.terminal-document@1",
            ),
        ),
        "wrong document mutability and persistence",
    );
    // ByteStream defined as a persistable primitive at the fixed identity.
    rejects_catalogue(
        standard_v3_catalogue_with_output_value_type(
            2,
            ValueTypeDefinition::primitive(
                STD_IO_BYTE_STREAM_TYPE_ID,
                QualifiedSemanticName::new(["std", "io", "bytestream"]).unwrap(),
                ValueTypeMutability::Immutable,
                ValueTypePersistence::Persistable,
                "orna.std.value.byte-stream@1",
            ),
        ),
        "wrong bytestream mutability and persistence",
    );
    // The terminal schema, document value type, and document binding are
    // missing from the catalogue.
    let catalogue = standard_v3_catalogue(true);
    let mut schemas = catalogue.schemas().to_vec();
    schemas.retain(|schema| schema.id() != STD_TERMINAL_SCHEMA_ID);
    let mut value_types = catalogue.value_types().to_vec();
    value_types.retain(|value_type| value_type.id() != STD_TERMINAL_DOCUMENT_TYPE_ID);
    let mut type_bindings = catalogue.type_bindings().to_vec();
    type_bindings.retain(|binding| binding.target() != STD_TERMINAL_DOCUMENT_TYPE_ID);
    let catalogue = CatalogueSnapshot::new_with_functions_and_types(
        catalogue.revision(),
        schemas,
        vec![],
        value_types,
        type_bindings,
        catalogue.functions().to_vec(),
    )
    .unwrap();
    rejects_catalogue(catalogue, "missing terminal schema and document");
    // The std.Document binding targets the wrong value type.
    let catalogue = standard_v3_catalogue(true);
    let mut type_bindings = catalogue.type_bindings().to_vec();
    let document_lookup =
        TypeLookupName::qualified(QualifiedSemanticName::new(["std", "document"]).unwrap());
    let document_index = type_bindings
        .iter()
        .position(|binding| binding.name() == &document_lookup)
        .unwrap();
    type_bindings[document_index] = TypeBinding::qualified(
        QualifiedSemanticName::new(["std", "document"]).unwrap(),
        STD_IO_BYTE_STREAM_TYPE_ID,
    )
    .unwrap();
    let catalogue = CatalogueSnapshot::new_with_functions_and_types(
        catalogue.revision(),
        catalogue.schemas().to_vec(),
        vec![],
        catalogue.value_types().to_vec(),
        type_bindings,
        catalogue.functions().to_vec(),
    )
    .unwrap();
    rejects_catalogue(catalogue, "wrong document binding target");
}

#[test]
fn rejects_swapped_output_declaration_order_closed() {
    // The retained origins bind each identity to its exact declaration
    // byte range; a source that swaps the two schema declarations shifts
    // those ranges and fails closed.
    let (types_unit, invoke_unit, _) = standard_v3_units();
    let catalogue = standard_v3_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    origins.extend(standard_v3_output_origins(
        &catalogue,
        STANDARD_V3_OUTPUT_SOURCE,
    ));
    let executable = standard_v2_executable(&catalogue, &origins);

    let swapped = STANDARD_V3_OUTPUT_SOURCE.replacen(
        "CREATE SCHEMA std.terminal;\nCREATE SCHEMA std.io;\n",
        "CREATE SCHEMA std.io;\nCREATE SCHEMA std.terminal;\n",
        1,
    );
    assert_ne!(swapped, STANDARD_V3_OUTPUT_SOURCE);
    let output_unit = stored_v2_unit(STD_OUTPUT_SOURCE_UNIT_ID, 2, "std/output.orna", &swapped);
    let error = check_v3_parts(
        vec![types_unit, invoke_unit, output_unit],
        &catalogue,
        &origins,
        &[executable],
    )
    .unwrap_err();
    assert!(
        matches!(error, StandardLibraryCheckError::SourceMismatch),
        "unexpected rejection: {error}"
    );
}

#[test]
fn rejects_wrong_v3_output_origin_ranges_closed() {
    let (types_unit, invoke_unit, output_unit) = standard_v3_units();
    let catalogue = standard_v3_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let assert_rejected = |error: StandardLibraryCheckError| {
        assert!(
            matches!(error, StandardLibraryCheckError::SourceMismatch),
            "unexpected rejection: {error}"
        );
    };
    let document_lookup =
        TypeLookupName::qualified(QualifiedSemanticName::new(["std", "document"]).unwrap());
    let document_binding_id = catalogue
        .type_binding_by_name(&document_lookup)
        .unwrap()
        .id();

    // Shifted document type origin range.
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    let mut output_origins = standard_v3_output_origins(&catalogue, STANDARD_V3_OUTPUT_SOURCE);
    let document_origin = output_origins
        .iter_mut()
        .find(|origin| {
            origin.identity() == DefinitionIdentity::ValueType(STD_TERMINAL_DOCUMENT_TYPE_ID)
        })
        .unwrap();
    *document_origin = DefinitionOrigin::new(
        document_origin.identity(),
        SourceOrigin::new(
            STD_OUTPUT_SOURCE_UNIT_ID,
            document_origin.source().byte_start() + 1,
            document_origin.source().byte_end(),
        )
        .unwrap(),
    );
    origins.extend(output_origins);
    let executable = standard_v2_executable(&catalogue, &origins);
    assert_rejected(
        check_v3_parts(
            vec![types_unit.clone(), invoke_unit.clone(), output_unit.clone()],
            &catalogue,
            &origins,
            &[executable],
        )
        .unwrap_err(),
    );

    // Missing document export origin.
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    let mut output_origins = standard_v3_output_origins(&catalogue, STANDARD_V3_OUTPUT_SOURCE);
    output_origins
        .retain(|origin| origin.identity() != DefinitionIdentity::TypeBinding(document_binding_id));
    origins.extend(output_origins);
    let executable = standard_v2_executable(&catalogue, &origins);
    assert_rejected(
        check_v3_parts(
            vec![types_unit.clone(), invoke_unit.clone(), output_unit.clone()],
            &catalogue,
            &origins,
            &[executable],
        )
        .unwrap_err(),
    );

    // Duplicate output origin identity.
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    let mut output_origins = standard_v3_output_origins(&catalogue, STANDARD_V3_OUTPUT_SOURCE);
    let schema_origin = output_origins
        .iter()
        .find(|origin| origin.identity() == DefinitionIdentity::Schema(STD_TERMINAL_SCHEMA_ID))
        .unwrap()
        .clone();
    output_origins.push(schema_origin);
    origins.extend(output_origins);
    let executable = standard_v2_executable(&catalogue, &origins);
    assert_rejected(
        check_v3_parts(
            vec![types_unit.clone(), invoke_unit.clone(), output_unit.clone()],
            &catalogue,
            &origins,
            &[executable],
        )
        .unwrap_err(),
    );

    // Output origin on a foreign source unit.
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    let mut output_origins = standard_v3_output_origins(&catalogue, STANDARD_V3_OUTPUT_SOURCE);
    let schema_origin = output_origins
        .iter_mut()
        .find(|origin| origin.identity() == DefinitionIdentity::Schema(STD_IO_SCHEMA_ID))
        .unwrap();
    *schema_origin = DefinitionOrigin::new(
        schema_origin.identity(),
        SourceOrigin::new(
            SourceUnitId::from_bytes([0x99; 16]),
            schema_origin.source().byte_start(),
            schema_origin.source().byte_end(),
        )
        .unwrap(),
    );
    origins.extend(output_origins);
    let executable = standard_v2_executable(&catalogue, &origins);
    assert_rejected(
        check_v3_parts(
            vec![types_unit, invoke_unit, output_unit],
            &catalogue,
            &origins,
            &[executable],
        )
        .unwrap_err(),
    );
}

#[test]
fn rejects_a_byte_modified_output_unit_closed() {
    let (types_unit, invoke_unit, _) = standard_v3_units();
    let catalogue = standard_v3_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    origins.extend(standard_v3_output_origins(
        &catalogue,
        STANDARD_V3_OUTPUT_SOURCE,
    ));
    let executable = standard_v2_executable(&catalogue, &origins);

    // Whitespace-only modification: tokens identical, declaration byte
    // ranges shift, so the stored origins no longer agree with the
    // retained source.
    let modified = STANDARD_V3_OUTPUT_SOURCE.replacen(
        "CREATE SCHEMA std.terminal;",
        "CREATE  SCHEMA std.terminal;",
        1,
    );
    assert_ne!(modified, STANDARD_V3_OUTPUT_SOURCE);
    let output_unit = stored_v2_unit(STD_OUTPUT_SOURCE_UNIT_ID, 2, "std/output.orna", &modified);
    let error = check_v3_parts(
        vec![types_unit.clone(), invoke_unit.clone(), output_unit],
        &catalogue,
        &origins,
        &[executable],
    )
    .unwrap_err();
    assert!(
        matches!(error, StandardLibraryCheckError::SourceMismatch),
        "unexpected rejection: {error}"
    );

    // Semantic modification: the schema declaration itself is rejected.
    let modified =
        STANDARD_V3_OUTPUT_SOURCE.replacen("CREATE SCHEMA std.io;", "CREATE SCHEMA std.other;", 1);
    let output_unit = stored_v2_unit(STD_OUTPUT_SOURCE_UNIT_ID, 2, "std/output.orna", &modified);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    origins.extend(standard_v3_output_origins(&catalogue, &modified));
    let executable = standard_v2_executable(&catalogue, &origins);
    let error = check_v3_parts(
        vec![types_unit, invoke_unit, output_unit],
        &catalogue,
        &origins,
        &[executable],
    )
    .unwrap_err();
    assert!(
        matches!(error, StandardLibraryCheckError::SourceMismatch),
        "unexpected rejection: {error}"
    );
}

#[test]
fn rejects_a_byte_modified_invoke_unit_through_the_v3_path() {
    let (types_unit, _, output_unit) = standard_v3_units();
    let catalogue = standard_v3_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    origins.extend(standard_v3_output_origins(
        &catalogue,
        STANDARD_V3_OUTPUT_SOURCE,
    ));
    let executable = standard_v2_executable(&catalogue, &origins);

    // The invoke unit reconciles exactly as the V2 checker does: a
    // semantic modification fails closed through the echo checker.
    let modified = STD_INVOKE_SOURCE.replacen("p_value INTEGER", "p_value BIGINT", 1);
    let invoke_unit = stored_v2_unit(STD_INVOKE_SOURCE_UNIT_ID, 1, "std/invoke.orna", &modified);
    let error = check_v3_parts(
        vec![types_unit, invoke_unit, output_unit],
        &catalogue,
        &origins,
        &[executable],
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::UnexpectedParameterType
    ));
}

#[test]
fn rejects_a_wrong_stored_executable_through_the_v3_path() {
    let (types_unit, invoke_unit, output_unit) = standard_v3_units();
    let catalogue = standard_v3_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    origins.extend(standard_v3_output_origins(
        &catalogue,
        STANDARD_V3_OUTPUT_SOURCE,
    ));
    let executable = standard_v2_executable(&catalogue, &origins);

    // Rebuild the stored executable with a different revision identity.
    let wrong_revision = FunctionRevisionId::from_bytes([0x11; 16]);
    let revision = executable.revision().clone();
    let references = executable
        .references()
        .iter()
        .map(|reference| {
            DefinitionReference::new(
                reference.source_function(),
                wrong_revision,
                reference.ordinal(),
                reference.target(),
                reference.kind(),
                reference.source_origin(),
            )
        })
        .collect::<Vec<_>>();
    let revision = FunctionRevisionRecord::new(
        revision.function(),
        wrong_revision,
        revision.revision_number(),
        revision.declaration_origin(),
        revision.declaration_content_hash(),
        revision.semantic_hash(),
        revision.language_version(),
        revision.artifact().clone(),
    )
    .unwrap()
    .with_semantic_hash_version(revision.semantic_hash_version());
    let executable = StandardExecutable::new(revision.function(), revision, references).unwrap();

    let error = check_v3_parts(
        vec![types_unit, invoke_unit, output_unit],
        &catalogue,
        &origins,
        &[executable],
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::ExecutableMismatch
    ));
}

/// The exact retained ADR 0062 `std/ui.orna` source: the single `std.ui`
/// schema declaration, the single opaque UI value type declaration, and
/// its single qualified export.
const STANDARD_V4_UI_SOURCE: &str = "CREATE SCHEMA std.ui;\n\nCREATE TYPE std.ui.UI AS VALUE\n    OPAQUE\n    KERNEL CONTRACT 'orna.std.value.ui@1'\n    IMMUTABLE\n    TRANSIENT;\n\nEXPORT TYPE std.ui.UI AS std.UI;";

fn standard_v4_catalogue(with_invoke: bool) -> CatalogueSnapshot {
    let catalogue = standard_v3_catalogue(with_invoke);
    let mut schemas = catalogue.schemas().to_vec();
    schemas.push(SchemaDefinition::new(
        STD_UI_SCHEMA_ID,
        QualifiedSemanticName::new(["std", "ui"]).unwrap(),
    ));
    let mut value_types = catalogue.value_types().to_vec();
    value_types.push(ValueTypeDefinition::opaque(
        STD_UI_TYPE_ID,
        QualifiedSemanticName::new(["std", "ui", "ui"]).unwrap(),
        STD_UI_CONTRACT,
    ));
    let mut type_bindings = catalogue.type_bindings().to_vec();
    type_bindings.push(
        TypeBinding::qualified(
            QualifiedSemanticName::new(["std", "ui"]).unwrap(),
            STD_UI_TYPE_ID,
        )
        .unwrap(),
    );
    CatalogueSnapshot::new_with_functions_and_types(
        catalogue.revision(),
        schemas,
        vec![],
        value_types,
        type_bindings,
        catalogue.functions().to_vec(),
    )
    .unwrap()
}

fn standard_v4_catalogue_with_ui_value_type(
    index: usize,
    definition: ValueTypeDefinition,
) -> CatalogueSnapshot {
    let catalogue = standard_v4_catalogue(true);
    let mut value_types = catalogue.value_types().to_vec();
    value_types[index] = definition;
    CatalogueSnapshot::new_with_functions_and_types(
        catalogue.revision(),
        catalogue.schemas().to_vec(),
        vec![],
        value_types,
        catalogue.type_bindings().to_vec(),
        catalogue.functions().to_vec(),
    )
    .unwrap()
}

fn standard_v4_units() -> (
    StoredSourceUnit,
    StoredSourceUnit,
    StoredSourceUnit,
    StoredSourceUnit,
) {
    let (types_unit, invoke_unit, output_unit) = standard_v3_units();
    (
        types_unit,
        invoke_unit,
        output_unit,
        stored_v2_unit(
            STD_UI_SOURCE_UNIT_ID,
            3,
            "std/ui.orna",
            STANDARD_V4_UI_SOURCE,
        ),
    )
}

fn standard_v4_ui_origins(catalogue: &CatalogueSnapshot, source: &str) -> Vec<DefinitionOrigin> {
    let report =
        parse_bundle(&SourceBundle::new([SourceUnit::new("std/ui.orna", source)]).unwrap());
    assert!(report.diagnostics().is_empty(), "{source}");
    let parsed = &report.units()[0];
    let origin = |identity: DefinitionIdentity, span: &SourceSpan| -> DefinitionOrigin {
        DefinitionOrigin::new(
            identity,
            SourceOrigin::new(
                STD_UI_SOURCE_UNIT_ID,
                u32::try_from(span.start).unwrap(),
                u32::try_from(span.end).unwrap(),
            )
            .unwrap(),
        )
    };
    let ui_binding = catalogue.type_binding_by_name(&TypeLookupName::qualified(
        QualifiedSemanticName::new(["std", "ui"]).unwrap(),
    ));
    let mut origins = Vec::with_capacity(3);
    if let Some(schema) = parsed.parsed().schemas().first() {
        origins.push(origin(
            DefinitionIdentity::Schema(STD_UI_SCHEMA_ID),
            &schema.span,
        ));
    }
    if let Some(value_type) = parsed.parsed().opaque_value_types().first() {
        origins.push(origin(
            DefinitionIdentity::ValueType(STD_UI_TYPE_ID),
            &value_type.span,
        ));
    }
    if let (Some(binding), Some(export)) = (ui_binding, parsed.parsed().type_exports().first()) {
        origins.push(origin(
            DefinitionIdentity::TypeBinding(binding.id()),
            &export.span,
        ));
    }
    origins
}

fn standard_v4_parts() -> (
    Vec<StoredSourceUnit>,
    CatalogueSnapshot,
    Vec<DefinitionOrigin>,
    Vec<StandardExecutable>,
) {
    let (types_unit, invoke_unit, output_unit, ui_unit) = standard_v4_units();
    let catalogue = standard_v4_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    origins.extend(standard_v3_output_origins(
        &catalogue,
        STANDARD_V3_OUTPUT_SOURCE,
    ));
    origins.extend(standard_v4_ui_origins(&catalogue, STANDARD_V4_UI_SOURCE));
    let executable = standard_v2_executable(&catalogue, &origins);
    (
        vec![types_unit, invoke_unit, output_unit, ui_unit],
        catalogue,
        origins,
        vec![executable],
    )
}

fn standard_v4_source(units: Vec<StoredSourceUnit>) -> StoredSourceRevision {
    let bundle_hash = source_bundle_digest(&units).unwrap();
    StoredSourceRevision::new(
        SourceBundleId::from_bytes([0x61; 16]),
        SourceRevisionId::from_bytes([0x62; 16]),
        Some(SourceRevisionId::from_bytes([0x63; 16])),
        units,
        bundle_hash,
        source_revision_record_digest(
            SourceBundleId::from_bytes([0x61; 16]),
            Some(SourceRevisionId::from_bytes([0x63; 16])),
            bundle_hash,
        )
        .unwrap(),
    )
    .unwrap()
}

/// Runs the V4 source reconcile directly on raw stored facts, without the
/// separate digest-verification gate.
fn check_v4_parts(
    units: Vec<StoredSourceUnit>,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    executables: &[StandardExecutable],
) -> Result<(StandardSourceFamilies, CheckedStandardExecutable), StandardLibraryCheckError> {
    check_standard_library_source_v4_parts(
        &standard_v4_source(units),
        catalogue,
        origins,
        executables,
    )
}

fn build_standard_v4_snapshot(
    units: Vec<StoredSourceUnit>,
    catalogue: CatalogueSnapshot,
    origins: Vec<DefinitionOrigin>,
    executables: Vec<StandardExecutable>,
    digest: Sha256Digest,
) -> StandardLibrarySnapshot {
    StandardLibrarySnapshot::new_with_executables(
        STANDARD_LIBRARY_V4_REVISION_ID,
        StandardLibraryDigestVersion::Version2,
        standard_v4_source(units),
        "orna.language/1",
        catalogue,
        executables,
        origins,
        digest,
    )
    .unwrap()
}

/// The compiled canonical V4 standard-library digest for the exact test
/// inputs (`STANDARD_V2_TYPES_SOURCE`, `STD_INVOKE_SOURCE`,
/// `STANDARD_V3_OUTPUT_SOURCE`, `STANDARD_V4_UI_SOURCE`, the fixed
/// identities, catalogue, executable, and origins). Computed by the
/// canonical encoder.
const STANDARD_V4_CANONICAL_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xc3, 0xc4, 0x05, 0x29, 0xba, 0x69, 0xe3, 0x4e, 0x6d, 0x44, 0x1a, 0x83, 0x86, 0x9f, 0x5a, 0x9e,
    0x30, 0xc8, 0x71, 0x4d, 0x20, 0x55, 0x06, 0xfa, 0xa0, 0x5c, 0xd3, 0x96, 0x47, 0x09, 0xb5, 0xfc,
]);

fn verified_standard_v4_snapshot() -> VerifiedStandardLibrarySnapshot {
    let (units, catalogue, origins, executables) = standard_v4_parts();
    verify_standard_library_v2_snapshot(build_standard_v4_snapshot(
        units,
        catalogue,
        origins,
        executables,
        STANDARD_V4_CANONICAL_DIGEST,
    ))
    .unwrap()
}

/// Reconciles the exact retained V4 bundle (types, invoke, output, ui)
/// against the source-independent V4 catalogue and proves the ui unit
/// contributes its schema, opaque value type, and qualified export at the
/// exact declaration byte ranges.
#[test]
fn reconciles_the_exact_v4_standard_bundle_with_the_ui_unit() {
    let verified = verified_standard_v4_snapshot();
    assert_eq!(verified.revision(), STANDARD_LIBRARY_V4_REVISION_ID);
    assert_eq!(
        verified.digest_version(),
        StandardLibraryDigestVersion::Version2
    );
    let checked = check_standard_library_source(&verified).unwrap();

    // The types/invoke reconcile surfaces the V2 schema, value type, and
    // binding facts unchanged; the output and ui units are reconciled
    // closed without contributing to the families.
    assert_eq!(checked.schemas().len(), 2);
    assert_eq!(checked.value_types().len(), 1);
    assert_eq!(checked.type_bindings().len(), 2);

    let executable = checked.checked_executable().unwrap();
    assert_eq!(executable.function_id(), STD_INVOKE_ECHO_FUNCTION_ID);

    // The ui unit contributes exactly -one- additional schema, the opaque
    // std.ui.ui value type, and the std.UI qualified binding; all present
    // in the retained snapshot at the exact declaration byte ranges.
    let ui_schema_origin = verified
        .origins()
        .iter()
        .find(|origin| {
            origin.source().source_unit() == STD_UI_SOURCE_UNIT_ID
                && origin.identity() == DefinitionIdentity::Schema(STD_UI_SCHEMA_ID)
        })
        .unwrap();
    assert_eq!(
        &STANDARD_V4_UI_SOURCE[ui_schema_origin.source().byte_start() as usize
            ..ui_schema_origin.source().byte_end() as usize],
        "CREATE SCHEMA std.ui;"
    );
    let ui_type_origin = verified
        .origins()
        .iter()
        .find(|origin| {
            origin.source().source_unit() == STD_UI_SOURCE_UNIT_ID
                && origin.identity() == DefinitionIdentity::ValueType(STD_UI_TYPE_ID)
        })
        .unwrap();
    assert_eq!(
        &STANDARD_V4_UI_SOURCE[ui_type_origin.source().byte_start() as usize
            ..ui_type_origin.source().byte_end() as usize],
        "CREATE TYPE std.ui.UI AS VALUE\n    OPAQUE\n    KERNEL CONTRACT 'orna.std.value.ui@1'\n    IMMUTABLE\n    TRANSIENT;"
    );
    let ui_binding_origin = verified
        .origins()
        .iter()
        .find(|origin| {
            origin.source().source_unit() == STD_UI_SOURCE_UNIT_ID
                && matches!(origin.identity(), DefinitionIdentity::TypeBinding(_))
        })
        .unwrap();
    assert_eq!(
        &STANDARD_V4_UI_SOURCE[ui_binding_origin.source().byte_start() as usize
            ..ui_binding_origin.source().byte_end() as usize],
        "EXPORT TYPE std.ui.UI AS std.UI;"
    );
    assert_eq!(verified.origins().len(), 17);
}

#[test]
fn rejects_a_v4_bundle_with_the_wrong_ui_unit_identity_order_or_path() {
    let (types_unit, invoke_unit, output_unit, _ui_unit) = standard_v4_units();
    let catalogue = standard_v4_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    origins.extend(standard_v3_output_origins(
        &catalogue,
        STANDARD_V3_OUTPUT_SOURCE,
    ));
    origins.extend(standard_v4_ui_origins(&catalogue, STANDARD_V4_UI_SOURCE));
    let executable = standard_v2_executable(&catalogue, &origins);
    let rejects = |units: Vec<StoredSourceUnit>, label: &str| {
        let error = check_v4_parts(
            units,
            &catalogue,
            &origins,
            std::slice::from_ref(&executable),
        )
        .unwrap_err();
        assert!(
            matches!(error, StandardLibraryCheckError::SourceMismatch),
            "{label}: unexpected rejection: {error}"
        );
    };

    rejects(
        vec![
            types_unit.clone(),
            invoke_unit.clone(),
            output_unit.clone(),
            stored_v2_unit(
                SourceUnitId::from_bytes([0x79; 16]),
                3,
                "std/ui.orna",
                STANDARD_V4_UI_SOURCE,
            ),
        ],
        "wrong ui unit identity",
    );
    // The ui content placed in the output slot (ordinal 2) with the output
    // unit displaced to ordinal 3 keeps the ordinals in sequence so the
    // parts checker sees a ui unit whose identity/ordinal do not match.
    rejects(
        vec![
            types_unit.clone(),
            invoke_unit.clone(),
            stored_v2_unit(
                STD_UI_SOURCE_UNIT_ID,
                2,
                "std/ui.orna",
                STANDARD_V4_UI_SOURCE,
            ),
            stored_v2_unit(
                STD_OUTPUT_SOURCE_UNIT_ID,
                3,
                "std/output.orna",
                STANDARD_V3_OUTPUT_SOURCE,
            ),
        ],
        "ui unit at wrong ordinal",
    );
    rejects(
        vec![
            types_unit,
            invoke_unit,
            output_unit,
            stored_v2_unit(
                STD_UI_SOURCE_UNIT_ID,
                3,
                "std/display.orna",
                STANDARD_V4_UI_SOURCE,
            ),
        ],
        "wrong ui unit path",
    );
}

#[test]
fn rejects_a_v4_bundle_with_a_missing_or_extra_unit() {
    let (types_unit, invoke_unit, output_unit, ui_unit) = standard_v4_units();
    let catalogue = standard_v4_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    let executable = standard_v2_executable(&catalogue, &origins);

    let error = check_v4_parts(
        vec![types_unit.clone(), invoke_unit.clone(), output_unit.clone()],
        &catalogue,
        &origins,
        std::slice::from_ref(&executable),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::SourceUnitCount { actual: 3 }
    ));

    let extra = stored_v2_unit(
        SourceUnitId::from_bytes([0x78; 16]),
        4,
        "std/extra.orna",
        "CREATE SCHEMA std.extra;",
    );
    let full_origins = {
        let mut o = origins.clone();
        o.extend(standard_v3_output_origins(
            &catalogue,
            STANDARD_V3_OUTPUT_SOURCE,
        ));
        o.extend(standard_v4_ui_origins(&catalogue, STANDARD_V4_UI_SOURCE));
        o
    };
    let full_executable = standard_v2_executable(&catalogue, &full_origins);
    let error = check_v4_parts(
        vec![types_unit, invoke_unit, output_unit.clone(), ui_unit, extra],
        &catalogue,
        &full_origins,
        &[full_executable],
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::SourceUnitCount { actual: 5 }
    ));
}

#[test]
fn rejects_every_ui_unit_content_variation_closed() {
    let (types_unit, invoke_unit, output_unit, _ui_unit) = standard_v4_units();
    let catalogue = standard_v4_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut base_origins = standard_v2_types_origins(&catalogue, &parsed_types);
    base_origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    base_origins.extend(standard_v3_output_origins(
        &catalogue,
        STANDARD_V3_OUTPUT_SOURCE,
    ));
    let rejects_ui = |source: &str, label: &str| {
        let ui_unit = stored_v2_unit(STD_UI_SOURCE_UNIT_ID, 3, "std/ui.orna", source);
        let mut origins = base_origins.clone();
        origins.extend(standard_v4_ui_origins(&catalogue, source));
        let executable = standard_v2_executable(&catalogue, &origins);
        let error = check_v4_parts(
            vec![
                types_unit.clone(),
                invoke_unit.clone(),
                output_unit.clone(),
                ui_unit,
            ],
            &catalogue,
            &origins,
            &[executable],
        )
        .unwrap_err();
        assert!(
            matches!(error, StandardLibraryCheckError::SourceMismatch),
            "{label}: unexpected rejection: {error}"
        );
    };

    rejects_ui(
        &STANDARD_V4_UI_SOURCE.replace("CREATE SCHEMA std.ui;", "CREATE SCHEMA std.ux;"),
        "wrong ui schema name",
    );
    rejects_ui(
        &STANDARD_V4_UI_SOURCE.replace(
            "CREATE TYPE std.ui.UI AS VALUE",
            "CREATE TYPE std.ui.Window AS VALUE",
        ),
        "wrong ui type local name",
    );
    rejects_ui(
        &STANDARD_V4_UI_SOURCE.replace(
            "KERNEL CONTRACT 'orna.std.value.ui@1'",
            "KERNEL CONTRACT 'orna.std.value.window@1'",
        ),
        "wrong ui kernel contract",
    );
    rejects_ui(
        &STANDARD_V4_UI_SOURCE.replace(
            "EXPORT TYPE std.ui.UI AS std.UI;",
            "EXPORT TYPE std.ui.UI AS std.Window;",
        ),
        "wrong ui export binding",
    );
}

const STANDARD_V5_JSON_SOURCE: &str = include_str!("../../../../stdlib/std/json.orna");
const STANDARD_V6_ACTION_SOURCE: &str = include_str!("../../../../stdlib/std/action.orna");

fn standard_v5_catalogue() -> CatalogueSnapshot {
    let base = standard_v4_catalogue(true);
    let mut schemas = base.schemas().to_vec();
    schemas.push(SchemaDefinition::new(
        STD_JSON_SCHEMA_ID,
        QualifiedSemanticName::new(["std", "json"]).unwrap(),
    ));
    let mut value_types = base.value_types().to_vec();
    value_types.push(ValueTypeDefinition::opaque(
        STD_JSON_VALUE_TYPE_ID,
        QualifiedSemanticName::new(["std", "json", "value"]).unwrap(),
        STD_JSON_CONTRACT,
    ));
    let mut type_bindings = base.type_bindings().to_vec();
    type_bindings.push(
        TypeBinding::qualified(
            QualifiedSemanticName::new(["std", "jsonvalue"]).unwrap(),
            STD_JSON_VALUE_TYPE_ID,
        )
        .unwrap(),
    );
    let mut functions = base.functions().to_vec();
    functions.push(FunctionDefinition::new(
        STD_JSON_ENCODE_FUNCTION_ID,
        QualifiedSemanticName::new(["std", "json", "encode"]).unwrap(),
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            STD_JSON_ENCODE_PARAMETER_ID,
            "p_value",
            0,
            ResolvedType::Named(STD_JSON_VALUE_TYPE_ID),
            None,
        )],
        FunctionReturn::Single(ResolvedType::Named(STD_IO_BYTE_STREAM_TYPE_ID)),
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    ));
    CatalogueSnapshot::new_with_functions_and_types(
        base.revision(),
        schemas,
        vec![],
        value_types,
        type_bindings,
        functions,
    )
    .unwrap()
}

fn standard_v5_json_origins(catalogue: &CatalogueSnapshot) -> Vec<DefinitionOrigin> {
    let report = parse_bundle(
        &SourceBundle::new([SourceUnit::new("std/json.orna", STANDARD_V5_JSON_SOURCE)]).unwrap(),
    );
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let parsed = &report.units()[0];
    let binding = catalogue
        .type_binding_by_name(&TypeLookupName::qualified(
            QualifiedSemanticName::new(["std", "jsonvalue"]).unwrap(),
        ))
        .unwrap();
    let origin = |identity: DefinitionIdentity, span: &SourceSpan| {
        DefinitionOrigin::new(
            identity,
            SourceOrigin::new(
                super::STD_JSON_SOURCE_UNIT_ID,
                u32::try_from(span.start).unwrap(),
                u32::try_from(span.end).unwrap(),
            )
            .unwrap(),
        )
    };
    let schema = &parsed.parsed().schemas()[0];
    let value_type = &parsed.parsed().opaque_value_types()[0];
    let export = &parsed.parsed().type_exports()[0];
    let function = &parsed.parsed().server_functions()[0];
    vec![
        origin(DefinitionIdentity::Schema(STD_JSON_SCHEMA_ID), &schema.span),
        origin(
            DefinitionIdentity::ValueType(STD_JSON_VALUE_TYPE_ID),
            &value_type.span,
        ),
        origin(DefinitionIdentity::TypeBinding(binding.id()), &export.span),
        origin(
            DefinitionIdentity::Function(STD_JSON_ENCODE_FUNCTION_ID),
            &function.span,
        ),
        origin(
            DefinitionIdentity::Parameter {
                owner: STD_JSON_ENCODE_FUNCTION_ID,
                parameter: STD_JSON_ENCODE_PARAMETER_ID,
            },
            &function.parameters[0].span,
        ),
    ]
}

fn standard_v5_parts() -> (
    Vec<StoredSourceUnit>,
    CatalogueSnapshot,
    Vec<DefinitionOrigin>,
    Vec<StandardExecutable>,
) {
    let (types_unit, invoke_unit, output_unit, ui_unit) = standard_v4_units();
    let catalogue = standard_v5_catalogue();
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    origins.extend(standard_v3_output_origins(
        &catalogue,
        STANDARD_V3_OUTPUT_SOURCE,
    ));
    origins.extend(standard_v4_ui_origins(&catalogue, STANDARD_V4_UI_SOURCE));
    let json_origins = standard_v5_json_origins(&catalogue);
    origins.extend(json_origins.iter().cloned());
    let json_unit = stored_v2_unit(
        super::STD_JSON_SOURCE_UNIT_ID,
        4,
        "std/json.orna",
        STANDARD_V5_JSON_SOURCE,
    );
    let json_function = parsed_standard_unit(STANDARD_V5_JSON_SOURCE)
        .parsed()
        .server_functions()[0]
        .clone();
    let json_executable =
        expected_standard_json_executable(&json_function, &catalogue, &json_origins, &json_unit)
            .unwrap();
    let executable = standard_v2_executable(&catalogue, &origins);
    (
        vec![types_unit, invoke_unit, output_unit, ui_unit, json_unit],
        catalogue,
        origins,
        vec![executable, json_executable],
    )
}

fn check_v5_parts(
    units: Vec<StoredSourceUnit>,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    executables: &[StandardExecutable],
) -> Result<(StandardSourceFamilies, CheckedStandardExecutable), StandardLibraryCheckError> {
    check_standard_library_source_v5_parts(&units, catalogue, origins, executables)
}

fn standard_v6_catalogue() -> CatalogueSnapshot {
    let base = standard_v5_catalogue();
    let mut schemas = base.schemas().to_vec();
    schemas.push(SchemaDefinition::new(
        STD_ACTION_SCHEMA_ID,
        QualifiedSemanticName::new(["std", "action"]).unwrap(),
    ));
    let mut value_types = base.value_types().to_vec();
    value_types.push(ValueTypeDefinition::opaque(
        STD_ACTION_TYPE_ID,
        QualifiedSemanticName::new(["std", "action", "action"]).unwrap(),
        STD_ACTION_CONTRACT,
    ));
    let mut type_bindings = base.type_bindings().to_vec();
    type_bindings.push(
        TypeBinding::qualified(
            QualifiedSemanticName::new(["std", "action"]).unwrap(),
            STD_ACTION_TYPE_ID,
        )
        .unwrap(),
    );
    CatalogueSnapshot::new_with_functions_and_types(
        base.revision(),
        schemas,
        vec![],
        value_types,
        type_bindings,
        base.functions().to_vec(),
    )
    .unwrap()
}

fn standard_v6_action_origins(catalogue: &CatalogueSnapshot) -> Vec<DefinitionOrigin> {
    let report = parse_bundle(
        &SourceBundle::new([SourceUnit::new(
            "std/action.orna",
            STANDARD_V6_ACTION_SOURCE,
        )])
        .unwrap(),
    );
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let parsed = &report.units()[0];
    let binding = catalogue
        .type_binding_by_name(&TypeLookupName::qualified(
            QualifiedSemanticName::new(["std", "action"]).unwrap(),
        ))
        .unwrap();
    let origin = |identity: DefinitionIdentity, span: &SourceSpan| {
        DefinitionOrigin::new(
            identity,
            SourceOrigin::new(
                STD_ACTION_SOURCE_UNIT_ID,
                u32::try_from(span.start).unwrap(),
                u32::try_from(span.end).unwrap(),
            )
            .unwrap(),
        )
    };
    let schema = &parsed.parsed().schemas()[0];
    let value_type = &parsed.parsed().opaque_value_types()[0];
    let export = &parsed.parsed().type_exports()[0];
    vec![
        origin(
            DefinitionIdentity::Schema(STD_ACTION_SCHEMA_ID),
            &schema.span,
        ),
        origin(
            DefinitionIdentity::ValueType(STD_ACTION_TYPE_ID),
            &value_type.span,
        ),
        origin(DefinitionIdentity::TypeBinding(binding.id()), &export.span),
    ]
}

fn standard_v6_parts() -> (
    Vec<StoredSourceUnit>,
    CatalogueSnapshot,
    Vec<DefinitionOrigin>,
    Vec<StandardExecutable>,
) {
    let (v5_units, _, v5_origins, executables) = standard_v5_parts();
    let catalogue = standard_v6_catalogue();
    let mut origins = v5_origins;
    origins.extend(standard_v6_action_origins(&catalogue));
    let action_unit = stored_v2_unit(
        STD_ACTION_SOURCE_UNIT_ID,
        5,
        "std/action.orna",
        STANDARD_V6_ACTION_SOURCE,
    );
    (
        v5_units.into_iter().chain([action_unit]).collect(),
        catalogue,
        origins,
        executables,
    )
}

fn check_v6_parts(
    units: Vec<StoredSourceUnit>,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    executables: &[StandardExecutable],
) -> Result<(StandardSourceFamilies, CheckedStandardExecutable), StandardLibraryCheckError> {
    check_standard_library_source_v6_parts(&units, catalogue, origins, executables)
}

#[test]
fn rejects_v5_when_a_retained_v4_unit_identity_order_path_or_ordinal_is_tampered() {
    let (units, catalogue, origins, executables) = standard_v5_parts();
    assert!(
        check_v5_parts(units.clone(), &catalogue, &origins, &executables).is_ok(),
        "canonical V5 fixture must be accepted before tamper checks",
    );
    for (label, replacement) in [
        (
            "identity",
            stored_v2_unit(
                SourceUnitId::from_bytes([0x9c; 16]),
                2,
                "std/output.orna",
                STANDARD_V3_OUTPUT_SOURCE,
            ),
        ),
        (
            "path",
            stored_v2_unit(
                STD_OUTPUT_SOURCE_UNIT_ID,
                2,
                "std/other.orna",
                STANDARD_V3_OUTPUT_SOURCE,
            ),
        ),
        (
            "ordinal",
            stored_v2_unit(
                STD_OUTPUT_SOURCE_UNIT_ID,
                9,
                "std/output.orna",
                STANDARD_V3_OUTPUT_SOURCE,
            ),
        ),
    ] {
        let mut tampered = units.clone();
        tampered[2] = replacement;
        let error = check_v5_parts(tampered, &catalogue, &origins, &executables).unwrap_err();
        assert!(
            matches!(error, StandardLibraryCheckError::SourceMismatch),
            "{label}: {error}"
        );
    }

    assert!(
        check_v5_parts(units.clone(), &catalogue, &origins, &executables).is_ok(),
        "canonical V5 fixture must be accepted before order tamper",
    );
    let mut tampered = units;
    tampered[2] = stored_v2_unit(
        STD_UI_SOURCE_UNIT_ID,
        2,
        "std/ui.orna",
        STANDARD_V4_UI_SOURCE,
    );
    tampered[3] = stored_v2_unit(
        STD_OUTPUT_SOURCE_UNIT_ID,
        3,
        "std/output.orna",
        STANDARD_V3_OUTPUT_SOURCE,
    );
    let error = check_v5_parts(tampered, &catalogue, &origins, &executables).unwrap_err();
    assert!(
        matches!(error, StandardLibraryCheckError::SourceMismatch),
        "swapped retained V4 units: {error}"
    );
}

#[test]
fn rejects_v5_when_the_json_unit_declaration_or_identity_is_tampered() {
    let (units, catalogue, origins, executables) = standard_v5_parts();
    assert!(
        check_v5_parts(units.clone(), &catalogue, &origins, &executables).is_ok(),
        "canonical V5 fixture must be accepted before tamper checks",
    );
    let rejects_source = |source: &str, label: &str| {
        let mut tampered = units.clone();
        tampered[4] = stored_v2_unit(STD_JSON_SOURCE_UNIT_ID, 4, "std/json.orna", source);
        let error = check_v5_parts(tampered, &catalogue, &origins, &executables).unwrap_err();
        assert!(
            matches!(error, StandardLibraryCheckError::SourceMismatch),
            "{label}: {error}"
        );
    };

    rejects_source(
        &STANDARD_V5_JSON_SOURCE.replace("CREATE SCHEMA std.json;", "CREATE SCHEMA std.jason;"),
        "wrong JSON schema",
    );
    rejects_source(
        &STANDARD_V5_JSON_SOURCE.replace(
            "CREATE TYPE std.json.Value AS VALUE",
            "CREATE TYPE std.json.Token AS VALUE",
        ),
        "wrong JSON opaque type name",
    );
    rejects_source(
        &STANDARD_V5_JSON_SOURCE.replace("orna.std.value.json@1", "orna.std.value.token@1"),
        "wrong JSON kernel contract",
    );
    rejects_source(
        &STANDARD_V5_JSON_SOURCE.replace(
            "EXPORT TYPE std.json.Value AS std.JsonValue;",
            "EXPORT TYPE std.json.Value AS std.JsonToken;",
        ),
        "wrong JSON export",
    );
    rejects_source(
        &format!("-- tampered\n{STANDARD_V5_JSON_SOURCE}"),
        "changed JSON source content",
    );

    for (label, replacement) in [
        (
            "wrong JSON source-unit identity",
            stored_v2_unit(
                SourceUnitId::from_bytes([0x9a; 16]),
                4,
                "std/json.orna",
                STANDARD_V5_JSON_SOURCE,
            ),
        ),
        (
            "wrong JSON source-unit ordinal",
            stored_v2_unit(
                STD_JSON_SOURCE_UNIT_ID,
                6,
                "std/json.orna",
                STANDARD_V5_JSON_SOURCE,
            ),
        ),
        (
            "wrong JSON source-unit path",
            stored_v2_unit(
                STD_JSON_SOURCE_UNIT_ID,
                4,
                "std/document.orna",
                STANDARD_V5_JSON_SOURCE,
            ),
        ),
    ] {
        let mut tampered = units.clone();
        tampered[4] = replacement;
        let error = check_v5_parts(tampered, &catalogue, &origins, &executables).unwrap_err();
        assert!(
            matches!(error, StandardLibraryCheckError::SourceMismatch),
            "{label}: {error}"
        );
    }
}

#[test]
fn rejects_v6_when_a_retained_v4_unit_identity_order_path_or_ordinal_is_tampered() {
    let (units, catalogue, origins, executables) = standard_v6_parts();
    assert!(
        check_v6_parts(units.clone(), &catalogue, &origins, &executables).is_ok(),
        "canonical V6 fixture must be accepted before tamper checks",
    );
    for (label, replacement) in [
        (
            "identity",
            stored_v2_unit(
                SourceUnitId::from_bytes([0x9d; 16]),
                2,
                "std/output.orna",
                STANDARD_V3_OUTPUT_SOURCE,
            ),
        ),
        (
            "path",
            stored_v2_unit(
                STD_OUTPUT_SOURCE_UNIT_ID,
                2,
                "std/other.orna",
                STANDARD_V3_OUTPUT_SOURCE,
            ),
        ),
        (
            "ordinal",
            stored_v2_unit(
                STD_OUTPUT_SOURCE_UNIT_ID,
                9,
                "std/output.orna",
                STANDARD_V3_OUTPUT_SOURCE,
            ),
        ),
    ] {
        let mut tampered = units.clone();
        tampered[2] = replacement;
        let error = check_v6_parts(tampered, &catalogue, &origins, &executables).unwrap_err();
        assert!(
            matches!(error, StandardLibraryCheckError::SourceMismatch),
            "{label}: {error}"
        );
    }

    assert!(
        check_v6_parts(units.clone(), &catalogue, &origins, &executables).is_ok(),
        "canonical V6 fixture must be accepted before order tamper",
    );
    let mut tampered = units;
    tampered[2] = stored_v2_unit(
        STD_UI_SOURCE_UNIT_ID,
        2,
        "std/ui.orna",
        STANDARD_V4_UI_SOURCE,
    );
    tampered[3] = stored_v2_unit(
        STD_OUTPUT_SOURCE_UNIT_ID,
        3,
        "std/output.orna",
        STANDARD_V3_OUTPUT_SOURCE,
    );
    let error = check_v6_parts(tampered, &catalogue, &origins, &executables).unwrap_err();
    assert!(
        matches!(error, StandardLibraryCheckError::SourceMismatch),
        "swapped retained V4 units: {error}"
    );
}

#[test]
fn rejects_v6_when_the_action_unit_declaration_or_identity_is_tampered() {
    let (units, catalogue, origins, executables) = standard_v6_parts();
    assert!(
        check_v6_parts(units.clone(), &catalogue, &origins, &executables).is_ok(),
        "canonical V6 fixture must be accepted before tamper checks",
    );
    let rejects_source = |source: &str, label: &str| {
        let mut tampered = units.clone();
        tampered[5] = stored_v2_unit(STD_ACTION_SOURCE_UNIT_ID, 5, "std/action.orna", source);
        let error = check_v6_parts(tampered, &catalogue, &origins, &executables).unwrap_err();
        assert!(
            matches!(error, StandardLibraryCheckError::SourceMismatch),
            "{label}: {error}"
        );
    };

    rejects_source(
        &STANDARD_V6_ACTION_SOURCE.replace("CREATE SCHEMA std.action;", "CREATE SCHEMA std.acted;"),
        "wrong action schema",
    );
    rejects_source(
        &STANDARD_V6_ACTION_SOURCE.replace(
            "CREATE TYPE std.action.Action AS VALUE",
            "CREATE TYPE std.action.Command AS VALUE",
        ),
        "wrong action opaque type name",
    );
    rejects_source(
        &STANDARD_V6_ACTION_SOURCE.replace(
            "KERNEL CONTRACT 'orna.std.value.action@1'",
            "KERNEL CONTRACT 'orna.std.value.command@1'",
        ),
        "wrong action kernel contract",
    );
    rejects_source(
        &STANDARD_V6_ACTION_SOURCE.replace(
            "EXPORT TYPE std.action.Action AS std.Action;",
            "EXPORT TYPE std.action.Action AS std.Command;",
        ),
        "wrong action export",
    );
    rejects_source(
        &STANDARD_V6_ACTION_SOURCE.replace("OPAQUE", "PRIMITIVE"),
        "wrong action value kind",
    );
    rejects_source(
        &format!("-- tampered\n{STANDARD_V6_ACTION_SOURCE}"),
        "changed action source content",
    );

    for (label, replacement) in [
        (
            "wrong action source-unit identity",
            stored_v2_unit(
                SourceUnitId::from_bytes([0x9b; 16]),
                5,
                "std/action.orna",
                STANDARD_V6_ACTION_SOURCE,
            ),
        ),
        (
            "wrong action source-unit ordinal",
            stored_v2_unit(
                STD_ACTION_SOURCE_UNIT_ID,
                7,
                "std/action.orna",
                STANDARD_V6_ACTION_SOURCE,
            ),
        ),
        (
            "wrong action source-unit path",
            stored_v2_unit(
                STD_ACTION_SOURCE_UNIT_ID,
                5,
                "std/command.orna",
                STANDARD_V6_ACTION_SOURCE,
            ),
        ),
    ] {
        let mut tampered = units.clone();
        tampered[5] = replacement;
        let error = check_v6_parts(tampered, &catalogue, &origins, &executables).unwrap_err();
        assert!(
            matches!(error, StandardLibraryCheckError::SourceMismatch),
            "{label}: {error}"
        );
    }

    for (label, source, message) in [
        (
            "wrong action mutability",
            STANDARD_V6_ACTION_SOURCE.replace("IMMUTABLE", "MUTABLE"),
            "expected IMMUTABLE after opaque codec contract",
        ),
        (
            "wrong action persistence",
            STANDARD_V6_ACTION_SOURCE.replace("TRANSIENT", "PERSISTABLE"),
            "expected TRANSIENT after IMMUTABLE",
        ),
    ] {
        let mut tampered = units.clone();
        tampered[5] = stored_v2_unit(STD_ACTION_SOURCE_UNIT_ID, 5, "std/action.orna", &source);
        let error = check_v6_parts(tampered, &catalogue, &origins, &executables).unwrap_err();
        let StandardLibraryCheckError::Diagnostics { diagnostics } = error else {
            panic!("{label}: expected parser diagnostics");
        };
        assert_eq!(diagnostics.len(), 1, "{label}: {diagnostics:?}");
        assert_eq!(diagnostics[0].message(), message, "{label}");
    }
}

#[test]
fn checks_and_prepares_scalar_resource_against_standard_echo() {
    let verified = verified_standard_v2_snapshot();
    let standard = check_standard_library_source(&verified).unwrap();
    let unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes([0x91; 16]),
        0,
        "application.orna",
        "",
        source_unit_content_digest("").unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
    let source = StoredSourceRevision::new(
        SourceBundleId::from_bytes([0x92; 16]),
        SourceRevisionId::from_bytes([0x93; 16]),
        None,
        vec![unit],
        bundle_hash,
        source_revision_record_digest(SourceBundleId::from_bytes([0x92; 16]), None, bundle_hash)
            .unwrap(),
    )
    .unwrap();
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([0x94; 16]),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    let hash_context = CatalogueHashContext::version_two(verified.clone());
    let catalogue_hash =
        catalogue_digest_with_context(&hash_context, &catalogue, &[], &[], &[], &[]).unwrap();
    let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            RevisionPair::new(source.id(), catalogue.revision()),
            source,
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(Vec::new(), Vec::new(), Vec::new(), Vec::new()),
        ),
        hash_context,
    )
    .unwrap();
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let source = "CREATE SCHEMA scalar_fixture; CREATE CLIENT FUNCTION scalar_fixture.call() RETURNS INTEGER IS BEGIN RETURN AWAIT std.data.resource(target => std.invoke.echo, arguments => std.call.args(p_value => 43)); END;";
    let report = check_standard_application(&bundle([("resource.orna", source)]), &context);
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let checked = report.preparation_view().unwrap().checked();
    let function = checked
        .client_functions()
        .iter()
        .find(|function| function.name().to_string() == "scalar_fixture.call")
        .unwrap();
    let CheckedClientFunctionBody::Expression { expression } = function.body() else {
        panic!("resource body must be an expression");
    };
    let super::CheckedClientExpression::Await { expression, .. } = expression else {
        panic!("resource body must await the resource");
    };
    let super::CheckedClientExpression::Resource { operation } = expression.as_ref() else {
        panic!("await operand must be a resource");
    };
    assert_eq!(
        operation.target(),
        super::CheckedFunctionId::Existing(STD_INVOKE_ECHO_FUNCTION_ID)
    );
    assert_eq!(operation.arguments().len(), 1);
    assert_eq!(
        operation.arguments()[0].0,
        super::CheckedParameterId::Existing(STD_INVOKE_ECHO_PARAMETER_ID)
    );

    let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();
    let client = prepared
        .candidate()
        .functions()
        .iter()
        .find(|function| function.name().to_string() == "scalar_fixture.call")
        .unwrap();
    let revision = prepared
        .current_function_revisions()
        .unwrap()
        .iter()
        .find(|revision| revision.function() == client.id())
        .unwrap();
    let plan =
        orna_artifact::client_plan::ResourceClientPlan::decode(revision.artifact().payload())
            .unwrap();
    let orna_artifact::client_plan::ClientExpressionNode::Await { expression } = plan.expression()
    else {
        panic!("prepared resource plan must await the resource");
    };
    let orna_artifact::client_plan::ClientExpressionNode::Resource { operation } =
        expression.as_ref()
    else {
        panic!("prepared resource plan must contain a resource operation");
    };
    assert_eq!(operation.target(), STD_INVOKE_ECHO_FUNCTION_ID);
    assert_eq!(operation.target_revision(), prepared.candidate_pair());
    assert_eq!(operation.arguments().len(), 1);
    assert_eq!(operation.arguments()[0].0, STD_INVOKE_ECHO_PARAMETER_ID);
    assert_eq!(
        operation.arguments()[0].1,
        orna_artifact::client_plan::ClientExpressionNode::Integer { value: 43 }
    );
}

#[test]
fn accepts_scalar_resource_with_named_call_arguments() {
    let base = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes([0x41; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x42; 16]),
            QualifiedSemanticName::new(["tasks"]).unwrap(),
        )],
        Vec::new(),
        vec![FunctionDefinition::new(
            FunctionId::from_bytes([0x43; 16]),
            QualifiedSemanticName::new(["tasks", "find"]).unwrap(),
            FunctionDomain::Server,
            vec![parameter(
                0x44,
                "p_name",
                0,
                ResolvedType::Scalar(StandardScalar::CharacterLargeObject),
            )],
            FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::CharacterLargeObject)),
            FunctionRevisionId::from_bytes([0x45; 16]),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        )],
    )
    .unwrap();
    let source = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.find(p_name TEXT) RETURNS TEXT IS BEGIN RETURN AWAIT std.data.resource(target => tasks.find, arguments => std.call.args(p_name => p_name)); END;";
    let report = check(&bundle([("resource.orna", source)]), &base);
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let checked = report.checked_bundle().expect("resource source checks");
    let function = checked
        .client_functions()
        .iter()
        .find(|function| function.name().to_string() == "ui.find")
        .expect("checked CLIENT resource function");
    let CheckedClientFunctionBody::Expression { expression } = function.body() else {
        panic!("resource body must be an expression");
    };
    let super::CheckedClientExpression::Await {
        expression,
        location: await_location,
    } = expression
    else {
        panic!("resource body must await the resource");
    };
    let await_text = "AWAIT std.data.resource(target => tasks.find, arguments => std.call.args(p_name => p_name))";
    let await_start = source
        .find(await_text)
        .expect("await expression is present");
    assert_eq!(await_location.logical_path(), "resource.orna");
    assert_eq!(await_location.span().start(), await_start);
    assert_eq!(await_location.span().end(), await_start + await_text.len());
    let super::CheckedClientExpression::Resource { operation } = expression.as_ref() else {
        panic!("await operand must be a resource");
    };
    assert_eq!(
        operation.kind(),
        orna_artifact::client_plan::ResourceKind::Scalar
    );
    assert_eq!(operation.arguments().len(), 1);
    assert_eq!(
        operation.target(),
        super::CheckedFunctionId::Existing(FunctionId::from_bytes([0x43; 16]))
    );
    let resource_text =
        "std.data.resource(target => tasks.find, arguments => std.call.args(p_name => p_name))";
    let resource_start = source
        .find(resource_text)
        .expect("resource constructor is present");
    assert_eq!(operation.location().logical_path(), "resource.orna");
    assert_eq!(operation.location().span().start(), resource_start);
    assert_eq!(
        operation.location().span().end(),
        resource_start + resource_text.len()
    );
    let argument_location = match &operation.arguments()[0].1 {
        super::CheckedClientExpression::ParameterRead { location, .. } => location,
        _ => panic!("resource argument must retain its parameter-read span"),
    };
    let argument_start = source
        .rfind("p_name")
        .expect("argument parameter read is present");
    assert_eq!(argument_location.logical_path(), "resource.orna");
    assert_eq!(argument_location.span().start(), argument_start);
    assert_eq!(
        argument_location.span().end(),
        argument_start + "p_name".len()
    );
}

#[test]
fn rejects_inline_row_resource_descriptors_in_both_procedural_local_paths() {
    let base = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes([0x41; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x42; 16]),
            QualifiedSemanticName::new(["tasks"]).unwrap(),
        )],
        Vec::new(),
        vec![FunctionDefinition::new(
            FunctionId::from_bytes([0x43; 16]),
            QualifiedSemanticName::new(["tasks", "find"]).unwrap(),
            FunctionDomain::Server,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::CharacterLargeObject)),
            FunctionRevisionId::from_bytes([0x44; 16]),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        )],
    )
    .unwrap();

    for descriptor in [
        "TABLE (task_id UUID, title TEXT)",
        "RECORD (task_id UUID, title TEXT)",
    ] {
        for resource_type in ["Resource", "StreamResource"] {
            for (path, local) in [
                (
                    "state-less",
                    format!(
                        "LET rows std.data.{resource_type}<{descriptor}> := std.data.resource(target => tasks.find, arguments => std.call.args()); BEGIN RETURN AWAIT rows; END;"
                    ),
                ),
                (
                    "BEGIN LET",
                    format!(
                        "BEGIN LET rows std.data.{resource_type}<{descriptor}> := std.data.resource(target => tasks.find, arguments => std.call.args()); RETURN AWAIT rows; END;"
                    ),
                ),
            ] {
                let source = format!(
                    "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.find() RETURNS TEXT IS {local}"
                );
                let source_bundle =
                    SourceBundle::new([SourceUnit::new("resource.orna", source)]).unwrap();
                let report = check(&source_bundle, &base);
                assert_eq!(report.diagnostics().len(), 1, "{path} {descriptor}");
                assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);
                assert_eq!(
                    report.diagnostics()[0].message(),
                    "CLIENT local rows uses an inline TABLE/RECORD resource descriptor; row-resource transport is deferred"
                );
                assert_no_checked_bundle(&report);
            }
        }
    }
}

#[test]
fn rejects_client_resource_table_descriptor_with_deferred_row_diagnostic() {
    let source = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.find() RETURNS TEXT IS BEGIN LET rows std.data.Resource<TABLE (task_id UUID, title TEXT)> := std.data.resource(target => tasks.find, arguments => std.call.args()); RETURN AWAIT rows; END;";
    let parsed = parse(source);
    assert!(
        parsed.diagnostics().is_empty(),
        "{source}: {:?}",
        parsed.diagnostics()
    );

    let base = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes([0x41; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x42; 16]),
            QualifiedSemanticName::new(["tasks"]).unwrap(),
        )],
        Vec::new(),
        vec![FunctionDefinition::new(
            FunctionId::from_bytes([0x43; 16]),
            QualifiedSemanticName::new(["tasks", "find"]).unwrap(),
            FunctionDomain::Server,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::CharacterLargeObject)),
            FunctionRevisionId::from_bytes([0x44; 16]),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        )],
    )
    .unwrap();
    let report = check(&bundle([("resource-table.orna", source)]), &base);
    assert_eq!(report.diagnostics().len(), 1, "{:?}", report.diagnostics());
    assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);
    assert_eq!(
        report.diagnostics()[0].message(),
        "CLIENT local rows uses an inline TABLE/RECORD resource descriptor; row-resource transport is deferred"
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn rejects_client_stream_resource_record_descriptor_with_deferred_row_diagnostic() {
    let source = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); CREATE SERVER FUNCTION tasks.events() RETURNS STREAM<TEXT> AS SELECT t.title FROM tasks.task t; CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.events() RETURNS STREAM<TEXT> IS BEGIN LET rows std.data.StreamResource<RECORD (task_id UUID, title TEXT)> := std.data.stream_resource(target => tasks.events, arguments => std.call.args()); RETURN AWAIT rows; END;";
    let parsed = parse(source);
    assert!(
        parsed.diagnostics().is_empty(),
        "{source}: {:?}",
        parsed.diagnostics()
    );

    let report = check(
        &bundle([("stream-resource-record.orna", source)]),
        &empty_catalogue(),
    );
    assert_eq!(report.diagnostics().len(), 1, "{:?}", report.diagnostics());
    assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);
    assert_eq!(
        report.diagnostics()[0].message(),
        "CLIENT local rows uses an inline TABLE/RECORD resource descriptor; row-resource transport is deferred"
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn rejects_malformed_client_resource_local_descriptors() {
    let base = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes([0x41; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x42; 16]),
            QualifiedSemanticName::new(["tasks"]).unwrap(),
        )],
        Vec::new(),
        vec![FunctionDefinition::new(
            FunctionId::from_bytes([0x43; 16]),
            QualifiedSemanticName::new(["tasks", "find"]).unwrap(),
            FunctionDomain::Server,
            vec![parameter(
                0x44,
                "p_name",
                0,
                ResolvedType::Scalar(StandardScalar::CharacterLargeObject),
            )],
            FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::CharacterLargeObject)),
            FunctionRevisionId::from_bytes([0x45; 16]),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        )],
    )
    .unwrap();
    for (descriptor, source) in [
        (
            "",
            "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.find(p_name TEXT) RETURNS TEXT IS \
                 LET rows std.data.Resource<> := std.data.resource(target => tasks.find, arguments => std.call.args(p_name => p_name)); \
                 BEGIN RETURN AWAIT rows; END;",
        ),
        (
            "not-a-type",
            "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.find(p_name TEXT) RETURNS TEXT IS \
                 LET rows std.data.Resource<not-a-type> := std.data.resource(target => tasks.find, arguments => std.call.args(p_name => p_name)); \
                 BEGIN RETURN AWAIT rows; END;",
        ),
    ] {
        let report = check(&bundle([("resource.orna", source)]), &base);
        assert_eq!(report.diagnostics().len(), 1, "{descriptor:?}");
        assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);
        assert_eq!(
            report.diagnostics()[0].message(),
            "CLIENT local rows must declare std.data.Resource<T> or std.data.StreamResource<T>"
        );
        assert_no_checked_bundle(&report);
    }
}

#[test]
fn rejects_client_resource_descriptor_beyond_type_depth() {
    let text = format!(
        "std.data.Resource<TEXT{}>",
        "?".repeat(ClientResourceTypeParser::MAX_TYPE_DEPTH + 1),
    );
    let source = SourceSlice {
        span: SourceSpan {
            start: 0,
            end: text.len(),
        },
        text,
    };

    assert!(super::client_local_resource_type(&source).is_none());
}

#[test]
fn accepts_inline_table_resource_descriptor_shape() {
    let text = "std.data.Resource<TABLE (task_id UUID, title TEXT)>";
    let source = orna_syntax::SourceSlice {
        text: text.to_owned(),
        span: SourceSpan {
            start: 0,
            end: text.len(),
        },
    };
    let Some((kind, descriptor)) = super::client_local_resource_type(&source) else {
        panic!("inline table resource descriptor should parse");
    };
    assert_eq!(kind, orna_artifact::client_plan::ResourceKind::Scalar);
    assert!(descriptor.is_none());
}

#[test]
fn rejects_await_nested_in_non_suspending_expression() {
    let base = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes([0x41; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x42; 16]),
            QualifiedSemanticName::new(["tasks"]).unwrap(),
        )],
        Vec::new(),
        vec![FunctionDefinition::new(
            FunctionId::from_bytes([0x43; 16]),
            QualifiedSemanticName::new(["tasks", "find"]).unwrap(),
            FunctionDomain::Server,
            vec![parameter(
                0x44,
                "p_name",
                0,
                ResolvedType::Scalar(StandardScalar::CharacterLargeObject),
            )],
            FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::CharacterLargeObject)),
            FunctionRevisionId::from_bytes([0x45; 16]),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        )],
    )
    .unwrap();
    let source = "CREATE SCHEMA ui; \
            CREATE CLIENT FUNCTION ui.wrap(p_value TEXT) RETURNS TEXT AS p_value; \
            CREATE CLIENT FUNCTION ui.find(p_name TEXT) RETURNS TEXT AS \
            ui.wrap(AWAIT std.data.resource(target => tasks.find, arguments => std.call.args(p_name => p_name))) || 'x';";
    let report = check(&bundle([("resource.orna", source)]), &base);
    assert_eq!(report.diagnostics().len(), 1, "{:?}", report.diagnostics());
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(diagnostic.code(), DiagnosticCode::UnexpectedToken);
    assert_eq!(diagnostic.location().logical_path(), "resource.orna");
    let await_start = source.find("AWAIT").expect("await expression is present");
    assert_eq!(diagnostic.location().span().start(), await_start);
    assert_eq!(
        diagnostic.location().span().end(),
        await_start + "AWAIT".len()
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn rejects_wrong_v4_ui_catalogue_definition_closed() {
    let (types_unit, invoke_unit, output_unit, _ui_unit) = standard_v4_units();
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let rejects_catalogue = |catalogue: CatalogueSnapshot, label: &str| {
        let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
        origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
        origins.extend(standard_v3_output_origins(
            &catalogue,
            STANDARD_V3_OUTPUT_SOURCE,
        ));
        origins.extend(standard_v4_ui_origins(&catalogue, STANDARD_V4_UI_SOURCE));
        let executable = standard_v2_executable(&catalogue, &origins);
        let error = check_v4_parts(
            vec![
                types_unit.clone(),
                invoke_unit.clone(),
                output_unit.clone(),
                stored_v2_unit(
                    STD_UI_SOURCE_UNIT_ID,
                    3,
                    "std/ui.orna",
                    STANDARD_V4_UI_SOURCE,
                ),
            ],
            &catalogue,
            &origins,
            &[executable],
        )
        .unwrap_err();
        assert!(
            matches!(
                error,
                StandardLibraryCheckError::SourceMismatch
                    | StandardLibraryCheckError::MissingSchema
            ),
            "{label}: unexpected rejection: {error}"
        );
    };

    // The opaque ui value type at the fixed identity with a wrong kernel
    // contract is rejected by the reconcile.
    let ui_index = standard_v4_catalogue(true)
        .value_types()
        .iter()
        .position(|value_type| value_type.id() == STD_UI_TYPE_ID)
        .unwrap();
    rejects_catalogue(
        standard_v4_catalogue_with_ui_value_type(
            ui_index,
            ValueTypeDefinition::opaque(
                STD_UI_TYPE_ID,
                QualifiedSemanticName::new(["std", "ui", "ui"]).unwrap(),
                "orna.std.value.window@1",
            ),
        ),
        "wrong ui contract",
    );
    // The ui value type defined as a persistable primitive at the fixed
    // identity, not the opaque IMMUTABLE TRANSIENT ui contract.
    rejects_catalogue(
        standard_v4_catalogue_with_ui_value_type(
            ui_index,
            ValueTypeDefinition::primitive(
                STD_UI_TYPE_ID,
                QualifiedSemanticName::new(["std", "ui", "ui"]).unwrap(),
                ValueTypeMutability::Immutable,
                ValueTypePersistence::Persistable,
                STD_UI_CONTRACT,
            ),
        ),
        "wrong ui mutability and persistence",
    );
}

#[test]
fn accepts_resource_constructor_arguments_in_reverse_named_order_and_derives_result_type() {
    let integer = ResolvedType::Scalar(StandardScalar::Integer);
    let server_target_id = FunctionId::from_bytes([0x81; 16]);
    let base = catalogue(
        vec![schema(0x82, &["tasks"])],
        Vec::new(),
        vec![FunctionDefinition::new(
            server_target_id,
            QualifiedSemanticName::new(["tasks", "find"]).unwrap(),
            FunctionDomain::Server,
            Vec::new(),
            FunctionReturn::Single(integer),
            FunctionRevisionId::from_bytes([0x83; 16]),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        )],
    );
    let source = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.find() RETURNS INTEGER IS \
            BEGIN RETURN AWAIT std.data.resource(arguments => std.call.args(), target => tasks.find); END;";
    let report = check(&bundle([("resource-order.orna", source)]), &base);
    assert!(
        report.diagnostics().is_empty(),
        "{:#?}",
        report.diagnostics()
    );

    let function = &report.checked_bundle().unwrap().client_functions()[0];
    let CheckedClientFunctionBody::Expression { expression } = function.body() else {
        panic!("resource body must be a checked expression");
    };
    let CheckedClientExpression::Await { expression, .. } = expression else {
        panic!("resource body must await the constructor");
    };
    let CheckedClientExpression::Resource { operation } = expression.as_ref() else {
        panic!("await operand must be a resource constructor");
    };
    assert_eq!(
        operation.kind(),
        orna_artifact::client_plan::ResourceKind::Scalar
    );
    assert_eq!(
        operation.target(),
        super::CheckedFunctionId::Existing(server_target_id)
    );
    assert_eq!(
        operation.result_type(),
        SemanticType::Scalar(StandardScalar::Integer)
    );
    assert_eq!(
        function.return_type(),
        SemanticType::Scalar(StandardScalar::Integer)
    );
}

#[test]
fn accepts_resource_constructor_positional_arguments_before_canonical_id_sorting() {
    let verified_standard = verified_standard_v2_snapshot();
    let integer = ResolvedType::Value(STD_INTEGER_TYPE_ID);
    let text = integer;
    let server_target_id = FunctionId::from_bytes([0x91; 16]);
    let number_parameter_id = ParameterId::from_bytes([0x93; 16]);
    let text_parameter_id = ParameterId::from_bytes([0x92; 16]);
    let base = catalogue(
        vec![schema(0x94, &["tasks"])],
        Vec::new(),
        vec![FunctionDefinition::new(
            server_target_id,
            QualifiedSemanticName::new(["tasks", "find"]).unwrap(),
            FunctionDomain::Server,
            vec![
                parameter(0x93, "p_number", 0, integer),
                parameter(0x92, "p_text", 1, text),
            ],
            FunctionReturn::Single(integer),
            FunctionRevisionId::from_bytes([0x95; 16]),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        )],
    );
    let unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes([0x96; 16]),
        0,
        "application.orna",
        "",
        source_unit_content_digest("").unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
    let source = StoredSourceRevision::new(
        SourceBundleId::from_bytes([0x97; 16]),
        SourceRevisionId::from_bytes([0x98; 16]),
        None,
        vec![unit],
        bundle_hash,
        source_revision_record_digest(SourceBundleId::from_bytes([0x97; 16]), None, bundle_hash)
            .unwrap(),
    )
    .unwrap();
    let origin = |identity| {
        DefinitionOrigin::new(
            identity,
            SourceOrigin::new(SourceUnitId::from_bytes([0x96; 16]), 0, 0).unwrap(),
        )
    };
    let origins = vec![
        origin(DefinitionIdentity::Schema(SchemaId::from_bytes([0x94; 16]))),
        origin(DefinitionIdentity::Function(server_target_id)),
        origin(DefinitionIdentity::Parameter {
            owner: server_target_id,
            parameter: number_parameter_id,
        }),
        origin(DefinitionIdentity::Parameter {
            owner: server_target_id,
            parameter: text_parameter_id,
        }),
    ];
    let target_function = base
        .function_by_id(server_target_id)
        .expect("resource target is in the fixture catalogue");
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        "orna.test.server",
        1,
        vec![0],
        artifact_payload_digest(&[0]).unwrap(),
    )
    .unwrap();
    let semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version1,
        target_function,
        "orna.language/1",
        &artifact,
        &[],
        &[],
    )
    .unwrap();
    let function_revision = FunctionRevisionRecord::new(
        server_target_id,
        FunctionRevisionId::from_bytes([0x95; 16]),
        1,
        SourceOrigin::new(SourceUnitId::from_bytes([0x96; 16]), 0, 0).unwrap(),
        function_declaration_digest(b"").unwrap(),
        semantic_hash,
        "orna.language/1",
        artifact,
    )
    .unwrap()
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version1);
    let hash_context = CatalogueHashContext::version_two(verified_standard.clone());
    let catalogue_hash = catalogue_digest_with_context(
        &hash_context,
        &base,
        std::slice::from_ref(&function_revision),
        &[],
        &origins,
        &[],
    )
    .unwrap();
    let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            RevisionPair::new(source.id(), base.revision()),
            source,
            base,
            catalogue_hash,
            ActiveRevisionContent::new(Vec::new(), vec![function_revision], origins, Vec::new()),
        ),
        hash_context,
    )
    .unwrap();
    let standard = check_standard_library_source(&verified_standard).unwrap();
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let source = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.find(p_number INTEGER, p_text INTEGER) RETURNS INTEGER IS BEGIN RETURN AWAIT std.data.resource(target => tasks.find, arguments => std.call.args(p_number, p_text => p_text)); END;";
    let report =
        check_standard_application(&bundle([("resource-positional.orna", source)]), &context);
    assert!(
        report.diagnostics().is_empty(),
        "{:#?}",
        report.diagnostics()
    );

    let checked = report.preparation_view().unwrap().checked();
    let function = checked
        .client_functions()
        .iter()
        .find(|function| function.name().to_string() == "ui.find")
        .unwrap();
    let CheckedClientFunctionBody::Expression { expression } = function.body() else {
        panic!("resource body must be an expression");
    };
    let CheckedClientExpression::Await { expression, .. } = expression else {
        panic!("resource body must await the resource");
    };
    let CheckedClientExpression::Resource { operation } = expression.as_ref() else {
        panic!("await operand must be a resource");
    };
    assert_eq!(
        operation.target(),
        super::CheckedFunctionId::Existing(server_target_id)
    );
    assert_eq!(operation.arguments().len(), 2);
    assert_eq!(
        operation.arguments()[0].0,
        super::CheckedParameterId::Existing(text_parameter_id)
    );
    assert!(matches!(
        &operation.arguments()[0].1,
        super::CheckedClientExpression::ParameterRead { parameter, .. }
            if *parameter == function.parameters()[1].id()
    ));
    assert_eq!(
        operation.arguments()[1].0,
        super::CheckedParameterId::Existing(number_parameter_id)
    );
    assert!(matches!(
        &operation.arguments()[1].1,
        super::CheckedClientExpression::ParameterRead { parameter, .. }
            if *parameter == function.parameters()[0].id()
    ));
    assert_eq!(
        operation.result_type(),
        SemanticType::Scalar(StandardScalar::Integer)
    );
    assert_eq!(operation.standard_result_type(), Some(STD_INTEGER_TYPE_ID));

    let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();
    let client = prepared
        .candidate()
        .functions()
        .iter()
        .find(|function| function.name().to_string() == "ui.find")
        .unwrap();
    let revision = prepared
        .current_function_revisions()
        .unwrap()
        .iter()
        .find(|revision| revision.function() == client.id())
        .unwrap();
    let plan =
        orna_artifact::client_plan::ResourceClientPlan::decode(revision.artifact().payload())
            .unwrap();
    let orna_artifact::client_plan::ClientExpressionNode::Await { expression } = plan.expression()
    else {
        panic!("prepared resource plan must await the resource");
    };
    let orna_artifact::client_plan::ClientExpressionNode::Resource { operation } =
        expression.as_ref()
    else {
        panic!("prepared resource plan must contain a resource operation");
    };
    assert_eq!(
        operation.kind(),
        orna_artifact::client_plan::ResourceKind::Scalar
    );
    assert_eq!(operation.target(), server_target_id);
    assert_eq!(operation.target_revision(), prepared.candidate_pair());
    assert_eq!(operation.result_type(), STD_INTEGER_TYPE_ID);
    let caller_parameter = |name: &str| {
        client
            .parameters()
            .iter()
            .find(|parameter| parameter.name() == name)
            .unwrap()
            .id()
    };
    assert_eq!(
        operation.arguments(),
        &[
            (
                text_parameter_id,
                orna_artifact::client_plan::ClientExpressionNode::ParameterRead {
                    parameter: caller_parameter("p_text"),
                },
            ),
            (
                number_parameter_id,
                orna_artifact::client_plan::ClientExpressionNode::ParameterRead {
                    parameter: caller_parameter("p_number"),
                },
            ),
        ],
    );
}

#[test]
fn rejects_resource_constructor_duplicate_missing_and_client_targets() {
    let integer = ResolvedType::Scalar(StandardScalar::Integer);
    let server_target_id = FunctionId::from_bytes([0x84; 16]);
    let client_target_id = FunctionId::from_bytes([0x85; 16]);
    let base = catalogue(
        vec![schema(0x86, &["tasks"])],
        Vec::new(),
        vec![
            FunctionDefinition::new(
                server_target_id,
                QualifiedSemanticName::new(["tasks", "find"]).unwrap(),
                FunctionDomain::Server,
                vec![parameter(0x89, "p_value", 0, integer)],
                FunctionReturn::Single(integer),
                FunctionRevisionId::from_bytes([0x87; 16]),
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::ReadOnly),
                FunctionVolatility::Stable,
            ),
            FunctionDefinition::new(
                client_target_id,
                QualifiedSemanticName::new(["tasks", "client_find"]).unwrap(),
                FunctionDomain::Client,
                Vec::new(),
                FunctionReturn::Single(integer),
                FunctionRevisionId::from_bytes([0x88; 16]),
                FunctionSecurity::Invoker,
                None,
                FunctionVolatility::Immutable,
            ),
        ],
    );
    let cases = [
        (
            "duplicate constructor argument",
            "std.data.resource(target => tasks.find, target => tasks.find)",
            DiagnosticCode::DuplicateDefinition,
            "duplicate resource constructor argument",
        ),
        (
            "missing constructor argument",
            "std.data.resource(target => tasks.find)",
            DiagnosticCode::TypeMismatch,
            "resource constructor requires exactly one target and one arguments value",
        ),
        (
            "single abbreviated positional constructor argument",
            "std.data.resource(tasks.find)",
            DiagnosticCode::TypeMismatch,
            "resource constructor requires exactly one target and one arguments value",
        ),
        (
            "positional constructor arguments",
            "std.data.resource(tasks.find, std.call.args())",
            DiagnosticCode::TypeMismatch,
            "resource constructor arguments must be named target and arguments",
        ),
        (
            "mixed positional and named constructor arguments",
            "std.data.resource(tasks.find, arguments => std.call.args())",
            DiagnosticCode::TypeMismatch,
            "resource constructor arguments must be named target and arguments",
        ),
        (
            "CLIENT resource target",
            "std.data.resource(target => tasks.client_find, arguments => std.call.args())",
            DiagnosticCode::DomainIncompatible,
            "resource target tasks.client_find must be a SERVER function",
        ),
        (
            "unknown resource argument name",
            "std.data.resource(target => tasks.find, arguments => std.call.args(p_unknown => 7))",
            DiagnosticCode::UnknownQualifiedName,
            "unknown SERVER resource parameter p_unknown",
        ),
        (
            "trailing positional resource argument",
            "std.data.resource(target => tasks.find, arguments => std.call.args(7, 8))",
            DiagnosticCode::TypeMismatch,
            "too many arguments for SERVER resource target tasks.find",
        ),
        (
            "mistyped resource argument value",
            "std.data.resource(target => tasks.find, arguments => std.call.args(p_value => TRUE))",
            DiagnosticCode::TypeMismatch,
            "resource argument does not match SERVER parameter p_value",
        ),
    ];

    for (label, constructor, code, message) in cases {
        let source = format!(
            "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.find() RETURNS INTEGER IS BEGIN RETURN AWAIT {constructor}; END;"
        );
        let report = check(
            &SourceBundle::new([SourceUnit::new("resource-rejections.orna", source)]).unwrap(),
            &base,
        );
        assert_eq!(
            report.diagnostics().len(),
            1,
            "{label}: {:?}",
            report.diagnostics()
        );
        assert_eq!(report.diagnostics()[0].code(), code, "{label}");
        assert_eq!(report.diagnostics()[0].message(), message, "{label}");
        assert_no_checked_bundle(&report);
    }
}
