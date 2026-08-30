use super::*;

struct RecoveredStandardHeader {
    revision: StandardLibraryRevisionId,
    bundle: SourceBundleId,
    source: SourceRevisionId,
    source_parent: Option<SourceRevisionId>,
    catalogue: CatalogueRevisionId,
    digest_version: StandardLibraryDigestVersion,
    language_version: String,
    bundle_hash: Sha256Digest,
    source_hash: Sha256Digest,
    digest: Sha256Digest,
}

struct RecoveredStandardSchema {
    definition: SchemaDefinition,
    origin: DefinitionOrigin,
}

struct RecoveredStandardValueType {
    schema: SchemaId,
    definition: ValueTypeDefinition,
    origin: DefinitionOrigin,
}

struct RecoveredStandardEnumType {
    schema: SchemaId,
    definition: EnumTypeDefinition,
    origin: DefinitionOrigin,
}

struct RecoveredStandardTypeBinding {
    binding: TypeBinding,
    origin: DefinitionOrigin,
}

struct RecoveredStandardFunction {
    schema: SchemaId,
    id: FunctionId,
    name: QualifiedSemanticName,
    domain: FunctionDomain,
    security: FunctionSecurity,
    transaction: Option<FunctionTransaction>,
    volatility: FunctionVolatility,
    return_type: FunctionReturn,
    current_revision: FunctionRevisionId,
    origin: DefinitionOrigin,
}

#[derive(Clone)]
struct RecoveredStandardParameter {
    function: FunctionId,
    definition: ParameterDefinition,
    origin: DefinitionOrigin,
}
pub(crate) async fn load_verified_standard_library(
    transaction: &Transaction<'_>,
    expected_revision: StandardLibraryRevisionId,
) -> Result<VerifiedStandardLibrarySnapshot, PostgresKernelError> {
    let header = load_standard_header(transaction, expected_revision).await?;
    let units = load_source_units(transaction, header.bundle).await?;
    let source = StoredSourceRevision::new(
        header.bundle,
        header.source,
        header.source_parent,
        units,
        header.bundle_hash,
        header.source_hash,
    )
    .map_err(PostgresKernelError::RevisionInvariant)?;
    let bundle_record =
        DurableRecord::new("_orna_kernel.source_bundles", header.bundle.canonical());
    let computed_bundle_hash =
        source_bundle_digest(source.units()).map_err(PostgresKernelError::CanonicalHash)?;
    if computed_bundle_hash != header.bundle_hash {
        return Err(bundle_record.invariant(
            "standard source bundle digest must match the ordered source unit records",
        ));
    }
    let source_record =
        DurableRecord::new("_orna_kernel.source_revisions", header.source.canonical());
    let computed_source_hash =
        source_revision_digest(&source).map_err(PostgresKernelError::CanonicalHash)?;
    if computed_source_hash != header.source_hash {
        return Err(source_record.invariant(
            "standard source revision digest must match its bundle, parent, and bundle digest",
        ));
    }

    let (catalogue, origins) = load_standard_catalogue(transaction, &header).await?;
    let snapshot = match header.digest_version {
        StandardLibraryDigestVersion::Version1 => {
            require_no_standard_executable_rows(transaction, header.revision).await?;
            StandardLibrarySnapshot::new(
                header.revision,
                header.digest_version,
                source,
                header.language_version,
                catalogue,
                origins,
                header.digest,
            )
            .map_err(PostgresKernelError::RevisionInvariant)?
        }
        StandardLibraryDigestVersion::Version2 => {
            let executables =
                load_standard_executable_facts(transaction, header.revision, &catalogue).await?;
            StandardLibrarySnapshot::new_with_executables(
                header.revision,
                header.digest_version,
                source,
                header.language_version,
                catalogue,
                executables,
                origins,
                header.digest,
            )
            .map_err(PostgresKernelError::RevisionInvariant)?
        }
        _ => {
            return Err(DurableRecord::new(
                "_orna_kernel.standard_library_revisions",
                header.revision.canonical(),
            )
            .invariant("standard library digest version is unsupported"));
        }
    };
    #[cfg(feature = "test-hooks")]
    {
        verify_recovered_standard_snapshot_for_test_hooks(snapshot)
    }
    #[cfg(not(feature = "test-hooks"))]
    {
        verify_recovered_standard_snapshot(snapshot)
    }
}

pub(super) fn verify_recovered_standard_snapshot(
    snapshot: StandardLibrarySnapshot,
) -> Result<VerifiedStandardLibrarySnapshot, PostgresKernelError> {
    let revision = snapshot.revision();
    let result = match revision {
        STANDARD_LIBRARY_REVISION_ID => verify_standard_library_snapshot(snapshot),
        STANDARD_LIBRARY_V2_REVISION_ID => verify_standard_library_v2_snapshot(snapshot),
        STANDARD_LIBRARY_V3_REVISION_ID => verify_standard_library_v3_snapshot(snapshot),
        STANDARD_LIBRARY_V4_REVISION_ID => verify_standard_library_v4_snapshot(snapshot),
        STANDARD_LIBRARY_V5_REVISION_ID => verify_standard_library_v5_snapshot(snapshot),
        STANDARD_LIBRARY_V6_REVISION_ID => verify_standard_library_v6_snapshot(snapshot),
        STANDARD_LIBRARY_V7_REVISION_ID => verify_standard_library_v7_snapshot(snapshot),
        STANDARD_LIBRARY_V8_REVISION_ID => verify_standard_library_v8_snapshot(snapshot),
        STANDARD_LIBRARY_V9_REVISION_ID => verify_standard_library_v9_snapshot(snapshot),
        _ => {
            return Err(DurableRecord::new(
                "_orna_kernel.standard_library_revisions",
                revision.canonical(),
            )
            .invariant("standard library revision identity is not an accepted retained revision"));
        }
    };
    result.map_err(|error| map_recovered_standard_verifier_error(error, revision))
}

fn map_recovered_standard_verifier_error(
    error: orna_standard::StandardLibraryError,
    revision: StandardLibraryRevisionId,
) -> PostgresKernelError {
    match error {
        orna_standard::StandardLibraryError::CanonicalHash { source } => {
            PostgresKernelError::CanonicalHash(source)
        }
        orna_standard::StandardLibraryError::Revision { source } => {
            PostgresKernelError::RevisionInvariant(source)
        }
        _ => DurableRecord::new(
            "_orna_kernel.standard_library_revisions",
            revision.canonical(),
        )
        .invariant("standard library retained verifier rejected the recovered snapshot"),
    }
}

#[cfg(feature = "test-hooks")]
fn verify_recovered_standard_snapshot_for_test_hooks(
    snapshot: StandardLibrarySnapshot,
) -> Result<VerifiedStandardLibrarySnapshot, PostgresKernelError> {
    let revision = snapshot.revision();
    if matches!(
        revision,
        STANDARD_LIBRARY_REVISION_ID
            | STANDARD_LIBRARY_V2_REVISION_ID
            | STANDARD_LIBRARY_V3_REVISION_ID
            | STANDARD_LIBRARY_V4_REVISION_ID
            | STANDARD_LIBRARY_V5_REVISION_ID
            | STANDARD_LIBRARY_V6_REVISION_ID
            | STANDARD_LIBRARY_V7_REVISION_ID
            | STANDARD_LIBRARY_V8_REVISION_ID
            | STANDARD_LIBRARY_V9_REVISION_ID
    ) {
        return verify_recovered_standard_snapshot(snapshot);
    }

    let result = match snapshot.digest_version() {
        StandardLibraryDigestVersion::Version1 => {
            verify_structural_standard_library_snapshot(snapshot)
        }
        StandardLibraryDigestVersion::Version2 => {
            verify_structural_standard_library_v2_snapshot(snapshot)
        }
        _ => {
            return Err(DurableRecord::new(
                "_orna_kernel.standard_library_revisions",
                revision.canonical(),
            )
            .invariant("standard library test fixture digest version is unsupported"));
        }
    };
    result.map_err(PostgresKernelError::CanonicalHash)
}

