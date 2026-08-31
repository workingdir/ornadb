#[cfg(feature = "test-hooks")]
use super::*;

// The version-2 standard source constants below are the exact retained
// `std/types.orna` and `std/invoke.orna` shapes from the compiler reconcile
// fixtures. The fixture uses the same fixed identities, source, catalogue,
// executable, and origins, so its canonical digest is the compiled
// STANDARD_V2_CANONICAL_DIGEST golden.
#[cfg(feature = "test-hooks")]
const STD_INVOKE_SOURCE: &str = "CREATE SCHEMA std.invoke;\n\
    CREATE SERVER FUNCTION std.invoke.echo(\n\
    \x20   p_value INTEGER\n\
    )\n\
    RETURNS INTEGER\n\
    SECURITY INVOKER\n\
    TRANSACTION READ ONLY\n\
    VOLATILITY STABLE\n\
    AS\n\
    \x20   SELECT p_value;";

#[cfg(feature = "test-hooks")]
const STANDARD_V2_TYPES_SOURCE: &str = "CREATE SCHEMA std;CREATE SCHEMA std.types;\
    CREATE TYPE std.types.INTEGER AS VALUE PRIMITIVE KERNEL CONTRACT \
    'orna.kernel.value.integer@1' IMMUTABLE PERSISTABLE;\
    EXPORT TYPE std.types.INTEGER AS std.INTEGER;\
    EXPORT TYPE std.INTEGER TO PRELUDE AS INTEGER;";

#[cfg(feature = "test-hooks")]
const STANDARD_V2_CANONICAL_DIGEST: [u8; 32] = [
    115, 202, 159, 209, 255, 174, 218, 69, 195, 114, 168, 108, 210, 7, 50, 127, 176, 149, 134, 145,
    229, 113, 139, 179, 237, 228, 75, 75, 94, 20, 52, 52,
];

