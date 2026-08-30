use super::*;

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

pub(super) fn standard_parameter_echo_origins(source: &str) -> Vec<DefinitionOrigin> {
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

pub(super) fn check_echo(
    source: &str,
) -> Result<CheckedStandardParameterEcho, StandardLibraryCheckError> {
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
        super::super::CheckedServerFunctionReturn::Single {
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