async fn load_standard_header(
    transaction: &Transaction<'_>,
    expected_revision: StandardLibraryRevisionId,
) -> Result<RecoveredStandardHeader, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.standard_library_revisions";
    let rows = transaction
        .query(
            "SELECT
                standard.id AS standard_id,
                standard.source_revision_id AS standard_source_id,
                standard.catalogue_revision_id AS standard_catalogue_id,
                standard.digest_version AS standard_digest_version,
                standard.language_version AS standard_language_version,
                standard.content_hash AS standard_digest,
                standard.hash_algorithm AS standard_algorithm,
                source.id AS source_id,
                source.parent_source_revision_id AS source_parent_id,
                source.bundle_id AS source_bundle_id,
                source.content_hash AS source_hash,
                source.hash_algorithm AS source_algorithm,
                source.hash_contract_version AS source_contract_version,
                bundle.id AS bundle_id,
                bundle.content_hash AS bundle_hash,
                bundle.hash_algorithm AS bundle_algorithm,
                bundle.hash_contract_version AS bundle_contract_version
             FROM _orna_kernel.standard_library_revisions AS standard
             JOIN _orna_kernel.source_revisions AS source
               ON source.id = standard.source_revision_id
             JOIN _orna_kernel.source_bundles AS bundle
               ON bundle.id = source.bundle_id
             WHERE standard.id = $1",
            &[&expected_revision.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    if rows.len() != 1 {
        return Err(DurableRecord::new(RELATION, expected_revision.canonical()).invariant(
            "each version 2 catalogue pin must join exactly one standard source revision and bundle",
        ));
    }
    decode_standard_header(&rows[0], expected_revision)
}

fn decode_standard_header(
    row: &Row,
    expected_revision: StandardLibraryRevisionId,
) -> Result<RecoveredStandardHeader, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.standard_library_revisions";
    let row_record = DurableRecord::new(RELATION, expected_revision.canonical());
    let revision = StandardLibraryRevisionId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "standard_id",
            "standard library revision identity must be 16 bytes",
        )?,
        &row_record,
        "standard library revision identity must be 16 bytes",
    )?);
    if revision != expected_revision {
        return Err(row_record.invariant(
            "selected standard library revision must identify the joined standard record",
        ));
    }
    let record = DurableRecord::new(RELATION, revision.canonical());
    let standard_source = SourceRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "standard_source_id",
            "standard source revision identity must be 16 bytes",
        )?,
        &record,
        "standard source revision identity must be 16 bytes",
    )?);
    let source = SourceRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "source_id",
            "joined standard source identity must be 16 bytes",
        )?,
        &record,
        "joined standard source identity must be 16 bytes",
    )?);
    if standard_source != source {
        return Err(record
            .invariant("standard library source link must identify the joined source revision"));
    }
    let bundle = SourceBundleId::from_bytes(identity_bytes(
        record.column(
            row,
            "source_bundle_id",
            "standard source bundle identity must be 16 bytes",
        )?,
        &record,
        "standard source bundle identity must be 16 bytes",
    )?);
    let joined_bundle = SourceBundleId::from_bytes(identity_bytes(
        record.column(
            row,
            "bundle_id",
            "joined standard bundle identity must be 16 bytes",
        )?,
        &record,
        "joined standard bundle identity must be 16 bytes",
    )?);
    if bundle != joined_bundle {
        return Err(
            record.invariant("standard source bundle link must identify the joined source bundle")
        );
    }
    let source_parent = optional_identity_bytes(
        record.column(
            row,
            "source_parent_id",
            "standard source parent identity must be null or 16 bytes",
        )?,
        &record,
        "standard source parent identity must be null or 16 bytes",
    )?
    .map(SourceRevisionId::from_bytes);
    let catalogue = CatalogueRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "standard_catalogue_id",
            "standard catalogue revision identity must be 16 bytes",
        )?,
        &record,
        "standard catalogue revision identity must be 16 bytes",
    )?);
    let digest_version = decode_standard_library_digest_version(
        record.column(
            row,
            "standard_digest_version",
            "standard library digest version must be a supported smallint",
        )?,
        &record,
    )?;
    let language_version: String = record.column(
        row,
        "standard_language_version",
        "standard library language version must be PostgreSQL text",
    )?;
    if language_version.is_empty() {
        return Err(record.invariant("standard library language version must not be empty"));
    }
    let standard_algorithm: String = record.column(
        row,
        "standard_algorithm",
        "standard library hash algorithm must be sha256",
    )?;
    exact_enum(
        &standard_algorithm,
        &[("sha256", HashAlgorithm::Sha256)],
        &record,
        "standard library hash algorithm must be sha256",
    )?;
    let source_record = DurableRecord::new("_orna_kernel.source_revisions", source.canonical());
    let bundle_record = DurableRecord::new("_orna_kernel.source_bundles", bundle.canonical());
    require_hash_contract(
        row,
        &source_record,
        "source_algorithm",
        "source_contract_version",
        "standard source hash algorithm must be sha256",
        "standard source hash contract version must be 1",
    )?;
    require_hash_contract(
        row,
        &bundle_record,
        "bundle_algorithm",
        "bundle_contract_version",
        "standard bundle hash algorithm must be sha256",
        "standard bundle hash contract version must be 1",
    )?;

    Ok(RecoveredStandardHeader {
        revision,
        bundle,
        source,
        source_parent,
        catalogue,
        digest_version,
        language_version,
        bundle_hash: Sha256Digest::from_bytes(digest_bytes(
            bundle_record.column(
                row,
                "bundle_hash",
                "standard bundle digest must be 32 bytes",
            )?,
            &bundle_record,
            "standard bundle digest must be 32 bytes",
        )?),
        source_hash: Sha256Digest::from_bytes(digest_bytes(
            source_record.column(
                row,
                "source_hash",
                "standard source digest must be 32 bytes",
            )?,
            &source_record,
            "standard source digest must be 32 bytes",
        )?),
        digest: Sha256Digest::from_bytes(digest_bytes(
            record.column(
                row,
                "standard_digest",
                "standard library digest must be 32 bytes",
            )?,
            &record,
            "standard library digest must be 32 bytes",
        )?),
    })
}

fn decode_standard_library_digest_version(
    value: i16,
    record: &DurableRecord,
) -> Result<StandardLibraryDigestVersion, PostgresKernelError> {
    let value = decode_durable_version(
        value,
        record,
        "standard library digest version must be a supported smallint",
    )?;
    StandardLibraryDigestVersion::try_from(value)
        .map_err(|_| record.invariant("standard library digest version must be 1"))
}