/// The complete V2 standard fixture with the exact compiler-reconcile
/// identities and the compiled canonical digest golden. Its source revision
/// parent must exist as a durable source revision before the upgrade applies.
#[cfg(feature = "test-hooks")]
fn verified_standard_v2_fixture() -> TestResult<VerifiedStandardLibrarySnapshot> {
    let types_unit = StoredSourceUnit::new(
        STD_TYPES_SOURCE_UNIT_ID,
        0,
        "std/types.orna",
        STANDARD_V2_TYPES_SOURCE,
        source_unit_content_digest(STANDARD_V2_TYPES_SOURCE)?,
    )?;
    let invoke_unit = StoredSourceUnit::new(
        STD_INVOKE_SOURCE_UNIT_ID,
        1,
        "std/invoke.orna",
        STD_INVOKE_SOURCE,
        source_unit_content_digest(STD_INVOKE_SOURCE)?,
    )?;
    let units = vec![types_unit, invoke_unit];
    let bundle = SourceBundleId::from_bytes([0x41; 16]);
    let bundle_hash = source_bundle_digest(&units)?;
    let source = StoredSourceRevision::new(
        bundle,
        SourceRevisionId::from_bytes([0x42; 16]),
        Some(SourceRevisionId::from_bytes([0x43; 16])),
        units,
        bundle_hash,
        source_revision_record_digest(
            bundle,
            Some(SourceRevisionId::from_bytes([0x43; 16])),
            bundle_hash,
        )?,
    )?;

    let integer = ValueTypeDefinition::primitive(
        STD_INTEGER_TYPE_ID,
        QualifiedSemanticName::new(["std", "types", "integer"])?,
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Persistable,
        "orna.kernel.value.integer@1",
    );
    let qualified = TypeBinding::qualified(
        QualifiedSemanticName::new(["std", "integer"])?,
        integer.id(),
    )?;
    let prelude = TypeBinding::prelude(PreludeTypeName::new(["integer"])?, integer.id())?;
    let echo = FunctionDefinition::new(
        STD_INVOKE_ECHO_FUNCTION_ID,
        QualifiedSemanticName::new(["std", "invoke", "echo"])?,
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
    let catalogue = CatalogueSnapshot::new_with_functions_and_types(
        CatalogueRevisionId::from_bytes([0x21; 16]),
        vec![
            SchemaDefinition::new(
                SchemaId::from_bytes([1; 16]),
                QualifiedSemanticName::new(["std"])?,
            ),
            SchemaDefinition::new(
                SchemaId::from_bytes([2; 16]),
                QualifiedSemanticName::new(["std", "types"])?,
            ),
            SchemaDefinition::new(
                STD_INVOKE_SCHEMA_ID,
                QualifiedSemanticName::new(["std", "invoke"])?,
            ),
        ],
        vec![],
        vec![integer],
        vec![qualified, prelude],
        vec![echo],
    )?;

    let origins = standard_v2_origins(&catalogue, STD_INVOKE_SOURCE)?;
    let executable = standard_v2_executable(&catalogue, &origins)?;

    let provisional = StandardLibrarySnapshot::new_with_executables(
        StandardLibraryRevisionId::from_bytes([0x44; 16]),
        StandardLibraryDigestVersion::Version2,
        source,
        "orna.language/1",
        catalogue,
        vec![executable],
        origins,
        Sha256Digest::from_bytes(STANDARD_V2_CANONICAL_DIGEST),
    )?;
    Ok(verify_standard_library_v2_snapshot(provisional)?)
}

/// Builds the exact origin sequence for both retained V2 source units. The
/// byte ranges match the parsed declaration spans of the compiler fixture.
#[cfg(feature = "test-hooks")]
fn standard_v2_origins(
    catalogue: &CatalogueSnapshot,
    invoke_source: &str,
) -> TestResult<Vec<DefinitionOrigin>> {
    let mut origins = Vec::new();
    let types = STANDARD_V2_TYPES_SOURCE;
    let schema_std_end = "CREATE SCHEMA std;".len();
    let schema_types_end = schema_std_end + "CREATE SCHEMA std.types;".len();
    let type_declaration = "CREATE TYPE std.types.INTEGER AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.integer@1' IMMUTABLE PERSISTABLE;";
    let type_start = types
        .find("CREATE TYPE")
        .ok_or_else(|| failure("missing type"))?;
    let type_end = type_start + type_declaration.len();
    let qualified_declaration = "EXPORT TYPE std.types.INTEGER AS std.INTEGER;";
    let qualified_start = types
        .find("EXPORT TYPE std.types.INTEGER AS std.INTEGER")
        .ok_or_else(|| failure("missing qualified binding"))?;
    let qualified_end = qualified_start + qualified_declaration.len();
    let prelude_declaration = "EXPORT TYPE std.INTEGER TO PRELUDE AS INTEGER;";
    let prelude_start = types
        .find("EXPORT TYPE std.INTEGER TO PRELUDE")
        .ok_or_else(|| failure("missing prelude binding"))?;
    let prelude_end = prelude_start + prelude_declaration.len();
    let types_unit = STD_TYPES_SOURCE_UNIT_ID;
    let qualified_binding = catalogue
        .type_bindings()
        .first()
        .ok_or_else(|| failure("missing qualified binding"))?;
    let prelude_binding = catalogue
        .type_bindings()
        .last()
        .ok_or_else(|| failure("missing prelude binding"))?;
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::Schema(SchemaId::from_bytes([1; 16])),
        SourceOrigin::new(types_unit, 0, u32::try_from(schema_std_end)?)?,
    ));
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::Schema(SchemaId::from_bytes([2; 16])),
        SourceOrigin::new(
            types_unit,
            u32::try_from(schema_std_end)?,
            u32::try_from(schema_types_end)?,
        )?,
    ));
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::ValueType(STD_INTEGER_TYPE_ID),
        SourceOrigin::new(
            types_unit,
            u32::try_from(type_start)?,
            u32::try_from(type_end)?,
        )?,
    ));
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::TypeBinding(qualified_binding.id()),
        SourceOrigin::new(
            types_unit,
            u32::try_from(qualified_start)?,
            u32::try_from(qualified_end)?,
        )?,
    ));
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::TypeBinding(prelude_binding.id()),
        SourceOrigin::new(
            types_unit,
            u32::try_from(prelude_start)?,
            u32::try_from(prelude_end)?,
        )?,
    ));

    let function_start = invoke_source
        .find("CREATE SERVER FUNCTION")
        .ok_or_else(|| failure("missing function declaration"))?;
    let function_end = invoke_source.len();
    let parameter_start = invoke_source
        .find("p_value")
        .ok_or_else(|| failure("missing parameter declaration"))?;
    let parameter_end = parameter_start + "p_value INTEGER".len();
    let schema_end = "CREATE SCHEMA std.invoke;".len();
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::Schema(STD_INVOKE_SCHEMA_ID),
        SourceOrigin::new(STD_INVOKE_SOURCE_UNIT_ID, 0, u32::try_from(schema_end)?)?,
    ));
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::Function(STD_INVOKE_ECHO_FUNCTION_ID),
        SourceOrigin::new(
            STD_INVOKE_SOURCE_UNIT_ID,
            u32::try_from(function_start)?,
            u32::try_from(function_end)?,
        )?,
    ));
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::Parameter {
            owner: STD_INVOKE_ECHO_FUNCTION_ID,
            parameter: STD_INVOKE_ECHO_PARAMETER_ID,
        },
        SourceOrigin::new(
            STD_INVOKE_SOURCE_UNIT_ID,
            u32::try_from(parameter_start)?,
            u32::try_from(parameter_end)?,
        )?,
    ));
    Ok(origins)
}

