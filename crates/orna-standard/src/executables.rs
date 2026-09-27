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