async fn load_standard_catalogue(
    transaction: &Transaction<'_>,
    header: &RecoveredStandardHeader,
) -> Result<(CatalogueSnapshot, Vec<DefinitionOrigin>), PostgresKernelError> {
    let schemas = load_standard_schemas(transaction, header.revision).await?;
    let value_types = load_standard_value_types(transaction, header.revision).await?;
    let value_type_ids = value_types
        .iter()
        .map(|value_type| value_type.definition.id())
        .collect::<HashSet<_>>();
    let enum_types = load_standard_enum_types(transaction, header.revision).await?;
    let bindings = load_standard_type_bindings(transaction, header.revision).await?;
    let functions = load_standard_functions(transaction, header.revision, &value_type_ids).await?;
    let parameters =
        load_standard_parameters(transaction, header.revision, &value_type_ids).await?;

    let schema_names = schemas
        .iter()
        .map(|schema| (schema.definition.id(), schema.definition.name().clone()))
        .collect::<BTreeMap<_, _>>();
    let mut origins = Vec::with_capacity(
        schemas.len()
            + value_types.len()
            + enum_types.len()
            + bindings.len()
            + functions.len()
            + parameters.len(),
    );
    let schemas = schemas
        .into_iter()
        .map(|schema| {
            origins.push(schema.origin);
            schema.definition
        })
        .collect::<Vec<_>>();
    let mut definitions = Vec::with_capacity(value_types.len());
    for value_type in value_types {
        let record = DurableRecord::new(
            "_orna_kernel.standard_catalogue_value_types",
            value_type.definition.id().canonical(),
        );
        require_standard_definition_schema(
            &record,
            &schema_names,
            value_type.schema,
            value_type.definition.name(),
            "standard value type schema identity must identify a recovered schema",
            "standard value type qualified name must contain a schema namespace",
            "standard value type schema identity must equal the schema named by its namespace",
        )?;
        origins.push(value_type.origin);
        definitions.push(value_type.definition);
    }
    let mut enum_definitions = Vec::with_capacity(enum_types.len());
    for enum_type in enum_types {
        let record = DurableRecord::new(
            "_orna_kernel.standard_catalogue_enum_types",
            enum_type.definition.id().canonical(),
        );
        require_standard_definition_schema(
            &record,
            &schema_names,
            enum_type.schema,
            enum_type.definition.name(),
            "standard enum schema identity must identify a recovered schema",
            "standard enum qualified name must contain a schema namespace",
            "standard enum schema identity must equal the schema named by its namespace",
        )?;
        origins.push(enum_type.origin);
        enum_definitions.push(enum_type.definition);
    }
    let bindings = bindings
        .into_iter()
        .map(|binding| {
            origins.push(binding.origin);
            binding.binding
        })
        .collect::<Vec<_>>();
    let function_definitions = functions
        .into_iter()
        .map(|function| {
            let record = DurableRecord::new(
                "_orna_kernel.standard_catalogue_functions",
                function.id.canonical(),
            );
            require_standard_definition_schema(
                &record,
                &schema_names,
                function.schema,
                &function.name,
                "standard function schema identity must identify a recovered schema",
                "standard function qualified name must contain a schema namespace",
                "standard function schema identity must equal the schema named by its namespace",
            )?;
            let recovered_parameters = parameters.get(&function.id).cloned().unwrap_or_default();
            let definition = FunctionDefinition::new(
                function.id,
                function.name,
                function.domain,
                recovered_parameters
                    .iter()
                    .map(|parameter| parameter.definition.clone())
                    .collect(),
                function.return_type,
                function.current_revision,
                function.security,
                function.transaction,
                function.volatility,
            );
            origins.push(function.origin);
            origins.extend(
                recovered_parameters
                    .into_iter()
                    .map(|parameter| parameter.origin),
            );
            Ok(definition)
        })
        .collect::<Result<Vec<_>, PostgresKernelError>>()?;
    let catalogue = CatalogueSnapshot::new_with_functions_and_enum_types(
        header.catalogue,
        schemas,
        Vec::new(),
        definitions,
        enum_definitions,
        bindings,
        function_definitions,
    )
    .map_err(PostgresKernelError::CatalogueSnapshot)?;
    Ok((catalogue, origins))
}

#[allow(clippy::too_many_arguments)]
fn require_standard_definition_schema(
    record: &DurableRecord,
    schema_names: &BTreeMap<SchemaId, QualifiedSemanticName>,
    schema: SchemaId,
    name: &QualifiedSemanticName,
    missing_schema_rule: &'static str,
    missing_namespace_rule: &'static str,
    mismatch_rule: &'static str,
) -> Result<(), PostgresKernelError> {
    let schema_name = schema_names
        .get(&schema)
        .ok_or_else(|| record.invariant(missing_schema_rule))?;
    let name_parts = name.parts();
    let namespace = name_parts
        .get(..name_parts.len().saturating_sub(1))
        .filter(|parts| !parts.is_empty())
        .ok_or_else(|| record.invariant(missing_namespace_rule))?;
    if namespace != schema_name.parts() {
        return Err(record.invariant(mismatch_rule));
    }
    Ok(())
}

/// A version-one standard revision must have no row in any new executable
/// relation. The version-one digest contract covers no executable fact, so
/// stray executable rows would otherwise survive recovery unverified.
async fn require_no_standard_executable_rows(
    transaction: &Transaction<'_>,
    standard: StandardLibraryRevisionId,
) -> Result<(), PostgresKernelError> {
    const EXECUTABLE_RELATIONS: &[&str] = &[
        "standard_catalogue_functions",
        "standard_catalogue_function_parameters",
        "standard_function_revisions",
        "standard_function_artifacts",
        "standard_definition_references",
    ];
    for relation in EXECUTABLE_RELATIONS {
        let rows = transaction
            .query(
                &format!(
                    "SELECT 1 FROM _orna_kernel.{relation}
                     WHERE standard_library_revision_id = $1 LIMIT 1"
                ),
                &[&standard.to_bytes().to_vec()],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
        if !rows.is_empty() {
            return Err(DurableRecord::new(relation, standard.canonical())
                .invariant("a version-one standard revision must have no executable rows"));
        }
    }
    Ok(())
}

async fn load_standard_functions(
    transaction: &Transaction<'_>,
    standard: StandardLibraryRevisionId,
    value_type_ids: &HashSet<TypeId>,
) -> Result<Vec<RecoveredStandardFunction>, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.standard_catalogue_functions";
    let rows = transaction
        .query(
            "SELECT standard_library_revision_id, function_id, schema_id, name_parts,
                    domain, security_mode, transaction_mode, volatility, return_shape,
                    return_type_kind, return_scalar_type, return_value_type_id,
                    current_function_revision_id, source_unit_id, source_start, source_end
             FROM _orna_kernel.standard_catalogue_functions
             WHERE standard_library_revision_id = $1
             ORDER BY function_id",
            &[&standard.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let mut functions = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        functions.push(decode_standard_function(
            row,
            index,
            standard,
            RELATION,
            value_type_ids,
        )?);
    }
    Ok(functions)
}

fn decode_standard_function(
    row: &Row,
    index: usize,
    expected_standard: StandardLibraryRevisionId,
    relation: &'static str,
    value_type_ids: &HashSet<TypeId>,
) -> Result<RecoveredStandardFunction, PostgresKernelError> {
    let row_record = DurableRecord::new(relation, format!("row={index}"));
    require_standard_library_revision(row, &row_record, expected_standard, "function")?;
    let id = FunctionId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "function_id",
            "standard function identity must be 16 bytes",
        )?,
        &row_record,
        "standard function identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(relation, id.canonical());
    let schema = SchemaId::from_bytes(identity_bytes(
        record.column(
            row,
            "schema_id",
            "standard function schema identity must be 16 bytes",
        )?,
        &record,
        "standard function schema identity must be 16 bytes",
    )?);
    let name_parts: Vec<String> = record.column(
        row,
        "name_parts",
        "standard function name parts must be an exact PostgreSQL text array",
    )?;
    let name = QualifiedSemanticName::new(name_parts).map_err(|_| {
        record.invariant("standard function name parts must form one exact semantic name")
    })?;
    let domain_name: String =
        record.column(row, "domain", "standard function domain must decode")?;
    let domain = exact_enum(
        &domain_name,
        &[
            ("server", FunctionDomain::Server),
            ("client", FunctionDomain::Client),
        ],
        &record,
        "standard function domain must be server or client",
    )?;
    let security_name: String = record.column(
        row,
        "security_mode",
        "standard function security must decode",
    )?;
    let security = exact_enum(
        &security_name,
        &[
            ("invoker", FunctionSecurity::Invoker),
            ("definer", FunctionSecurity::Definer),
        ],
        &record,
        "standard function security must be invoker or definer",
    )?;
    let transaction_name: Option<String> = record.column(
        row,
        "transaction_mode",
        "standard function transaction mode must decode",
    )?;
    let transaction = transaction_name
        .map(|name| {
            exact_enum(
                &name,
                &[
                    ("atomic", FunctionTransaction::Atomic),
                    ("read_only", FunctionTransaction::ReadOnly),
                ],
                &record,
                "standard function transaction mode must be atomic or read_only",
            )
        })
        .transpose()?;
    let volatility_name: String = record.column(
        row,
        "volatility",
        "standard function volatility must decode",
    )?;
    let volatility = exact_enum(
        &volatility_name,
        &[
            ("immutable", FunctionVolatility::Immutable),
            ("stable", FunctionVolatility::Stable),
            ("volatile", FunctionVolatility::Volatile),
        ],
        &record,
        "standard function volatility must be immutable, stable, or volatile",
    )?;
    let return_type = decode_standard_function_return(row, &record, value_type_ids)?;
    let current_revision = FunctionRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "current_function_revision_id",
            "standard function current revision identity must be 16 bytes",
        )?,
        &record,
        "standard function current revision identity must be 16 bytes",
    )?);
    let origin = decode_origin(row, &record, DefinitionIdentity::Function(id))?;
    Ok(RecoveredStandardFunction {
        schema,
        id,
        name,
        domain,
        security,
        transaction,
        volatility,
        return_type,
        current_revision,
        origin,
    })
}

