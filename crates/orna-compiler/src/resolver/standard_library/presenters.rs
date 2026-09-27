use super::*;

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
