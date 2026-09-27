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