fn decode_standard_function_return(
    row: &Row,
    record: &DurableRecord,
    value_type_ids: &HashSet<TypeId>,
) -> Result<FunctionReturn, PostgresKernelError> {
    let shape: String = record.column(
        row,
        "return_shape",
        "standard function return shape must decode",
    )?;
    if shape != "single" {
        return Err(record.invariant(
            "standard catalogue functions with ROWS results are not supported by standard persistence",
        ));
    }
    let kind: Option<String> = record.column(
        row,
        "return_type_kind",
        "standard function return type kind must decode",
    )?;
    let scalar: Option<String> = record.column(
        row,
        "return_scalar_type",
        "standard function return scalar type must decode",
    )?;
    let value_type: Option<Vec<u8>> = record.column(
        row,
        "return_value_type_id",
        "standard function return value type identity must be null or exact bytes",
    )?;
    let resolved = decode_standard_resolved_type(
        kind,
        scalar,
        value_type,
        Some(value_type_ids),
        true,
        record,
    )?;
    Ok(FunctionReturn::Single(resolved))
}

/// Decodes the closed scalar-or-value resolved type persisted for standard
/// catalogue functions and parameters. `value_type_ids` is required for the
/// value shape so the type must identify one standard value type.
fn decode_standard_resolved_type(
    kind: Option<String>,
    scalar_name: Option<String>,
    value_type: Option<Vec<u8>>,
    value_type_ids: Option<&HashSet<TypeId>>,
    allow_void: bool,
    record: &DurableRecord,
) -> Result<ResolvedType, PostgresKernelError> {
    match kind.as_deref() {
        Some("scalar") => {
            if value_type.is_some() {
                return Err(record.invariant(
                    "standard resolved type columns must form one exact scalar or value tuple",
                ));
            }
            let Some(scalar_name) = scalar_name else {
                return Err(record.invariant(
                    "standard resolved type columns must form one exact scalar or value tuple",
                ));
            };
            let scalar = exact_enum(
                &scalar_name,
                &[
                    ("boolean", StandardScalar::Boolean),
                    ("integer", StandardScalar::Integer),
                    ("bigint", StandardScalar::BigInt),
                    ("float", StandardScalar::Float),
                    ("decimal", StandardScalar::Decimal),
                    (
                        "character_large_object",
                        StandardScalar::CharacterLargeObject,
                    ),
                    ("binary_large_object", StandardScalar::BinaryLargeObject),
                    ("uuid", StandardScalar::Uuid),
                    ("date", StandardScalar::Date),
                    ("time", StandardScalar::Time),
                    ("timestamp", StandardScalar::Timestamp),
                    ("duration", StandardScalar::Duration),
                    ("void", StandardScalar::Void),
                ],
                record,
                "standard resolved scalar type must be one exact supported scalar",
            )?;
            if scalar == StandardScalar::Void && !allow_void {
                return Err(record.invariant(
                    "void is valid only as a SINGLE function return, never as a parameter",
                ));
            }
            Ok(ResolvedType::scalar(scalar))
        }
        Some("value") => {
            if scalar_name.is_some() {
                return Err(record.invariant(
                    "standard resolved type columns must form one exact scalar or value tuple",
                ));
            }
            let Some(bytes) = value_type else {
                return Err(record.invariant(
                    "standard resolved type columns must form one exact scalar or value tuple",
                ));
            };
            let id = TypeId::from_bytes(identity_bytes(
                bytes,
                record,
                "standard resolved value type identity must be 16 bytes",
            )?);
            if value_type_ids.is_none_or(|value_type_ids| !value_type_ids.contains(&id)) {
                return Err(record.invariant(
                    "standard resolved value type must identify one standard catalogue value type",
                ));
            }
            Ok(ResolvedType::value(id))
        }
        _ => Err(record.invariant("standard resolved type kind must be scalar or value")),
    }
}

async fn load_standard_parameters(
    transaction: &Transaction<'_>,
    standard: StandardLibraryRevisionId,
    value_type_ids: &HashSet<TypeId>,
) -> Result<BTreeMap<FunctionId, Vec<RecoveredStandardParameter>>, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.standard_catalogue_function_parameters";
    let rows = transaction
        .query(
            "SELECT standard_library_revision_id, function_id, parameter_id, name, ordinal,
                    type_kind, scalar_type, value_type_id,
                    source_unit_id, source_start, source_end
             FROM _orna_kernel.standard_catalogue_function_parameters
             WHERE standard_library_revision_id = $1
             ORDER BY function_id, ordinal, parameter_id",
            &[&standard.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let mut parameters = BTreeMap::<FunctionId, Vec<RecoveredStandardParameter>>::new();
    for (index, row) in rows.iter().enumerate() {
        let parameter = decode_standard_parameter(row, index, standard, RELATION, value_type_ids)?;
        parameters
            .entry(parameter.function)
            .or_default()
            .push(parameter);
    }
    Ok(parameters)
}

fn decode_standard_parameter(
    row: &Row,
    index: usize,
    expected_standard: StandardLibraryRevisionId,
    relation: &'static str,
    value_type_ids: &HashSet<TypeId>,
) -> Result<RecoveredStandardParameter, PostgresKernelError> {
    let row_record = DurableRecord::new(relation, format!("row={index}"));
    require_standard_library_revision(row, &row_record, expected_standard, "parameter")?;
    let function = FunctionId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "function_id",
            "standard parameter owner identity must be 16 bytes",
        )?,
        &row_record,
        "standard parameter owner identity must be 16 bytes",
    )?);
    let id = ParameterId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "parameter_id",
            "standard parameter identity must be 16 bytes",
        )?,
        &row_record,
        "standard parameter identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(
        relation,
        format!(
            "function={} parameter={}",
            function.canonical(),
            id.canonical()
        ),
    );
    let name: String = record.column(
        row,
        "name",
        "standard parameter name must be PostgreSQL text",
    )?;
    if name.is_empty() {
        return Err(record.invariant("standard parameter name must not be empty"));
    }
    let ordinal = u32_from_i64(
        record.column(row, "ordinal", "standard parameter ordinal must fit u32")?,
        &record,
        "standard parameter ordinal must fit u32",
    )?;
    let kind: Option<String> =
        record.column(row, "type_kind", "standard parameter type kind must decode")?;
    let scalar: Option<String> = record.column(
        row,
        "scalar_type",
        "standard parameter scalar type must decode",
    )?;
    let value_type: Option<Vec<u8>> = record.column(
        row,
        "value_type_id",
        "standard parameter value type identity must be null or exact bytes",
    )?;
    let resolved = decode_standard_resolved_type(
        kind,
        scalar,
        value_type,
        Some(value_type_ids),
        false,
        &record,
    )?;
    let origin = decode_origin(
        row,
        &record,
        DefinitionIdentity::Parameter {
            owner: function,
            parameter: id,
        },
    )?;
    Ok(RecoveredStandardParameter {
        function,
        definition: ParameterDefinition::new(id, name, ordinal, resolved, None),
        origin,
    })
}

/// Reconstructs the complete version-2 standard executable sequence: the
/// immutable function revisions with their artifacts and the ordered
/// definition references, aligned one-to-one with the catalogue functions.
async fn load_standard_executable_facts(
    transaction: &Transaction<'_>,
    standard: StandardLibraryRevisionId,
    catalogue: &CatalogueSnapshot,
) -> Result<Vec<StandardExecutable>, PostgresKernelError> {
    let artifacts = load_standard_artifacts(transaction, standard).await?;
    let revisions = load_standard_revisions(transaction, standard, &artifacts).await?;
    let references = load_standard_references(transaction, standard, &revisions).await?;
    build_standard_executables(catalogue, revisions, references)
}