/// Builds the exact V2 executable: the immutable echo revision, the 44-byte
/// server parameter-echo artifact, and the three ordered references.
#[cfg(feature = "test-hooks")]
fn standard_v2_executable(
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
) -> TestResult<StandardExecutable> {
    let function = catalogue
        .function_by_id(STD_INVOKE_ECHO_FUNCTION_ID)
        .ok_or_else(|| failure("missing echo function"))?;
    let function_origin = origins
        .iter()
        .find(|origin| {
            origin.identity() == DefinitionIdentity::Function(STD_INVOKE_ECHO_FUNCTION_ID)
        })
        .ok_or_else(|| failure("missing echo function origin"))?
        .source();
    let declaration_content_hash = function_declaration_digest(
        &STD_INVOKE_SOURCE.as_bytes()
            [function_origin.byte_start() as usize..function_origin.byte_end() as usize],
    )?;
    let payload =
        ServerParameterEcho::new(STD_INVOKE_ECHO_PARAMETER_ID, STD_INTEGER_TYPE_ID)?.encode()?;
    let content_hash = artifact_payload_digest(&payload)?;
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        "orna.server-parameter-echo",
        1,
        payload,
        content_hash,
    )?;
    let parameter_integer_start = STD_INVOKE_SOURCE
        .find("INTEGER")
        .ok_or_else(|| failure("missing parameter type"))? as u32;
    let result_integer_start = STD_INVOKE_SOURCE
        .rfind("INTEGER")
        .ok_or_else(|| failure("missing result type"))? as u32;
    let body_p_value_start = STD_INVOKE_SOURCE
        .rfind("p_value")
        .ok_or_else(|| failure("missing body identifier"))? as u32;
    let integer_origin = |start: u32| -> TestResult<SourceOrigin> {
        Ok(SourceOrigin::new(
            STD_INVOKE_SOURCE_UNIT_ID,
            start,
            start + 7,
        )?)
    };
    let references = vec![
        DefinitionReference::new(
            STD_INVOKE_ECHO_FUNCTION_ID,
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            0,
            DefinitionReferenceTarget::ValueType(STD_INTEGER_TYPE_ID),
            DefinitionReferenceKind::NamedType,
            integer_origin(parameter_integer_start)?,
        ),
        DefinitionReference::new(
            STD_INVOKE_ECHO_FUNCTION_ID,
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            1,
            DefinitionReferenceTarget::ValueType(STD_INTEGER_TYPE_ID),
            DefinitionReferenceKind::NamedType,
            integer_origin(result_integer_start)?,
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
            integer_origin(body_p_value_start)?,
        ),
    ];
    let semantic = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        function,
        "orna.language/1",
        &artifact,
        &[],
        &references,
    )?;
    let revision = FunctionRevisionRecord::new(
        STD_INVOKE_ECHO_FUNCTION_ID,
        STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        STD_INVOKE_ECHO_REVISION_NUMBER,
        function_origin,
        declaration_content_hash,
        semantic,
        "orna.language/1",
        artifact,
    )?
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    Ok(StandardExecutable::new(
        STD_INVOKE_ECHO_FUNCTION_ID,
        revision,
        references,
    )?)
}

