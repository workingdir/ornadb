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
fn verified_standard_v2_fixture() -> TestResult<orna_core::revision::VerifiedStandardLibrarySnapshot>
{
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

/// The companion application revision for one V2 standard upgrade. It is a
/// complete new source and catalogue revision whose hash context pins the
/// supplied verified standard snapshot.
#[cfg(feature = "test-hooks")]
fn standard_v2_application_candidate(
    active: &ActiveDatabaseRevision,
    standard: &orna_core::revision::VerifiedStandardLibrarySnapshot,
) -> TestResult<DeployableRevision> {
    let context = CatalogueHashContext::version_two(standard.clone());
    let bundle = SourceBundleId::from_bytes([0x92; 16]);
    let bundle_hash = source_bundle_digest(&[])?;
    let source = StoredSourceRevision::new(
        bundle,
        SourceRevisionId::from_bytes([0x93; 16]),
        Some(active.pair().source()),
        vec![],
        bundle_hash,
        source_revision_record_digest(bundle, Some(active.pair().source()), bundle_hash)?,
    )?;
    let catalogue =
        CatalogueSnapshot::new(CatalogueRevisionId::from_bytes([0x94; 16]), vec![], vec![])?;
    let catalogue_hash = catalogue_digest_with_context(&context, &catalogue, &[], &[], &[], &[])?;
    Ok(DeployableRevision::new_with_catalogue_hash_context(
        DeployableRevisionInput::new(
            active.pair(),
            source,
            active.pair().catalogue(),
            catalogue,
            catalogue_hash,
            DeployableRevisionContent::new(vec![], vec![], vec![], vec![])
                .with_current_function_revisions(vec![]),
        ),
        context,
    )?)
}

/// Inserts one inactive application catalogue function with its complete
/// revision chain and an optional protected invocation-audit row.
#[cfg(feature = "test-hooks")]
async fn insert_inactive_application_function(
    database: &TestDatabase,
    discriminator: u8,
    function_id: &[u8],
    revision_id: &[u8],
    name_parts: &[&str],
    audit_event: bool,
) -> TestResult<(Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>)> {
    let session = database.open().await?;
    let bundle = vec![discriminator; 16];
    let unit = vec![discriminator + 1; 16];
    let source = vec![discriminator + 2; 16];
    let catalogue = vec![discriminator + 3; 16];
    let schema = vec![discriminator + 4; 16];
    let content_hash = vec![discriminator; 32];
    let principal = vec![discriminator + 5; 16];
    let client = session.client();
    client.batch_execute("BEGIN").await?;
    client
        .execute(
            "INSERT INTO _orna_kernel.source_bundles (id, content_hash)
             VALUES ($1, $2)",
            &[&bundle, &content_hash],
        )
        .await?;
    client
        .execute(
            "INSERT INTO _orna_kernel.source_units
                (id, bundle_id, ordinal, logical_path, content, content_hash)
             VALUES ($1, $2, 0, 'hostile/func.orna', 'hostile', $3)",
            &[&unit, &bundle, &content_hash],
        )
        .await?;
    let has_source_bundle_units: bool = client
        .query_one(
            "SELECT to_regclass('_orna_kernel.source_bundle_units') IS NOT NULL",
            &[],
        )
        .await?
        .get(0);
    if has_source_bundle_units {
        client
            .execute(
                "INSERT INTO _orna_kernel.source_bundle_units
                    (bundle_id, source_unit_id, ordinal)
                 VALUES ($1, $2, 0)",
                &[&bundle, &unit],
            )
            .await?;
    }
    client
        .execute(
            "INSERT INTO _orna_kernel.source_revisions
                (id, parent_source_revision_id, bundle_id, content_hash)
             VALUES ($1, NULL, $2, $3)",
            &[&source, &bundle, &content_hash],
        )
        .await?;
    client
        .execute(
            "INSERT INTO _orna_kernel.catalogue_revisions
                (id, source_revision_id, parent_catalogue_revision_id, content_hash)
             VALUES ($1, $2, NULL, $3)",
            &[&catalogue, &source, &content_hash],
        )
        .await?;
    client
        .execute(
            "INSERT INTO _orna_kernel.catalogue_schemas
                (catalogue_revision_id, schema_id, name_parts,
                 source_unit_id, source_start, source_end)
             VALUES ($1, $2, ARRAY['hostile'], $3, 0, 1)",
            &[&catalogue, &schema, &unit],
        )
        .await?;
    client
        .execute(
            "INSERT INTO _orna_kernel.function_revisions
                (id, introduced_catalogue_revision_id, function_id, revision_number,
                 content_hash, semantic_ir_hash, hash_algorithm, language_version, status)
             VALUES ($1, $2, $3, 1, $4, $4, 'sha256', 'orna.language/1', 'active')",
            &[
                &revision_id.to_vec(),
                &catalogue,
                &function_id.to_vec(),
                &content_hash,
            ],
        )
        .await?;
    let artifact_payload =
        ServerParameterEcho::new(STD_INVOKE_ECHO_PARAMETER_ID, STD_INTEGER_TYPE_ID)?.encode()?;
    let artifact_hash = artifact_payload_digest(&artifact_payload)?;
    client
        .execute(
            "INSERT INTO _orna_kernel.function_artifacts
                (function_revision_id, artifact_kind, format, format_version, payload,
                 content_hash, hash_algorithm, hash_contract_version)
             VALUES ($1, 'server_plan', 'orna.server-parameter-echo', 1, $2, $3, 'sha256', 1)",
            &[
                &revision_id.to_vec(),
                &artifact_payload,
                &artifact_hash.to_bytes().to_vec(),
            ],
        )
        .await?;
    client
        .execute(
            "INSERT INTO _orna_kernel.catalogue_functions
                (catalogue_revision_id, function_id, schema_id, name_parts, domain,
                 security_mode, transaction_mode, volatility, return_shape,
                 return_type_kind, return_scalar_type, return_target_type_id,
                 current_function_revision_id, source_unit_id, source_start, source_end)
             VALUES ($1, $2, $3, $4, 'server', 'invoker', 'read_only', 'stable', 'rows',
                     NULL, NULL, NULL, $5, $6, 0, 1)",
            &[
                &catalogue,
                &function_id.to_vec(),
                &schema,
                &name_parts
                    .iter()
                    .map(|part| part.to_string())
                    .collect::<Vec<_>>(),
                &revision_id.to_vec(),
                &unit,
            ],
        )
        .await?;
    if audit_event {
        let security_event = vec![discriminator + 6; 16];
        let invocation_event = vec![discriminator + 7; 16];
        let invocation_id = vec![discriminator + 8; 16];
        client
            .execute(
                "INSERT INTO _orna_kernel.security_audit_events
                    (event_id, event_kind, outcome, session_principal_id,
                     effective_principal_id, authorising_principal_id, function_id,
                     source_revision_id, catalogue_revision_id, denial_reason)
                 VALUES ($1, 'execute', 'allowed', $2, $2, $2, $3, $4, $5, NULL)",
                &[
                    &security_event,
                    &principal,
                    &function_id.to_vec(),
                    &source,
                    &catalogue,
                ],
            )
            .await?;
        client
            .execute(
                "INSERT INTO _orna_kernel.invocation_audit_events
                    (event_id, invocation_id, outcome, session_principal_id,
                     effective_principal_id, authorising_principal_id, function_id,
                     source_revision_id, catalogue_revision_id, security_audit_event_id)
                 VALUES ($1, $2, 'allowed', $3, $3, $3, $4, $5, $6, $7)",
                &[
                    &invocation_event,
                    &invocation_id,
                    &principal,
                    &function_id.to_vec(),
                    &source,
                    &catalogue,
                    &security_event,
                ],
            )
            .await?;
    }
    client.batch_execute("COMMIT").await?;
    session.shutdown().await?;
    Ok((
        catalogue,
        schema,
        function_id.to_vec(),
        revision_id.to_vec(),
    ))
}

/// Runs the migration SQL files whose numeric prefixes are in `versions`.
#[cfg(feature = "test-hooks")]
async fn run_migration_files(database: &TestDatabase, versions: &[u32]) -> TestResult<()> {
    let session = database.open().await?;
    session
        .client()
        .batch_execute("CREATE SCHEMA IF NOT EXISTS _orna_kernel; REVOKE ALL ON SCHEMA _orna_kernel FROM PUBLIC;")
        .await?;
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let mut paths = std::fs::read_dir(format!("{manifest_dir}/migrations"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    for path in paths {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| failure("migration file name"))?;
        let Some(version) = name.get(0..4).and_then(|prefix| prefix.parse::<u32>().ok()) else {
            continue;
        };
        if !versions.contains(&version) {
            continue;
        }
        let sql = std::fs::read_to_string(&path)?;
        session.client().batch_execute(&sql).await?;
    }
    session.shutdown().await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn persists_the_v2_standard_snapshot_and_authority_atomically() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel_instance = kernel(&database)?;
        kernel_instance.bootstrap().await?;
        install_standard_v2_parent_revision(&database)
            .await
            .map_err(|error| failure(format!("parent revision step: {error}")))?;
        let active = kernel_instance
            .recover()
            .await
            .map_err(|error| failure(format!("recover step: {error}")))?;
        let standard = verified_standard_v2_fixture()
            .map_err(|error| failure(format!("fixture step: {error}")))?;
        let candidate = standard_v2_application_candidate(&active, &standard)
            .map_err(|error| failure(format!("candidate step: {error}")))?;
        let applied = kernel_instance
            .apply_test_standard_upgrade(&candidate, &standard)
            .await
            .map_err(|error| failure(format!("apply step: {error}")))?;
        require_standard_context(&applied, &standard)
            .map_err(|error| failure(format!("context step: {error}")))?;
        require_recovered_snapshot(&candidate, &applied)?;
        let reopened = kernel_instance.recover().await?;
        require_standard_context(&reopened, &standard)?;

        let session = database.open().await?;
        let client = session.client();
        let standard_revision = standard.revision().to_bytes().to_vec();
        let catalogue_revision = candidate.candidate().revision().to_bytes().to_vec();
        let function_id = STD_INVOKE_ECHO_FUNCTION_ID.to_bytes().to_vec();
        let revision_id = STD_INVOKE_ECHO_FUNCTION_REVISION_ID.to_bytes().to_vec();

        let header = client
            .query_one(
                "SELECT digest_version FROM _orna_kernel.standard_library_revisions
                 WHERE id = $1",
                &[&standard_revision],
            )
            .await?;
        require(
            header.try_get::<_, i16>(0)? == 2,
            "standard header must record digest version 2",
        )?;

        let units = client
            .query(
                "SELECT membership.ordinal, source_unit.logical_path
                 FROM _orna_kernel.source_bundle_units AS membership
                 JOIN _orna_kernel.source_units AS source_unit
                   ON source_unit.id = membership.source_unit_id
                 WHERE membership.bundle_id = $1 ORDER BY membership.ordinal",
                &[&standard.source().bundle().to_bytes().to_vec()],
            )
            .await?;
        require(
            units.len() == 2
                && units[0].try_get::<_, i64>(0)? == 0
                && units[0].try_get::<_, String>(1)? == "std/types.orna"
                && units[1].try_get::<_, i64>(0)? == 1
                && units[1].try_get::<_, String>(1)? == "std/invoke.orna",
            "standard source units must persist both ordinals and paths",
        )?;

        let function = client
            .query_one(
                "SELECT name_parts, domain, security_mode, transaction_mode, volatility,
                        return_shape, return_type_kind, return_scalar_type,
                        current_function_revision_id, source_unit_id
                 FROM _orna_kernel.standard_catalogue_functions
                 WHERE standard_library_revision_id = $1 AND function_id = $2",
                &[&standard_revision, &function_id],
            )
            .await?;
        require(
            function.try_get::<_, Vec<String>>(0)?
                == vec!["std".to_owned(), "invoke".to_owned(), "echo".to_owned()]
                && function.try_get::<_, String>(1)? == "server"
                && function.try_get::<_, String>(2)? == "invoker"
                && function.try_get::<_, Option<String>>(3)? == Some("read_only".to_owned())
                && function.try_get::<_, String>(4)? == "stable"
                && function.try_get::<_, String>(5)? == "single"
                && function.try_get::<_, Option<String>>(6)? == Some("scalar".to_owned())
                && function.try_get::<_, Option<String>>(7)? == Some("integer".to_owned())
                && function.try_get::<_, Vec<u8>>(8)? == revision_id
                && function.try_get::<_, Vec<u8>>(9)? == STD_INVOKE_SOURCE_UNIT_ID.to_bytes().to_vec(),
            "standard catalogue function row must retain the exact resolved signature",
        )?;

        let parameter = client
            .query_one(
                "SELECT name, ordinal, type_kind, scalar_type, source_unit_id
                 FROM _orna_kernel.standard_catalogue_function_parameters
                 WHERE standard_library_revision_id = $1 AND function_id = $2 AND parameter_id = $3",
                &[&standard_revision, &function_id, &STD_INVOKE_ECHO_PARAMETER_ID.to_bytes().to_vec()],
            )
            .await?;
        require(
            parameter.try_get::<_, String>(0)? == "p_value"
                && parameter.try_get::<_, i64>(1)? == 0
                && parameter.try_get::<_, String>(2)? == "scalar"
                && parameter.try_get::<_, Option<String>>(3)? == Some("integer".to_owned())
                && parameter.try_get::<_, Vec<u8>>(4)? == STD_INVOKE_SOURCE_UNIT_ID.to_bytes().to_vec(),
            "standard parameter row must retain the exact ordered signature",
        )?;

        let revision_row = client
            .query_one(
                "SELECT function_id, revision_number, semantic_hash_version, language_version,
                        declaration_source_unit_id
                 FROM _orna_kernel.standard_function_revisions
                 WHERE standard_library_revision_id = $1 AND function_revision_id = $2",
                &[&standard_revision, &revision_id],
            )
            .await?;
        require(
            revision_row.try_get::<_, Vec<u8>>(0)? == function_id
                && revision_row.try_get::<_, i64>(1)? == 1
                && revision_row.try_get::<_, i16>(2)? == 2
                && revision_row.try_get::<_, String>(3)? == "orna.language/1"
                && revision_row.try_get::<_, Vec<u8>>(4)? == STD_INVOKE_SOURCE_UNIT_ID.to_bytes().to_vec(),
            "standard function revision row must retain the immutable revision facts",
        )?;

        let artifact = client
            .query_one(
                "SELECT artifact_kind, format, format_version, octet_length(payload)
                 FROM _orna_kernel.standard_function_artifacts
                 WHERE standard_library_revision_id = $1 AND function_revision_id = $2",
                &[&standard_revision, &revision_id],
            )
            .await?;
        require(
            artifact.try_get::<_, String>(0)? == "server_plan"
                && artifact.try_get::<_, String>(1)? == "orna.server-parameter-echo"
                && artifact.try_get::<_, i32>(2)? == 1
                && artifact.try_get::<_, i32>(3)? == 44,
            "standard artifact row must retain the exact 44-byte parameter-echo artifact",
        )?;

        let references = client
            .query(
                "SELECT ordinal, target_kind, reference_kind, target_standard_library_revision_id
                 FROM _orna_kernel.standard_definition_references
                 WHERE standard_library_revision_id = $1 AND function_revision_id = $2
                 ORDER BY ordinal",
                &[&standard_revision, &revision_id],
            )
            .await?;
        require(
            references.len() == 3,
            "standard reference rows must retain the exact ordered reference sequence",
        )?;
        for (index, reference) in references.iter().enumerate() {
            let i = i64::try_from(index)?;
            let pin = reference.try_get::<_, Option<Vec<u8>>>(3)?;
            require(
                reference.try_get::<_, i64>(0)? == i,
                "standard reference ordinal must be contiguous",
            )?;
            if index == 2 {
                require(
                    reference.try_get::<_, String>(1)? == "parameter"
                        && reference.try_get::<_, String>(2)? == "parameter_read"
                        && pin.is_none(),
                    "standard parameter reference must retain its scoped target",
                )?;
            } else {
                require(
                    reference.try_get::<_, String>(1)? == "value_type"
                        && reference.try_get::<_, String>(2)? == "named_type"
                        && pin == Some(standard_revision.clone()),
                    "standard value reference must pin the selected standard revision",
                )?;
            }
        }

        let authority = client
            .query_one(
                "SELECT target_class, function_revision_id, standard_library_revision_id
                 FROM _orna_kernel.invocation_target_authorities
                 WHERE catalogue_revision_id = $1 AND function_id = $2",
                &[&catalogue_revision, &function_id],
            )
            .await?;
        require(
            authority.try_get::<_, String>(0)? == "standard"
                && authority.try_get::<_, Vec<u8>>(1)? == revision_id
                && authority.try_get::<_, Vec<u8>>(2)? == standard_revision,
            "the standard authority row must pin the exact executable and standard revisions",
        )?;

        let application_rows: i64 = client
            .query_one(
                "SELECT count(*) FROM _orna_kernel.invocation_target_authorities
                 WHERE catalogue_revision_id = $1 AND target_class = 'application'",
                &[&catalogue_revision],
            )
            .await?
            .try_get(0)?;
        require(
            application_rows == 0,
            "an empty companion revision must not write application authority rows",
        )?;
        session.shutdown().await?;
        Ok(())
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn v1_standard_upgrade_writes_no_executable_rows() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let version_one = kernel
            .apply(&candidate(STANDARD_UPGRADE_V1_SOURCE, &empty)?)
            .await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)
            .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
        let applied = kernel.apply_standard_upgrade(&upgrade).await?;
        require_standard_context(&applied, upgrade.verified_standard_snapshot())?;

        let session = database.open().await?;
        let client = session.client();
        for table in [
            "standard_catalogue_functions",
            "standard_catalogue_function_parameters",
            "standard_function_revisions",
            "standard_function_artifacts",
            "standard_definition_references",
        ] {
            let count: i64 = client
                .query_one(&format!("SELECT count(*) FROM _orna_kernel.{table}"), &[])
                .await?
                .try_get(0)?;
            require(
                count == 0,
                "a version-one standard install must not write executable rows",
            )?;
        }
        let standard_rows: i64 = client
            .query_one(
                "SELECT count(*) FROM _orna_kernel.invocation_target_authorities
                 WHERE target_class = 'standard'",
                &[],
            )
            .await?
            .try_get(0)?;
        require(
            standard_rows == 0,
            "a version-one standard install must not write standard authority rows",
        )?;
        let expected_application_rows = applied.catalogue().functions().len() as i64;
        let application_rows: i64 = client
            .query_one(
                "SELECT count(*) FROM _orna_kernel.invocation_target_authorities
                 WHERE target_class = 'application' AND catalogue_revision_id = $1",
                &[&applied.pair().catalogue().to_bytes().to_vec()],
            )
            .await?
            .try_get(0)?;
        require(
            application_rows == expected_application_rows,
            "the companion application revision must retain one authority row per function",
        )?;
        session.shutdown().await?;
        Ok(())
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn v2_upgrade_rejects_duplicate_authority_row() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel_instance = kernel(&database)?;
        kernel_instance.bootstrap().await?;
        install_standard_v2_parent_revision(&database)
            .await
            .map_err(|error| failure(format!("parent revision step: {error}")))?;
        let active = kernel_instance
            .recover()
            .await
            .map_err(|error| failure(format!("recover step: {error}")))?;
        let standard = verified_standard_v2_fixture()
            .map_err(|error| failure(format!("fixture step: {error}")))?;
        let candidate = standard_v2_application_candidate(&active, &standard)
            .map_err(|error| failure(format!("candidate step: {error}")))?;
        let applied = kernel_instance
            .apply_test_standard_upgrade(&candidate, &standard)
            .await
            .map_err(|error| failure(format!("apply step: {error}")))?;
        require_standard_context(&applied, &standard)
            .map_err(|error| failure(format!("context step: {error}")))?;

        let session = database.open().await?;
        let catalogue_revision = candidate.candidate().revision().to_bytes().to_vec();
        let duplicate = session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.invocation_target_authorities
                    (catalogue_revision_id, function_id, target_class,
                     function_revision_id, standard_library_revision_id)
                 VALUES ($1, $2, 'standard', $3, $4)",
                &[
                    &catalogue_revision,
                    &STD_INVOKE_ECHO_FUNCTION_ID.to_bytes().to_vec(),
                    &STD_INVOKE_ECHO_FUNCTION_REVISION_ID.to_bytes().to_vec(),
                    &standard.revision().to_bytes().to_vec(),
                ],
            )
            .await;
        require(
            duplicate.is_err(),
            "a duplicate standard authority row must be rejected by the primary key",
        )?;
        let authority_rows: i64 = session
            .client()
            .query_one(
                "SELECT count(*) FROM _orna_kernel.invocation_target_authorities
                 WHERE catalogue_revision_id = $1 AND target_class = 'standard'",
                &[&catalogue_revision],
            )
            .await?
            .try_get(0)?;
        require(
            authority_rows == 1,
            "the standard authority row must exist exactly once",
        )?;
        session.shutdown().await?;
        Ok(())
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn migration_twenty_three_backfills_and_replaces_the_audit_target_fk() -> TestResult<()> {
    with_test_database(|database| async move {
        run_migration_files(&database, &(1..=22).collect::<Vec<_>>()).await?;
        let function_id = vec![0xc6; 16];
        let revision_id = vec![0xc7; 16];
        let (catalogue, _, _, _) = insert_inactive_application_function(
            &database,
            0xc6,
            &function_id,
            &revision_id,
            &["hostile", "audited"],
            true,
        )
        .await?;

        let session = database.open().await?;
        let before_rows: i64 = session
            .client()
            .query_one(
                "SELECT count(*) FROM _orna_kernel.invocation_audit_events",
                &[],
            )
            .await?
            .try_get(0)?;
        require(before_rows == 1, "the pre-migration audit row must exist")?;
        session.shutdown().await?;

        run_migration_files(&database, &[23]).await?;

        let session = database.open().await?;
        let client = session.client();
        let authority = client
            .query_one(
                "SELECT target_class, function_revision_id, standard_library_revision_id
                 FROM _orna_kernel.invocation_target_authorities
                 WHERE catalogue_revision_id = $1 AND function_id = $2",
                &[&catalogue, &function_id],
            )
            .await?;
        require(
            authority.try_get::<_, String>(0)? == "application"
                && authority.try_get::<_, Vec<u8>>(1)? == revision_id
                && authority.try_get::<_, Option<Vec<u8>>>(2)?.is_none(),
            "the migration must backfill exactly one application authority row per function",
        )?;
        let after_rows: i64 = client
            .query_one(
                "SELECT count(*) FROM _orna_kernel.invocation_audit_events",
                &[],
            )
            .await?
            .try_get(0)?;
        require(
            after_rows == before_rows,
            "the migration must never drop or rewrite an invocation-audit row",
        )?;
        let target_fk_points_at_authorities: bool = client
            .query_one(
                "SELECT confrelid = '_orna_kernel.invocation_target_authorities'::regclass
                 FROM pg_catalog.pg_constraint
                 WHERE conname = 'invocation_audit_events_target_fk'",
                &[],
            )
            .await?
            .try_get(0)?;
        require(
            target_fk_points_at_authorities,
            "the invocation-audit target foreign key must reference the authority relation",
        )?;
        let audit_targets: i64 = client
            .query_one(
                "SELECT count(*) FROM _orna_kernel.invocation_audit_events AS audit
                 JOIN _orna_kernel.invocation_target_authorities AS authority
                   ON authority.catalogue_revision_id = audit.catalogue_revision_id
                  AND authority.function_id = audit.function_id",
                &[],
            )
            .await?
            .try_get(0)?;
        require(
            audit_targets == before_rows,
            "every existing invocation-audit target pair must resolve through the authority relation",
        )?;
        session.shutdown().await?;
        Ok(())
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn migration_twenty_three_aborts_on_revision_mismatched_backfill() -> TestResult<()> {
    with_test_database(|database| async move {
        run_migration_files(&database, &(1..=22).collect::<Vec<_>>()).await?;
        insert_inactive_application_function(
            &database,
            0xd1,
            &[0xd2_u8; 16],
            &[0xd3_u8; 16],
            &["hostile", "corrupt"],
            false,
        )
        .await?;

        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let sql = std::fs::read_to_string(format!(
            "{manifest_dir}/migrations/0023_executable_standard_snapshots.sql"
        ))?;
        let split = sql
            .find("INSERT INTO _orna_kernel.invocation_target_authorities")
            .ok_or_else(|| failure("migration 23 backfill statement"))?;
        let ddl = &sql[..split];
        let backfill = &sql[split..];
        let session = database.open().await?;
        session.client().batch_execute(ddl).await?;
        session
            .client()
            .batch_execute(
                "CREATE FUNCTION _orna_kernel.test_corrupt_authority() RETURNS trigger
                 LANGUAGE plpgsql AS $$
                 BEGIN
                   NEW.function_revision_id := decode(repeat('ab', 16), 'hex');
                   RETURN NEW;
                 END $$;
                 CREATE TRIGGER corrupt_authority BEFORE INSERT
                 ON _orna_kernel.invocation_target_authorities
                 FOR EACH ROW EXECUTE FUNCTION _orna_kernel.test_corrupt_authority();",
            )
            .await?;
        let migration_result = session.client().batch_execute(backfill).await;
        require(
            migration_result.is_err(),
            "a revision-mismatched backfill must abort migration 23",
        )?;
        session.shutdown().await?;

        let session = database.open().await?;
        let client = session.client();
        let authority_rows: i64 = client
            .query_one(
                "SELECT count(*) FROM _orna_kernel.invocation_target_authorities",
                &[],
            )
            .await?
            .try_get(0)?;
        require(
            authority_rows == 0,
            "an aborted backfill must leave no authority row behind",
        )?;
        let target_fk_points_at_functions: bool = client
            .query_one(
                "SELECT confrelid = '_orna_kernel.catalogue_functions'::regclass
                 FROM pg_catalog.pg_constraint
                 WHERE conname = 'invocation_audit_events_target_fk'",
                &[],
            )
            .await?
            .try_get(0)?;
        require(
            target_fk_points_at_functions,
            "an aborted migration 23 must leave the invocation-audit target foreign key untouched",
        )?;
        session.shutdown().await?;
        Ok(())
    })
    .await
}