async fn load_standard_artifacts(
    transaction: &Transaction<'_>,
    standard: StandardLibraryRevisionId,
) -> Result<BTreeMap<FunctionRevisionId, Vec<ExecutableArtifact>>, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.standard_function_artifacts";
    let rows = transaction
        .query(
            "SELECT standard_library_revision_id, function_revision_id, artifact_kind,
                    format, format_version::bigint AS format_version, payload, content_hash,
                    hash_algorithm, hash_contract_version
             FROM _orna_kernel.standard_function_artifacts
             WHERE standard_library_revision_id = $1
             ORDER BY function_revision_id, artifact_kind",
            &[&standard.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let mut artifacts = BTreeMap::<FunctionRevisionId, Vec<ExecutableArtifact>>::new();
    for (index, row) in rows.iter().enumerate() {
        let (revision, artifact) = decode_standard_artifact(row, index, standard, RELATION)?;
        artifacts.entry(revision).or_default().push(artifact);
    }
    Ok(artifacts)
}

fn decode_standard_artifact(
    row: &Row,
    index: usize,
    expected_standard: StandardLibraryRevisionId,
    relation: &'static str,
) -> Result<(FunctionRevisionId, ExecutableArtifact), PostgresKernelError> {
    let row_record = DurableRecord::new(relation, format!("row={index}"));
    require_standard_library_revision(row, &row_record, expected_standard, "function artifact")?;
    let revision = FunctionRevisionId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "function_revision_id",
            "standard artifact function revision identity must be 16 bytes",
        )?,
        &row_record,
        "standard artifact function revision identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(relation, revision.canonical());
    require_hash_contract(
        row,
        &record,
        "hash_algorithm",
        "hash_contract_version",
        "standard function artifact hash algorithm must be sha256",
        "standard function artifact hash contract version must be 1",
    )?;
    let kind_name: String =
        record.column(row, "artifact_kind", "standard artifact kind must decode")?;
    let kind = exact_enum(
        &kind_name,
        &[
            ("server_plan", ExecutableArtifactKind::Server),
            ("client_bytecode", ExecutableArtifactKind::Client),
        ],
        &record,
        "standard artifact kind must be server_plan or client_bytecode",
    )?;
    let format: String = record.column(row, "format", "standard artifact format must be text")?;
    let version = u32_from_i64(
        record.column(
            row,
            "format_version",
            "standard artifact format version must fit u32",
        )?,
        &record,
        "standard artifact format version must fit u32",
    )?;
    let payload: Vec<u8> = record.column(
        row,
        "payload",
        "standard artifact payload must be exact bytes",
    )?;
    let content_hash = Sha256Digest::from_bytes(digest_bytes(
        record.column(
            row,
            "content_hash",
            "standard artifact content hash must be 32 bytes",
        )?,
        &record,
        "standard artifact content hash must be 32 bytes",
    )?);
    let computed = artifact_payload_digest(&payload).map_err(PostgresKernelError::CanonicalHash)?;
    if computed != content_hash {
        return Err(record.invariant("standard artifact digest must match its exact payload"));
    }
    let artifact = ExecutableArtifact::new(kind, format, version, payload, content_hash)
        .map_err(PostgresKernelError::RevisionInvariant)?;
    Ok((revision, artifact))
}

async fn load_standard_revisions(
    transaction: &Transaction<'_>,
    standard: StandardLibraryRevisionId,
    artifacts: &BTreeMap<FunctionRevisionId, Vec<ExecutableArtifact>>,
) -> Result<Vec<FunctionRevisionRecord>, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.standard_function_revisions";
    let rows = transaction
        .query(
            "SELECT standard_library_revision_id, function_revision_id, function_id,
                    revision_number, declaration_source_unit_id, declaration_source_start,
                    declaration_source_end, declaration_content_hash, semantic_hash,
                    semantic_hash_version, language_version, hash_contract_version
             FROM _orna_kernel.standard_function_revisions
             WHERE standard_library_revision_id = $1
             ORDER BY function_revision_id",
            &[&standard.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let mut revisions = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        revisions.push(decode_standard_revision(
            row, index, standard, RELATION, artifacts,
        )?);
    }
    Ok(revisions)
}

fn decode_standard_revision(
    row: &Row,
    index: usize,
    expected_standard: StandardLibraryRevisionId,
    relation: &'static str,
    artifacts: &BTreeMap<FunctionRevisionId, Vec<ExecutableArtifact>>,
) -> Result<FunctionRevisionRecord, PostgresKernelError> {
    let row_record = DurableRecord::new(relation, format!("row={index}"));
    require_standard_library_revision(row, &row_record, expected_standard, "function revision")?;
    let id = FunctionRevisionId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "function_revision_id",
            "standard function revision identity must be 16 bytes",
        )?,
        &row_record,
        "standard function revision identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(relation, id.canonical());
    let contract_version: i16 = record.column(
        row,
        "hash_contract_version",
        "standard function revision hash contract version must be 1",
    )?;
    if contract_version != 1 {
        return Err(record.invariant("standard function revision hash contract version must be 1"));
    }
    let function = FunctionId::from_bytes(identity_bytes(
        record.column(
            row,
            "function_id",
            "standard function revision owner identity must be 16 bytes",
        )?,
        &record,
        "standard function revision owner identity must be 16 bytes",
    )?);
    let revision_number = u64_from_i64(
        record.column(
            row,
            "revision_number",
            "standard function revision number must be a positive bigint",
        )?,
        &record,
        "standard function revision number must be a positive u64",
    )?;
    if revision_number == 0 {
        return Err(record.invariant("standard function revision number must be positive"));
    }
    let declaration_origin = decode_required_source_origin_columns(row, &record)?;
    let declaration_content_hash = Sha256Digest::from_bytes(digest_bytes(
        record.column(
            row,
            "declaration_content_hash",
            "standard function declaration hash must be 32 bytes",
        )?,
        &record,
        "standard function declaration hash must be 32 bytes",
    )?);
    let semantic_hash = Sha256Digest::from_bytes(digest_bytes(
        record.column(
            row,
            "semantic_hash",
            "standard function semantic hash must be 32 bytes",
        )?,
        &record,
        "standard function semantic hash must be 32 bytes",
    )?);
    let semantic_hash_version = decode_durable_version(
        record.column(
            row,
            "semantic_hash_version",
            "standard function semantic hash version must be a supported smallint",
        )?,
        &record,
        "standard function semantic hash version must be a supported smallint",
    )?;
    let semantic_hash_version = FunctionSemanticHashVersion::try_from(semantic_hash_version)
        .map_err(|_| record.invariant("standard function semantic hash version must be 1 or 2"))?;
    let language_version: String = record.column(
        row,
        "language_version",
        "standard function language version must be PostgreSQL text",
    )?;
    let revision_artifacts = artifacts.get(&id).ok_or_else(|| {
        record.invariant("standard function revision must own exactly one artifact")
    })?;
    if revision_artifacts.len() != 1 {
        return Err(record.invariant("standard function revision must own exactly one artifact"));
    }
    FunctionRevisionRecord::new(
        function,
        id,
        revision_number,
        declaration_origin,
        declaration_content_hash,
        semantic_hash,
        language_version,
        revision_artifacts[0].clone(),
    )
    .map_err(PostgresKernelError::RevisionInvariant)
    .map(|revision| revision.with_semantic_hash_version(semantic_hash_version))
}

fn decode_required_source_origin_columns(
    row: &Row,
    record: &DurableRecord,
) -> Result<SourceOrigin, PostgresKernelError> {
    let unit = SourceUnitId::from_bytes(identity_bytes(
        record.column(
            row,
            "declaration_source_unit_id",
            "standard function declaration source unit identity must be 16 bytes",
        )?,
        record,
        "standard function declaration source unit identity must be 16 bytes",
    )?);
    let start = u32_from_i64(
        record.column(
            row,
            "declaration_source_start",
            "standard function declaration source start must fit u32",
        )?,
        record,
        "standard function declaration source start must fit u32",
    )?;
    let end = u32_from_i64(
        record.column(
            row,
            "declaration_source_end",
            "standard function declaration source end must fit u32",
        )?,
        record,
        "standard function declaration source end must fit u32",
    )?;
    SourceOrigin::new(unit, start, end).map_err(PostgresKernelError::RevisionInvariant)
}