/// Installs the durable source-revision parent of the fixture V2 standard
/// source before the upgrade applies.
#[cfg(feature = "test-hooks")]
async fn install_standard_v2_parent_revision(database: &TestDatabase) -> TestResult<()> {
    let session = database.open().await?;
    let bundle = vec![0x99_u8; 16];
    let parent = vec![0x43_u8; 16];
    let content_hash = vec![0x98_u8; 32];
    session
        .client()
        .execute(
            "INSERT INTO _orna_kernel.source_bundles
                (id, content_hash, hash_algorithm, hash_contract_version)
             VALUES ($1, $2, 'sha256', 1)",
            &[&bundle, &content_hash],
        )
        .await?;
    session
        .client()
        .execute(
            "INSERT INTO _orna_kernel.source_revisions
                (id, parent_source_revision_id, bundle_id, content_hash,
                 hash_algorithm, hash_contract_version)
             VALUES ($1, NULL, $2, $3, 'sha256', 1)",
            &[&parent, &bundle, &content_hash],
        )
        .await?;
    session.shutdown().await
}

/// The companion application revision for the V2 standard upgrade: one
/// application CLIENT function under the pinned version-two context. Its
/// identity is distinct from every standard and system function, so the
/// upgrade scan admits it without a collision. The single upgrade installs
/// the application and standard authority rows under one catalogue revision.
#[cfg(feature = "test-hooks")]
fn v2_standard_and_application_candidate(
    active: &ActiveDatabaseRevision,
    standard: &VerifiedStandardLibrarySnapshot,
) -> TestResult<DeployableRevision> {
    let content = "CREATE SCHEMA app;\n";
    let unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes([0xa1; 16]),
        0,
        "main.orna",
        content,
        source_unit_content_digest(content)?,
    )?;
    let bundle = SourceBundleId::from_bytes([0xa2; 16]);
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit))?;
    let source = StoredSourceRevision::new(
        bundle,
        SourceRevisionId::from_bytes([0xa3; 16]),
        Some(active.pair().source()),
        vec![unit.clone()],
        bundle_hash,
        source_revision_record_digest(bundle, Some(active.pair().source()), bundle_hash)?,
    )?;
    let schema = SchemaDefinition::new(
        SchemaId::from_bytes([0xa4; 16]),
        QualifiedSemanticName::new(["app"])?,
    );
    let function = FunctionDefinition::new(
        FunctionId::from_bytes([0xa5; 16]),
        QualifiedSemanticName::new(["app", "answer"])?,
        FunctionDomain::Client,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::value(STD_INTEGER_TYPE_ID)),
        FunctionRevisionId::from_bytes([0xa6; 16]),
        FunctionSecurity::Invoker,
        None,
        FunctionVolatility::Immutable,
    );
    let catalogue = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes([0xa7; 16]),
        vec![schema.clone()],
        vec![],
        vec![function.clone()],
    )?;
    let origin = SourceOrigin::new(unit.id(), 0, u32::try_from(content.len())?)?;
    let origins = vec![
        DefinitionOrigin::new(DefinitionIdentity::Schema(schema.id()), origin),
        DefinitionOrigin::new(DefinitionIdentity::Function(function.id()), origin),
    ];
    let artifact = executable_artifact(
        ExecutableArtifactKind::Client,
        "orna.client-bytecode",
        b"ORNACB\0\0\0\0\0\x01answer".to_vec(),
    )?;
    let declaration_hash = function_declaration_digest(content.as_bytes())?;
    let semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        &function,
        "orna.language/1",
        &artifact,
        &[],
        &[],
    )?;
    let revision = FunctionRevisionRecord::new(
        function.id(),
        function.current_revision(),
        1,
        origin,
        declaration_hash,
        semantic_hash,
        "orna.language/1",
        artifact,
    )?
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    let context = CatalogueHashContext::version_two(standard.clone());
    let catalogue_hash = catalogue_digest_with_context(
        &context,
        &catalogue,
        std::slice::from_ref(&revision),
        &[],
        &origins,
        &[],
    )?;
    Ok(DeployableRevision::new_with_catalogue_hash_context(
        DeployableRevisionInput::new(
            active.pair(),
            source,
            active.pair().catalogue(),
            catalogue,
            catalogue_hash,
            DeployableRevisionContent::new(origins, vec![], vec![revision.clone()], vec![])
                .with_current_function_revisions(vec![revision]),
        ),
        context,
    )?)
}

