//! Retained standard executable reconstruction.

use super::*;
/// Builds the retained V2 `StandardExecutable` from the retained invoke unit.
///
/// The canonical compiler checker validates the exact closed
/// `std.invoke.echo` source shape and returns the 44-byte
/// `orna.server-parameter-echo` artifact and the three ordered references at
/// their exact token ranges. The declaration-content digest and the
/// version-2 semantic digest are computed by the canonical encoders from the
/// retained declaration bytes and the checked function, artifact, and
/// references.
pub(super) fn retained_v2_executable(
    invoke_source: &str,
    catalogue: &CatalogueSnapshot,
    invoke_origins: &[DefinitionOrigin],
) -> Result<StandardExecutable, StandardLibraryError> {
    let parsed = orna_syntax::parse(invoke_source);
    let declaration = parsed
        .server_functions()
        .first()
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let checked = orna_compiler::check_standard_parameter_echo(
        declaration,
        catalogue,
        invoke_origins,
        INTEGER_TYPE_ID,
    )
    .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
    if checked.artifact().content_hash() != ACCEPTED_V2_ARTIFACT_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }

    let function = catalogue
        .function_by_id(STD_INVOKE_ECHO_FUNCTION_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let function_origin = invoke_origins
        .iter()
        .find(|origin| {
            origin.identity() == DefinitionIdentity::Function(STD_INVOKE_ECHO_FUNCTION_ID)
        })
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?
        .source();
    let declaration_bytes = &invoke_source.as_bytes()
        [function_origin.byte_start() as usize..function_origin.byte_end() as usize];
    let declaration_content_hash = function_declaration_digest(declaration_bytes)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        function,
        LANGUAGE_VERSION_IDENTITY,
        checked.artifact(),
        &[],
        checked.references(),
    )
    .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if semantic_hash != ACCEPTED_V2_SEMANTIC_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let revision = FunctionRevisionRecord::new(
        checked.function_id(),
        checked.revision_id(),
        STD_INVOKE_ECHO_REVISION_NUMBER,
        function_origin,
        declaration_content_hash,
        semantic_hash,
        LANGUAGE_VERSION_IDENTITY,
        checked.artifact().clone(),
    )
    .map_err(|source| StandardLibraryError::Revision { source })?
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);

    StandardExecutable::new(
        checked.function_id(),
        revision,
        checked.references().to_vec(),
    )
    .map_err(|source| StandardLibraryError::Revision { source })
}