async fn load_standard_references(
    transaction: &Transaction<'_>,
    standard: StandardLibraryRevisionId,
    revisions: &[FunctionRevisionRecord],
) -> Result<Vec<DefinitionReference>, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.standard_definition_references";
    let rows = transaction
        .query(
            "SELECT standard_library_revision_id, function_revision_id, ordinal,
                    target_definition_id, target_kind, target_owner_type_id,
                    target_owner_function_id, target_standard_library_revision_id,
                    reference_kind, source_unit_id, source_start, source_end
             FROM _orna_kernel.standard_definition_references
             WHERE standard_library_revision_id = $1
             ORDER BY function_revision_id, ordinal",
            &[&standard.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let mut references = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        references.push(decode_standard_reference(
            row, index, standard, RELATION, revisions,
        )?);
    }
    Ok(references)
}

fn decode_standard_reference(
    row: &Row,
    index: usize,
    expected_standard: StandardLibraryRevisionId,
    relation: &'static str,
    revisions: &[FunctionRevisionRecord],
) -> Result<DefinitionReference, PostgresKernelError> {
    let row_record = DurableRecord::new(relation, format!("row={index}"));
    require_standard_library_revision(row, &row_record, expected_standard, "definition reference")?;
    let source_revision = FunctionRevisionId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "function_revision_id",
            "standard reference source revision identity must be 16 bytes",
        )?,
        &row_record,
        "standard reference source revision identity must be 16 bytes",
    )?);
    let ordinal = u32_from_i64(
        row_record.column(row, "ordinal", "standard reference ordinal must fit u32")?,
        &row_record,
        "standard reference ordinal must fit u32",
    )?;
    let record = DurableRecord::new(
        relation,
        format!("revision={} ordinal={ordinal}", source_revision.canonical()),
    );
    let source_function = revisions
        .iter()
        .find(|revision| revision.id() == source_revision)
        .map(FunctionRevisionRecord::function)
        .ok_or_else(|| {
            record.invariant(
                "standard reference source revision must identify one recovered function revision",
            )
        })?;
    let target_bytes = identity_bytes(
        record.column(
            row,
            "target_definition_id",
            "standard reference target identity must be 16 bytes",
        )?,
        &record,
        "standard reference target identity must be 16 bytes",
    )?;
    let owner_type = optional_identity_bytes(
        record.column(
            row,
            "target_owner_type_id",
            "standard reference target type owner must be null or 16 bytes",
        )?,
        &record,
        "standard reference target type owner must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    let owner_function = optional_identity_bytes(
        record.column(
            row,
            "target_owner_function_id",
            "standard reference target function owner must be null or 16 bytes",
        )?,
        &record,
        "standard reference target function owner must be null or 16 bytes",
    )?
    .map(FunctionId::from_bytes);
    let target_standard_library_revision = optional_identity_bytes(
        record.column(
            row,
            "target_standard_library_revision_id",
            "standard reference target standard library revision identity must be null or 16 bytes",
        )?,
        &record,
        "standard reference target standard library revision identity must be null or 16 bytes",
    )?
    .map(StandardLibraryRevisionId::from_bytes);
    let target_kind: String = record.column(
        row,
        "target_kind",
        "standard reference target kind must decode",
    )?;
    let target = match (
        target_kind.as_str(),
        owner_type,
        owner_function,
        target_standard_library_revision,
    ) {
        ("object_type", None, None, None) => {
            DefinitionReferenceTarget::ObjectType(TypeId::from_bytes(target_bytes))
        }
        ("field", Some(owner), None, None) => DefinitionReferenceTarget::Field {
            owner,
            field: FieldId::from_bytes(target_bytes),
        },
        ("function", None, None, None) => {
            DefinitionReferenceTarget::Function(FunctionId::from_bytes(target_bytes))
        }
        ("parameter", None, Some(owner), None) => DefinitionReferenceTarget::Parameter {
            owner,
            parameter: ParameterId::from_bytes(target_bytes),
        },
        ("value_type", None, None, Some(revision)) if revision == expected_standard => {
            DefinitionReferenceTarget::ValueType(TypeId::from_bytes(target_bytes))
        }
        ("expression", None, None, None) => {
            DefinitionReferenceTarget::Expression(ExpressionId::from_bytes(target_bytes))
        }
        _ => {
            return Err(record.invariant(
                "standard reference target kind and owner columns must form one exact owner-qualified target",
            ));
        }
    };
    let kind_name: String =
        record.column(row, "reference_kind", "standard reference kind must decode")?;
    let kind = decode_standard_reference_kind(&kind_name, &record)?;
    if !standard_reference_kind_matches_target(kind, target) {
        return Err(record
            .invariant("standard reference kind must be compatible with its exact target kind"));
    }
    let source_origin = decode_standard_reference_origin(row, &record)?;
    Ok(DefinitionReference::new(
        source_function,
        source_revision,
        ordinal,
        target,
        kind,
        source_origin,
    ))
}

fn decode_standard_reference_kind(
    name: &str,
    record: &DurableRecord,
) -> Result<DefinitionReferenceKind, PostgresKernelError> {
    exact_enum(
        name,
        STANDARD_REFERENCE_KINDS,
        record,
        "standard reference kind must be one exact supported semantic relation",
    )
}

const STANDARD_REFERENCE_KINDS: &[(&str, DefinitionReferenceKind)] = &[
    ("function_call", DefinitionReferenceKind::FunctionCall),
    ("named_type", DefinitionReferenceKind::NamedType),
    ("object_reference", DefinitionReferenceKind::ObjectReference),
    ("parameter_read", DefinitionReferenceKind::ParameterRead),
    ("query_object", DefinitionReferenceKind::QueryObject),
    ("query_field", DefinitionReferenceKind::QueryField),
    ("expression", DefinitionReferenceKind::Expression),
    ("write_object", DefinitionReferenceKind::WriteObject),
    ("write_field", DefinitionReferenceKind::WriteField),
];

const fn standard_reference_kind_matches_target(
    kind: DefinitionReferenceKind,
    target: DefinitionReferenceTarget,
) -> bool {
    matches!(
        (kind, target),
        (
            DefinitionReferenceKind::FunctionCall,
            DefinitionReferenceTarget::Function(_)
        ) | (
            DefinitionReferenceKind::NamedType
                | DefinitionReferenceKind::ObjectReference
                | DefinitionReferenceKind::QueryObject,
            DefinitionReferenceTarget::ObjectType(_)
        ) | (
            DefinitionReferenceKind::NamedType,
            DefinitionReferenceTarget::ValueType(_)
        ) | (
            DefinitionReferenceKind::ObjectReference
                | DefinitionReferenceKind::QueryObject
                | DefinitionReferenceKind::QueryField,
            DefinitionReferenceTarget::Field { .. }
        ) | (
            DefinitionReferenceKind::ParameterRead,
            DefinitionReferenceTarget::Parameter { .. }
        ) | (
            DefinitionReferenceKind::Expression,
            DefinitionReferenceTarget::Expression(_)
        ) | (
            DefinitionReferenceKind::WriteObject,
            DefinitionReferenceTarget::ObjectType(_)
        ) | (
            DefinitionReferenceKind::WriteField,
            DefinitionReferenceTarget::Field { .. }
        )
    )
}

fn decode_standard_reference_origin(
    row: &Row,
    record: &DurableRecord,
) -> Result<SourceOrigin, PostgresKernelError> {
    let unit = SourceUnitId::from_bytes(identity_bytes(
        record.column(
            row,
            "source_unit_id",
            "standard reference source unit identity must be 16 bytes",
        )?,
        record,
        "standard reference source unit identity must be 16 bytes",
    )?);
    let start = u32_from_i64(
        record.column(
            row,
            "source_start",
            "standard reference source start must fit u32",
        )?,
        record,
        "standard reference source start must fit u32",
    )?;
    let end = u32_from_i64(
        record.column(
            row,
            "source_end",
            "standard reference source end must fit u32",
        )?,
        record,
        "standard reference source end must fit u32",
    )?;
    SourceOrigin::new(unit, start, end).map_err(PostgresKernelError::RevisionInvariant)
}