/// The complete live fixture: the executable V2 standard and one application
/// CLIENT function installed atomically through the production apply path.
/// The active catalogue therefore owns one application target and one standard
/// target under one catalogue revision.
#[cfg(feature = "test-hooks")]
pub(super) struct V2Fixture {
    pub(super) standard: VerifiedStandardLibrarySnapshot,
    pub(super) active: ActiveDatabaseRevision,
    pub(super) app_function: FunctionId,
}

#[cfg(feature = "test-hooks")]
pub(super) async fn install_v2_standard_fixture(database: &TestDatabase) -> TestResult<V2Fixture> {
    let kernel = kernel(database)?;
    kernel.bootstrap().await?;
    install_standard_v2_parent_revision(database).await?;
    let active = kernel.recover().await?;
    let standard = verified_standard_v2_fixture()?;
    let candidate = v2_standard_and_application_candidate(&active, &standard)?;
    let applied = kernel
        .apply_test_standard_upgrade(&candidate, &standard)
        .await?;
    let app_function = applied.catalogue().functions()[0].id();
    require(
        applied
            .catalogue_hash_context()
            .standard()
            .is_some_and(|selected| selected.revision() == standard.revision()),
        "fixture active revision must pin the executable standard snapshot",
    )?;
    Ok(V2Fixture {
        standard,
        active: applied,
        app_function,
    })
}

/// Re-pins the active catalogue revision to the retained version-one standard
/// snapshot and rewrites its target-authority rows without the standard
/// executable, simulating a later standard upgrade that removed the granted
/// function. The application catalogue content is unchanged, so the re-pin is
/// a valid version-two catalogue whose union has no standard target.
#[cfg(feature = "test-hooks")]
pub(super) async fn install_later_standard_upgrade_without_echo(
    database: &TestDatabase,
    fixture: &V2Fixture,
) -> TestResult<()> {
    let retained = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot()?,
    )?;
    insert_standard_snapshot(database, &retained).await?;
    let session = database.open().await?;
    let operation_result: TestResult<()> = async {
        let active = fixture.active.clone();
        let catalogue_bytes = active.pair().catalogue().to_bytes().to_vec();
        let context = CatalogueHashContext::version_two(retained.clone());
        let catalogue_hash = catalogue_digest_with_context(
            &context,
            active.catalogue(),
            active.function_revisions(),
            active.expressions(),
            active.origins(),
            active.references(),
        )?;
        session
            .client()
            .batch_execute("BEGIN; SET CONSTRAINTS ALL DEFERRED")
            .await?;
        // Remove the standard authority row and re-pin the carried application
        // function's resolved standard value type before re-pinning the
        // catalogue revision so the non-deferrable foreign keys stay valid.
        session
            .client()
            .execute(
                "DELETE FROM _orna_kernel.invocation_target_authorities
                 WHERE catalogue_revision_id = $1 AND function_id = $2",
                &[
                    &catalogue_bytes,
                    &STD_INVOKE_ECHO_FUNCTION_ID.to_bytes().to_vec(),
                ],
            )
            .await?;
        session
            .client()
            .execute(
                "UPDATE _orna_kernel.catalogue_functions
                 SET return_standard_library_revision_id = $2
                 WHERE catalogue_revision_id = $1 AND function_id = $3",
                &[
                    &catalogue_bytes,
                    &retained.revision().to_bytes().to_vec(),
                    &fixture.app_function.to_bytes().to_vec(),
                ],
            )
            .await?;
        session
            .client()
            .execute(
                "UPDATE _orna_kernel.catalogue_revisions
                 SET standard_library_revision_id = $2, content_hash = $3
                 WHERE id = $1",
                &[
                    &catalogue_bytes,
                    &retained.revision().to_bytes().to_vec(),
                    &catalogue_hash.to_bytes().to_vec(),
                ],
            )
            .await?;
        session.client().batch_execute("COMMIT").await?;
        Ok(())
    }
    .await;
    finish_session(
        operation_result,
        session.shutdown().await,
        "later standard upgrade fixture",
    )
}