pub(super) fn retained_json_executable(
    json_source: &str,
    catalogue: &CatalogueSnapshot,
    json_origins: &[DefinitionOrigin],
) -> Result<StandardExecutable, StandardLibraryError> {
    let function = catalogue
        .function_by_id(STD_JSON_ENCODE_FUNCTION_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let function_origin = json_origins
        .iter()
        .find(|origin| {
            origin.identity() == DefinitionIdentity::Function(STD_JSON_ENCODE_FUNCTION_ID)
        })
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?
        .source();
    let declaration_bytes = &json_source.as_bytes()
        [function_origin.byte_start() as usize..function_origin.byte_end() as usize];
    let declaration_content_hash = function_declaration_digest(declaration_bytes)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let mut payload = Vec::with_capacity(44);
    payload.extend_from_slice(b"ORNAJE\0\0");
    payload.extend_from_slice(&1_u32.to_be_bytes());
    payload.extend_from_slice(&STD_JSON_ENCODE_PARAMETER_ID.to_bytes());
    payload.extend_from_slice(&STD_JSON_VALUE_TYPE_ID.to_bytes());
    let artifact_hash = artifact_payload_digest(&payload)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        "orna.server-json-encode",
        1,
        payload,
        artifact_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        function,
        LANGUAGE_VERSION_IDENTITY,
        &artifact,
        &[],
        &[],
    )
    .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let revision = FunctionRevisionRecord::new(
        STD_JSON_ENCODE_FUNCTION_ID,
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        1,
        function_origin,
        declaration_content_hash,
        semantic_hash,
        LANGUAGE_VERSION_IDENTITY,
        artifact,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    StandardExecutable::new(STD_JSON_ENCODE_FUNCTION_ID, revision, Vec::new())
        .map_err(|source| StandardLibraryError::Revision { source })
}
pub(super) fn retained_window_executable(
    window_source: &str,
    catalogue: &CatalogueSnapshot,
    window_origins: &[DefinitionOrigin],
) -> Result<StandardExecutable, StandardLibraryError> {
    let parsed = orna_syntax::parse(window_source);
    let declaration = parsed
        .client_functions()
        .first()
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let checked = orna_compiler::check_standard_ui_window(declaration, catalogue, window_origins)
        .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
    let function = catalogue
        .function_by_id(STD_UI_WINDOW_FUNCTION_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let function_origin = window_origins
        .iter()
        .find(|origin| origin.identity() == DefinitionIdentity::Function(STD_UI_WINDOW_FUNCTION_ID))
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?
        .source();
    let declaration_bytes = &window_source.as_bytes()
        [function_origin.byte_start() as usize..function_origin.byte_end() as usize];
    let declaration_content_hash = function_declaration_digest(declaration_bytes)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let source_origin = |span: &orna_syntax::SourceSpan| {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        let end =
            u32::try_from(span.end).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        SourceOrigin::new(STD_WINDOW_SOURCE_UNIT_ID, start, end)
            .map_err(|source| StandardLibraryError::Revision { source })
    };
    let [title, content] = declaration.parameters.as_slice() else {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    };
    let result = match &declaration.return_type {
        orna_syntax::FunctionReturnType::Single(result) => result,
        orna_syntax::FunctionReturnType::Rows { .. }
        | orna_syntax::FunctionReturnType::Stream { .. } => {
            return Err(StandardLibraryError::RetainedSourceMismatch);
        }
    };
    let references = vec![
        orna_core::revision::DefinitionReference::new(
            checked.function_id(),
            checked.revision_id(),
            0,
            orna_core::revision::DefinitionReferenceTarget::ValueType(
                CHARACTER_LARGE_OBJECT_TYPE_ID,
            ),
            orna_core::revision::DefinitionReferenceKind::NamedType,
            source_origin(title.type_specification.span())?,
        ),
        orna_core::revision::DefinitionReference::new(
            checked.function_id(),
            checked.revision_id(),
            1,
            orna_core::revision::DefinitionReferenceTarget::ValueType(STD_UI_TYPE_ID),
            orna_core::revision::DefinitionReferenceKind::NamedType,
            source_origin(content.type_specification.span())?,
        ),
        orna_core::revision::DefinitionReference::new(
            checked.function_id(),
            checked.revision_id(),
            2,
            orna_core::revision::DefinitionReferenceTarget::ValueType(STD_UI_TYPE_ID),
            orna_core::revision::DefinitionReferenceKind::NamedType,
            source_origin(result.span())?,
        ),
    ];
    let plan = ExpressionClientPlan::new(ClientExpressionNode::ExternalContract {
        identity: STD_UI_WINDOW_CONTRACT.to_owned(),
    });
    let payload = plan
        .encode()
        .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
    let artifact_hash = artifact_payload_digest(&payload)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if artifact_hash != ACCEPTED_V7_WINDOW_ARTIFACT_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        orna_artifact::client_plan::FORMAT_IDENTITY,
        plan.format_version(),
        payload,
        artifact_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        function,
        orna_artifact::client_plan::LANGUAGE_VERSION_IDENTITY,
        &artifact,
        &[],
        &references,
    )
    .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if semantic_hash != ACCEPTED_V7_WINDOW_SEMANTIC_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
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
    .map_err(|source| StandardLibraryError::Revision { source })?
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    StandardExecutable::new(checked.function_id(), revision, references)
        .map_err(|source| StandardLibraryError::Revision { source })
}

pub(super) fn retained_terminal_table_executable(
    data_source: &str,
    catalogue: &CatalogueSnapshot,
    data_origins: &[DefinitionOrigin],
) -> Result<StandardExecutable, StandardLibraryError> {
    let parsed = orna_syntax::parse(data_source);
    let declaration = parsed
        .server_functions()
        .first()
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let checked = orna_compiler::check_standard_terminal_present_table(
        declaration,
        catalogue,
        data_origins,
        STD_DATA_ROWS_TYPE_ID,
    )
    .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
    let function = catalogue
        .function_by_id(STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let function_origin = data_origins
        .iter()
        .find(|origin| {
            origin.identity()
                == DefinitionIdentity::Function(STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID)
        })
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?
        .source();
    let declaration_bytes = &data_source.as_bytes()
        [function_origin.byte_start() as usize..function_origin.byte_end() as usize];
    let declaration_content_hash = function_declaration_digest(declaration_bytes)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let payload = server_terminal_table::TerminalTablePlan::new(
        STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
        STD_DATA_ROWS_TYPE_ID,
    )
    .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?
    .encode()
    .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
    let artifact_hash = artifact_payload_digest(&payload)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        server_terminal_table::FORMAT_IDENTITY,
        server_terminal_table::FORMAT_VERSION,
        payload,
        artifact_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let source_origin = |span: &orna_syntax::SourceSpan| {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        let end =
            u32::try_from(span.end).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        SourceOrigin::new(STD_DATA_SOURCE_UNIT_ID, start, end)
            .map_err(|source| StandardLibraryError::Revision { source })
    };
    let parameter = declaration
        .parameters
        .first()
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let result = match &declaration.return_type {
        orna_syntax::FunctionReturnType::Single(result) => result,
        orna_syntax::FunctionReturnType::Rows { .. }
        | orna_syntax::FunctionReturnType::Stream { .. } => {
            return Err(StandardLibraryError::RetainedSourceMismatch);
        }
    };
    let body = declaration
        .body
        .as_no_input_parameter_select()
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let references = vec![
        orna_core::revision::DefinitionReference::new(
            checked.function_id(),
            checked.revision_id(),
            0,
            orna_core::revision::DefinitionReferenceTarget::ValueType(STD_DATA_ROWS_TYPE_ID),
            orna_core::revision::DefinitionReferenceKind::NamedType,
            source_origin(parameter.type_specification.span())?,
        ),
        orna_core::revision::DefinitionReference::new(
            checked.function_id(),
            checked.revision_id(),
            1,
            orna_core::revision::DefinitionReferenceTarget::ValueType(
                STD_TERMINAL_DOCUMENT_TYPE_ID,
            ),
            orna_core::revision::DefinitionReferenceKind::NamedType,
            source_origin(result.span())?,
        ),
        orna_core::revision::DefinitionReference::new(
            checked.function_id(),
            checked.revision_id(),
            2,
            orna_core::revision::DefinitionReferenceTarget::Parameter {
                owner: STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
                parameter: STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
            },
            orna_core::revision::DefinitionReferenceKind::ParameterRead,
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
    .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
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
    .map_err(|source| StandardLibraryError::Revision { source })?
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    StandardExecutable::new(checked.function_id(), revision, references)
        .map_err(|source| StandardLibraryError::Revision { source })
}

pub(super) fn retained_ui_constructor_executables(
    source: &str,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
) -> Result<Vec<StandardExecutable>, StandardLibraryError> {
    let parsed = orna_syntax::parse(source);
    if !parsed.diagnostics().is_empty()
        || parsed.syntax().text() != source
        || parsed.client_functions().len() != 7
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let expected_functions = [
        STD_UI_TEXT_FUNCTION_ID,
        STD_UI_BUTTON_FUNCTION_ID,
        STD_UI_PANEL_FUNCTION_ID,
        STD_UI_ROW_FUNCTION_ID,
        STD_UI_COLUMN_FUNCTION_ID,
        STD_UI_TEXT_INPUT_FUNCTION_ID,
        STD_UI_TABS_FUNCTION_ID,
    ];
    let mut executables = Vec::with_capacity(expected_functions.len());
    for (index, declaration) in parsed.client_functions().iter().enumerate() {
        let function_id = expected_functions[index];
        let declaration_origins = origins
            .iter()
            .filter(|origin| match origin.identity() {
                DefinitionIdentity::Function(function) => function == function_id,
                DefinitionIdentity::Parameter { owner, .. } => owner == function_id,
                _ => false,
            })
            .cloned()
            .collect::<Vec<_>>();
        let checked = orna_compiler::check_standard_ui_constructor(
            declaration,
            catalogue,
            &declaration_origins,
        )
        .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        executables.push(retained_ui_constructor_executable(
            source,
            catalogue,
            declaration,
            &declaration_origins,
            checked,
            index,
        )?);
    }
    Ok(executables)
}

fn retained_ui_constructor_executable(
    source: &str,
    catalogue: &CatalogueSnapshot,
    declaration: &orna_syntax::ClientFunctionDeclaration,
    origins: &[DefinitionOrigin],
    checked: CheckedStandardUiConstructor,
    index: usize,
) -> Result<StandardExecutable, StandardLibraryError> {
    let function_origin = origins
        .iter()
        .find(|origin| origin.identity() == DefinitionIdentity::Function(checked.function_id()))
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?
        .source();
    let source_origin = |span: &orna_syntax::SourceSpan| {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        let end =
            u32::try_from(span.end).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        SourceOrigin::new(STD_UI_CONSTRUCTORS_SOURCE_UNIT_ID, start, end)
            .map_err(|source| StandardLibraryError::Revision { source })
    };
    let function = catalogue
        .function_by_id(checked.function_id())
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let mut references = Vec::with_capacity(declaration.parameters.len() + 1);
    for (ordinal, parameter) in declaration.parameters.iter().enumerate() {
        let target = function
            .parameters()
            .get(ordinal)
            .and_then(|parameter| parameter.resolved_type().value_type())
            .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
        references.push(orna_core::revision::DefinitionReference::new(
            checked.function_id(),
            checked.revision_id(),
            ordinal as u32,
            orna_core::revision::DefinitionReferenceTarget::ValueType(target),
            orna_core::revision::DefinitionReferenceKind::NamedType,
            source_origin(parameter.type_specification.span())?,
        ));
    }
    let orna_syntax::FunctionReturnType::Single(result_type) = &declaration.return_type else {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    };
    references.push(orna_core::revision::DefinitionReference::new(
        checked.function_id(),
        checked.revision_id(),
        declaration.parameters.len() as u32,
        orna_core::revision::DefinitionReferenceTarget::ValueType(STD_UI_TYPE_ID),
        orna_core::revision::DefinitionReferenceKind::NamedType,
        source_origin(result_type.span())?,
    ));
    let plan = ExpressionClientPlan::new(ClientExpressionNode::ExternalContract {
        identity: checked.runtime_contract().to_owned(),
    });
    let payload = plan
        .encode()
        .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
    let artifact_hash = artifact_payload_digest(&payload)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let expected_artifact_hashes = [
        ACCEPTED_V9_UI_TEXT_ARTIFACT_DIGEST,
        ACCEPTED_V9_UI_BUTTON_ARTIFACT_DIGEST,
        ACCEPTED_V9_UI_PANEL_ARTIFACT_DIGEST,
        ACCEPTED_V9_UI_ROW_ARTIFACT_DIGEST,
        ACCEPTED_V9_UI_COLUMN_ARTIFACT_DIGEST,
        ACCEPTED_V9_UI_TEXT_INPUT_ARTIFACT_DIGEST,
        ACCEPTED_V9_UI_TABS_ARTIFACT_DIGEST,
    ];
    if artifact_hash != expected_artifact_hashes[index] {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        orna_artifact::client_plan::FORMAT_IDENTITY,
        plan.format_version(),
        payload,
        artifact_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        function,
        LANGUAGE_VERSION_IDENTITY,
        &artifact,
        &[],
        &references,
    )
    .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let expected_semantic_hashes = [
        ACCEPTED_V9_UI_TEXT_SEMANTIC_DIGEST,
        ACCEPTED_V9_UI_BUTTON_SEMANTIC_DIGEST,
        ACCEPTED_V9_UI_PANEL_SEMANTIC_DIGEST,
        ACCEPTED_V9_UI_ROW_SEMANTIC_DIGEST,
        ACCEPTED_V9_UI_COLUMN_SEMANTIC_DIGEST,
        ACCEPTED_V9_UI_TEXT_INPUT_SEMANTIC_DIGEST,
        ACCEPTED_V9_UI_TABS_SEMANTIC_DIGEST,
    ];
    if semantic_hash != expected_semantic_hashes[index] {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let declaration_bytes = &source.as_bytes()
        [function_origin.byte_start() as usize..function_origin.byte_end() as usize];
    let declaration_content_hash = function_declaration_digest(declaration_bytes)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let revision = FunctionRevisionRecord::new(
        checked.function_id(),
        checked.revision_id(),
        1,
        function_origin,
        declaration_content_hash,
        semantic_hash,
        LANGUAGE_VERSION_IDENTITY,
        artifact,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    StandardExecutable::new(checked.function_id(), revision, references)
        .map_err(|source| StandardLibraryError::Revision { source })
}