/// Aligns the recovered immutable revisions and references one-to-one with the
/// recovered standard catalogue functions and validates the complete
/// executable sequence before the canonical digest is verified.
fn build_standard_executables(
    catalogue: &CatalogueSnapshot,
    revisions: Vec<FunctionRevisionRecord>,
    references: Vec<DefinitionReference>,
) -> Result<Vec<StandardExecutable>, PostgresKernelError> {
    let relation = "_orna_kernel.standard_function_revisions";
    for revision in &revisions {
        if catalogue.function_by_id(revision.function()).is_none() {
            return Err(
                DurableRecord::new(relation, revision.id().canonical()).invariant(
                    "standard function revision must identify one standard catalogue function",
                ),
            );
        }
    }
    let mut executables = Vec::with_capacity(catalogue.functions().len());
    let mut consumed_references = 0usize;
    for function in catalogue.functions() {
        let mut owned = revisions
            .iter()
            .filter(|revision| revision.function() == function.id());
        let revision = owned.next().ok_or_else(|| {
            DurableRecord::new(relation, function.id().canonical()).invariant(
                "standard catalogue function must own exactly one current function revision",
            )
        })?;
        if owned.next().is_some() {
            return Err(
                DurableRecord::new(relation, function.id().canonical()).invariant(
                    "standard catalogue function must own exactly one current function revision",
                ),
            );
        }
        if revision.id() != function.current_revision() {
            return Err(DurableRecord::new(relation, revision.id().canonical()).invariant(
                "standard catalogue function current revision must equal its recovered revision",
            ));
        }
        let mut owned_references = references
            .iter()
            .filter(|reference| {
                reference.source_function() == function.id()
                    && reference.source_revision() == revision.id()
            })
            .cloned()
            .collect::<Vec<_>>();
        owned_references.sort_by_key(|reference| reference.ordinal());
        for (index, reference) in owned_references.iter().enumerate() {
            let expected = u32::try_from(index).map_err(|_| {
                DurableRecord::new(relation, revision.id().canonical())
                    .invariant("standard executable reference ordinal count must fit u32")
            })?;
            if reference.ordinal() != expected {
                return Err(DurableRecord::new(
                    "_orna_kernel.standard_definition_references",
                    revision.id().canonical(),
                )
                .invariant("standard executable reference ordinals must be contiguous from zero"));
            }
        }
        consumed_references += owned_references.len();
        executables.push(
            StandardExecutable::new(function.id(), revision.clone(), owned_references)
                .map_err(PostgresKernelError::RevisionInvariant)?,
        );
    }
    if consumed_references != references.len() {
        return Err(DurableRecord::new(
            "_orna_kernel.standard_definition_references",
            "unowned".to_owned(),
        )
        .invariant(
            "standard definition reference must belong to one recovered executable revision",
        ));
    }
    if executables.is_empty() {
        return Err(DurableRecord::new(
            "_orna_kernel.standard_library_revisions",
            catalogue.revision().canonical(),
        )
        .invariant("version-two standard snapshot must carry at least one executable"));
    }
    Ok(executables)
}

async fn load_standard_schemas(
    transaction: &Transaction<'_>,
    standard: StandardLibraryRevisionId,
) -> Result<Vec<RecoveredStandardSchema>, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.standard_catalogue_schemas";
    let rows = transaction
        .query(
            "SELECT standard_library_revision_id, schema_id, name_parts,
                    source_unit_id, source_start, source_end
             FROM _orna_kernel.standard_catalogue_schemas
             WHERE standard_library_revision_id = $1
             ORDER BY schema_id",
            &[&standard.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    rows.iter()
        .enumerate()
        .map(|(index, row)| decode_standard_schema(row, index, standard, RELATION))
        .collect()
}

fn decode_standard_schema(
    row: &Row,
    index: usize,
    expected_standard: StandardLibraryRevisionId,
    relation: &'static str,
) -> Result<RecoveredStandardSchema, PostgresKernelError> {
    let row_record = DurableRecord::new(relation, format!("row={index}"));
    require_standard_library_revision(row, &row_record, expected_standard, "schema")?;
    let id = SchemaId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "schema_id",
            "standard schema identity must be 16 bytes",
        )?,
        &row_record,
        "standard schema identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(relation, id.canonical());
    let name_parts: Vec<String> = record.column(
        row,
        "name_parts",
        "standard schema name parts must be an exact PostgreSQL text array",
    )?;
    let name = QualifiedSemanticName::new(name_parts).map_err(|_| {
        record.invariant("standard schema name parts must form one exact semantic name")
    })?;
    let origin = decode_origin(row, &record, DefinitionIdentity::Schema(id))?;
    Ok(RecoveredStandardSchema {
        definition: SchemaDefinition::new(id, name),
        origin,
    })
}

async fn load_standard_value_types(
    transaction: &Transaction<'_>,
    standard: StandardLibraryRevisionId,
) -> Result<Vec<RecoveredStandardValueType>, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.standard_catalogue_value_types";
    let rows = transaction
        .query(
            "SELECT standard_library_revision_id, type_id, schema_id, name_parts,
                    value_kind, mutability, persistence, representation_contract,
                    source_unit_id, source_start, source_end
             FROM _orna_kernel.standard_catalogue_value_types
             WHERE standard_library_revision_id = $1
             ORDER BY type_id",
            &[&standard.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    rows.iter()
        .enumerate()
        .map(|(index, row)| decode_standard_value_type(row, index, standard, RELATION))
        .collect()
}

fn decode_standard_value_type(
    row: &Row,
    index: usize,
    expected_standard: StandardLibraryRevisionId,
    relation: &'static str,
) -> Result<RecoveredStandardValueType, PostgresKernelError> {
    let row_record = DurableRecord::new(relation, format!("row={index}"));
    require_standard_library_revision(row, &row_record, expected_standard, "value type")?;
    let id = TypeId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "type_id",
            "standard value type identity must be 16 bytes",
        )?,
        &row_record,
        "standard value type identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(relation, id.canonical());
    let schema = SchemaId::from_bytes(identity_bytes(
        record.column(
            row,
            "schema_id",
            "standard value type schema identity must be 16 bytes",
        )?,
        &record,
        "standard value type schema identity must be 16 bytes",
    )?);
    let name_parts: Vec<String> = record.column(
        row,
        "name_parts",
        "standard value type name parts must be an exact PostgreSQL text array",
    )?;
    let name = QualifiedSemanticName::new(name_parts).map_err(|_| {
        record.invariant("standard value type name parts must form one exact semantic name")
    })?;
    let value_kind: String = record.column(
        row,
        "value_kind",
        "standard value type kind must be primitive or opaque",
    )?;
    let kind = exact_enum(
        &value_kind,
        &[
            ("primitive", ValueTypeKind::Primitive),
            ("opaque", ValueTypeKind::Opaque),
        ],
        &record,
        "standard value type kind must be primitive or opaque",
    )?;
    let mutability: String = record.column(
        row,
        "mutability",
        "standard value type mutability must be immutable",
    )?;
    exact_enum(
        &mutability,
        &[("immutable", ValueTypeMutability::Immutable)],
        &record,
        "standard value type mutability must be immutable",
    )?;
    let persistence_name: String = record.column(
        row,
        "persistence",
        "standard value type persistence must be persistable or transient",
    )?;
    let persistence = exact_enum(
        &persistence_name,
        &[
            ("persistable", ValueTypePersistence::Persistable),
            ("transient", ValueTypePersistence::Transient),
        ],
        &record,
        "standard value type persistence must be persistable or transient",
    )?;
    let representation_contract: String = record.column(
        row,
        "representation_contract",
        "standard value type representation contract must be PostgreSQL text",
    )?;
    let origin = decode_origin(row, &record, DefinitionIdentity::ValueType(id))?;
    Ok(RecoveredStandardValueType {
        schema,
        definition: recovered_standard_value_definition(
            &record,
            id,
            name,
            kind,
            persistence,
            representation_contract,
        )?,
        origin,
    })
}

