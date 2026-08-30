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

#[derive(Clone, Copy)]
pub(super) struct StandardUiConstructorSpec {
    pub(super) function_id: FunctionId,
    revision_id: FunctionRevisionId,
    runtime_contract: &'static str,
    parameter_ids: &'static [ParameterId],
    parameter_names: &'static [&'static str],
    parameter_types: &'static [TypeId],
}

pub(super) fn standard_ui_constructor_spec(
    name: &QualifiedSemanticName,
) -> Option<StandardUiConstructorSpec> {
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