pub(super) fn recovered_standard_value_definition(
    record: &DurableRecord,
    id: TypeId,
    name: QualifiedSemanticName,
    kind: ValueTypeKind,
    persistence: ValueTypePersistence,
    representation_contract: String,
) -> Result<ValueTypeDefinition, PostgresKernelError> {
    if representation_contract.is_empty() {
        return Err(
            record.invariant("standard value type representation contract must not be empty")
        );
    }
    match kind {
        ValueTypeKind::Primitive => Ok(ValueTypeDefinition::primitive(
            id,
            name,
            ValueTypeMutability::Immutable,
            persistence,
            representation_contract,
        )),
        ValueTypeKind::Opaque => {
            if persistence != ValueTypePersistence::Transient {
                return Err(record.invariant("standard opaque value type must be transient"));
            }
            if representation_contract.len() > 128
                || !representation_contract
                    .bytes()
                    .all(|byte| (0x20..=0x7e).contains(&byte))
            {
                return Err(record.invariant(
                    "standard opaque value type contract must be 1 to 128 printable ASCII bytes",
                ));
            }
            Ok(ValueTypeDefinition::opaque(
                id,
                name,
                representation_contract,
            ))
        }
        _ => Err(record.invariant("standard value type kind is not recoverable")),
    }
}

async fn load_standard_enum_types(
    transaction: &Transaction<'_>,
    standard: StandardLibraryRevisionId,
) -> Result<Vec<RecoveredStandardEnumType>, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.standard_catalogue_enum_types";
    let rows = transaction
        .query(
            "SELECT standard_library_revision_id, type_id, schema_id, name_parts, labels,
                    source_unit_id, source_start, source_end
             FROM _orna_kernel.standard_catalogue_enum_types
             WHERE standard_library_revision_id = $1
             ORDER BY type_id",
            &[&standard.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    rows.iter()
        .enumerate()
        .map(|(index, row)| decode_standard_enum_type(row, index, standard, RELATION))
        .collect()
}

fn decode_standard_enum_type(
    row: &Row,
    index: usize,
    expected_standard: StandardLibraryRevisionId,
    relation: &'static str,
) -> Result<RecoveredStandardEnumType, PostgresKernelError> {
    let row_record = DurableRecord::new(relation, format!("row={index}"));
    require_standard_library_revision(row, &row_record, expected_standard, "enum type")?;
    let id = TypeId::from_bytes(identity_bytes(
        row_record.column(row, "type_id", "standard enum identity must be 16 bytes")?,
        &row_record,
        "standard enum identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(relation, id.canonical());
    let schema = SchemaId::from_bytes(identity_bytes(
        record.column(
            row,
            "schema_id",
            "standard enum schema identity must be 16 bytes",
        )?,
        &record,
        "standard enum schema identity must be 16 bytes",
    )?);
    let name_parts: Vec<String> = record.column(
        row,
        "name_parts",
        "standard enum name parts must be an exact PostgreSQL text array",
    )?;
    let name = QualifiedSemanticName::new(name_parts).map_err(|_| {
        record.invariant("standard enum name parts must form one exact semantic name")
    })?;
    let labels: Vec<String> = record.column(
        row,
        "labels",
        "standard enum labels must be one exact PostgreSQL text array",
    )?;
    let origin = decode_origin(row, &record, DefinitionIdentity::ValueType(id))?;
    Ok(RecoveredStandardEnumType {
        schema,
        definition: EnumTypeDefinition::new(id, name, labels),
        origin,
    })
}

async fn load_standard_type_bindings(
    transaction: &Transaction<'_>,
    standard: StandardLibraryRevisionId,
) -> Result<Vec<RecoveredStandardTypeBinding>, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.standard_catalogue_type_bindings";
    let rows = transaction
        .query(
            "SELECT standard_library_revision_id, type_binding_id, kind, name_parts,
                    target_type_kind, target_type_id, target_enum_type_id,
                    source_unit_id, source_start, source_end
             FROM _orna_kernel.standard_catalogue_type_bindings
             WHERE standard_library_revision_id = $1
             ORDER BY type_binding_id",
            &[&standard.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    rows.iter()
        .enumerate()
        .map(|(index, row)| decode_standard_type_binding(row, index, standard, RELATION))
        .collect()
}

fn decode_standard_type_binding(
    row: &Row,
    index: usize,
    expected_standard: StandardLibraryRevisionId,
    relation: &'static str,
) -> Result<RecoveredStandardTypeBinding, PostgresKernelError> {
    let row_record = DurableRecord::new(relation, format!("row={index}"));
    require_standard_library_revision(row, &row_record, expected_standard, "type binding")?;
    let id = TypeBindingId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "type_binding_id",
            "standard type binding identity must be 16 bytes",
        )?,
        &row_record,
        "standard type binding identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(relation, id.canonical());
    let kind_name: String = record.column(
        row,
        "kind",
        "standard type binding kind must be qualified or prelude",
    )?;
    let kind = exact_enum(
        &kind_name,
        &[
            ("qualified", TypeBindingKind::Qualified),
            ("prelude", TypeBindingKind::Prelude),
        ],
        &record,
        "standard type binding kind must be qualified or prelude",
    )?;
    let name_parts: Vec<String> = record.column(
        row,
        "name_parts",
        "standard type binding name parts must be an exact PostgreSQL text array",
    )?;
    let target_kind: String = record.column(
        row,
        "target_type_kind",
        "standard type binding target kind must be value or enum",
    )?;
    let value_target = optional_identity_bytes(
        record.column(
            row,
            "target_type_id",
            "standard type binding value target must be null or 16 bytes",
        )?,
        &record,
        "standard type binding value target must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    let enum_target = optional_identity_bytes(
        record.column(
            row,
            "target_enum_type_id",
            "standard type binding enum target must be null or 16 bytes",
        )?,
        &record,
        "standard type binding enum target must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    let target = decode_standard_binding_target(&target_kind, value_target, enum_target, &record)?;
    let binding = match kind {
        TypeBindingKind::Qualified => {
            let name = QualifiedSemanticName::new(name_parts).map_err(|_| {
                record.invariant(
                    "qualified standard type binding name must form one exact semantic name",
                )
            })?;
            TypeBinding::qualified(name, target).map_err(|_| {
                record.invariant("qualified standard type binding name must include a schema")
            })?
        }
        TypeBindingKind::Prelude => {
            let name = PreludeTypeName::new(name_parts).map_err(|_| {
                record.invariant("prelude standard type binding name must form exact keyword words")
            })?;
            TypeBinding::prelude(name, target).map_err(|_| {
                record.invariant(
                    "prelude standard type binding name must derive one binding identity",
                )
            })?
        }
        _ => {
            return Err(record.invariant("standard type binding kind must be qualified or prelude"));
        }
    };
    if binding.id() != id {
        return Err(record.invariant(
            "standard type binding identity must equal the identity derived from its kind and name",
        ));
    }
    let origin = decode_origin(row, &record, DefinitionIdentity::TypeBinding(id))?;
    Ok(RecoveredStandardTypeBinding { binding, origin })
}

pub(super) fn decode_standard_binding_target(
    kind: &str,
    value_target: Option<TypeId>,
    enum_target: Option<TypeId>,
    record: &DurableRecord,
) -> Result<TypeId, PostgresKernelError> {
    match (kind, value_target, enum_target) {
        ("value", Some(target), None) | ("enum", None, Some(target)) => Ok(target),
        _ => Err(record.invariant(
            "standard type binding target kind and identities must form one exact value or enum tuple",
        )),
    }
}

fn require_standard_library_revision(
    row: &Row,
    record: &DurableRecord,
    expected: StandardLibraryRevisionId,
    member: &'static str,
) -> Result<(), PostgresKernelError> {
    let standard = StandardLibraryRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "standard_library_revision_id",
            "standard catalogue member revision identity must be 16 bytes",
        )?,
        record,
        "standard catalogue member revision identity must be 16 bytes",
    )?);
    if standard != expected {
        return Err(record.invariant(match member {
            "schema" => "standard schema must belong to the selected standard library revision",
            "value type" => {
                "standard value type must belong to the selected standard library revision"
            }
            "enum type" => {
                "standard enum type must belong to the selected standard library revision"
            }
            "function" => {
                "standard function must belong to the selected standard library revision"
            }
            "parameter" => {
                "standard parameter must belong to the selected standard library revision"
            }
            "function revision" => {
                "standard function revision must belong to the selected standard library revision"
            }
            "function artifact" => {
                "standard function artifact must belong to the selected standard library revision"
            }
            "definition reference" => {
                "standard definition reference must belong to the selected standard library revision"
            }
            _ => "standard type binding must belong to the selected standard library revision",
        }));
    }
    Ok(())
}
