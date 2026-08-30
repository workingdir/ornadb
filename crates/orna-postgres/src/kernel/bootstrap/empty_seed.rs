//! Canonical empty revision seeding and legacy empty-state repair.

use super::*;

struct CanonicalEmptyHashes {
    bundle: Vec<u8>,
    source: Vec<u8>,
    catalogue: Vec<u8>,
}

fn canonical_empty_hashes(
    bundle: SourceBundleId,
    catalogue: CatalogueRevisionId,
) -> Result<CanonicalEmptyHashes, PostgresKernelError> {
    let bundle_hash = source_bundle_digest(&[]).map_err(|_| {
        PostgresKernelError::CatalogueInvariant(
            "cannot calculate the canonical empty source bundle hash",
        )
    })?;
    let source_hash = source_revision_record_digest(bundle, None, bundle_hash).map_err(|_| {
        PostgresKernelError::CatalogueInvariant(
            "cannot calculate the canonical empty source revision hash",
        )
    })?;
    let empty_catalogue =
        CatalogueSnapshot::new(catalogue, Vec::new(), Vec::new()).map_err(|_| {
            PostgresKernelError::CatalogueInvariant(
                "cannot construct the canonical empty catalogue",
            )
        })?;
    let catalogue_hash = catalogue_digest(&empty_catalogue, &[], &[], &[], &[]).map_err(|_| {
        PostgresKernelError::CatalogueInvariant(
            "cannot calculate the canonical empty catalogue hash",
        )
    })?;

    Ok(CanonicalEmptyHashes {
        bundle: bundle_hash.to_bytes().to_vec(),
        source: source_hash.to_bytes().to_vec(),
        catalogue: catalogue_hash.to_bytes().to_vec(),
    })
}

struct EmptyRevisionState {
    bundle: SourceBundleId,
    source: SourceRevisionId,
    catalogue: CatalogueRevisionId,
    bundle_hash: Vec<u8>,
    source_hash: Vec<u8>,
    catalogue_hash: Vec<u8>,
}

pub(super) async fn rewrite_legacy_empty_hashes(
    transaction: &Transaction<'_>,
) -> Result<(), PostgresKernelError> {
    let Some(legacy) = strict_empty_revision_state(transaction).await? else {
        return Ok(());
    };
    let legacy_hash = Sha256::digest([]).to_vec();
    require_empty_revision_hashes(
        &legacy,
        &legacy_hash,
        &legacy_hash,
        &legacy_hash,
        "unsupported legacy aggregate hash",
    )?;

    let canonical = canonical_empty_hashes(legacy.bundle, legacy.catalogue)?;
    if canonical.bundle == legacy_hash
        || canonical.source == legacy_hash
        || canonical.catalogue == legacy_hash
    {
        return Err(PostgresKernelError::CatalogueInvariant(
            "canonical empty hash migration computed a legacy aggregate hash",
        ));
    }
    let bundle_bytes = legacy.bundle.to_bytes().to_vec();
    let source_bytes = legacy.source.to_bytes().to_vec();
    let catalogue_bytes = legacy.catalogue.to_bytes().to_vec();
    let updated = transaction
        .execute(
            "UPDATE _orna_kernel.source_bundles
             SET content_hash = $2
             WHERE id = $1
               AND content_hash = $3
               AND hash_algorithm = 'sha256'
               AND hash_contract_version = 1",
            &[&bundle_bytes, &canonical.bundle, &legacy_hash],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    require_one_hash_rewrite(updated)?;
    let updated = transaction
        .execute(
            "UPDATE _orna_kernel.source_revisions
             SET content_hash = $2
             WHERE id = $1
               AND content_hash = $3
               AND hash_algorithm = 'sha256'
               AND hash_contract_version = 1",
            &[&source_bytes, &canonical.source, &legacy_hash],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    require_one_hash_rewrite(updated)?;
    let updated = transaction
        .execute(
            "UPDATE _orna_kernel.catalogue_revisions
             SET content_hash = $2
             WHERE id = $1
               AND content_hash = $3
               AND hash_algorithm = 'sha256'
               AND hash_contract_version = 1",
            &[&catalogue_bytes, &canonical.catalogue, &legacy_hash],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    require_one_hash_rewrite(updated)?;

    let postcondition = strict_empty_revision_state(transaction).await?.ok_or(
        PostgresKernelError::CatalogueInvariant(
            "canonical empty hash migration lost the active revision",
        ),
    )?;
    require_empty_revision_hashes(
        &postcondition,
        &canonical.bundle,
        &canonical.source,
        &canonical.catalogue,
        "canonical empty hash migration postcondition failed",
    )?;
    if postcondition.bundle_hash == legacy_hash
        || postcondition.source_hash == legacy_hash
        || postcondition.catalogue_hash == legacy_hash
    {
        return Err(PostgresKernelError::CatalogueInvariant(
            "canonical empty hash migration retained a legacy aggregate hash",
        ));
    }

    Ok(())
}

fn require_one_hash_rewrite(updated: u64) -> Result<(), PostgresKernelError> {
    if updated == 1 {
        Ok(())
    } else {
        Err(PostgresKernelError::CatalogueInvariant(
            "canonical empty hash migration could not rewrite one exact legacy aggregate",
        ))
    }
}

fn require_empty_revision_hashes(
    state: &EmptyRevisionState,
    expected_bundle: &[u8],
    expected_source: &[u8],
    expected_catalogue: &[u8],
    message: &'static str,
) -> Result<(), PostgresKernelError> {
    if state.bundle_hash == expected_bundle
        && state.source_hash == expected_source
        && state.catalogue_hash == expected_catalogue
    {
        Ok(())
    } else {
        Err(PostgresKernelError::CatalogueInvariant(message))
    }
}

async fn strict_empty_revision_state(
    transaction: &Transaction<'_>,
) -> Result<Option<EmptyRevisionState>, PostgresKernelError> {
    let counts = transaction
        .query_one(
            "SELECT
                (SELECT count(*) FROM _orna_kernel.source_bundles) AS bundles,
                (SELECT count(*) FROM _orna_kernel.source_units) AS source_units,
                (SELECT count(*) FROM _orna_kernel.source_revisions) AS source_revisions,
                (SELECT count(*) FROM _orna_kernel.catalogue_revisions) AS catalogue_revisions,
                (SELECT count(*) FROM _orna_kernel.active_revision) AS active_revisions,
                (SELECT count(*) FROM _orna_kernel.catalogue_schemas) AS schemas,
                (SELECT count(*) FROM _orna_kernel.catalogue_object_types) AS object_types,
                (SELECT count(*) FROM _orna_kernel.catalogue_fields) AS fields,
                (SELECT count(*) FROM _orna_kernel.catalogue_expressions) AS expressions,
                (SELECT count(*) FROM _orna_kernel.catalogue_functions) AS functions,
                (SELECT count(*) FROM _orna_kernel.catalogue_function_parameters) AS parameters,
                (SELECT count(*) FROM _orna_kernel.catalogue_function_return_columns) AS return_columns,
                (SELECT count(*) FROM _orna_kernel.function_revisions) AS function_revisions,
                (SELECT count(*) FROM _orna_kernel.function_artifacts) AS function_artifacts,
                (SELECT count(*) FROM _orna_kernel.definition_references) AS references,
                (SELECT count(*)
                 FROM pg_class AS relation
                 JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
                 WHERE namespace.nspname = '_orna_data'
                   AND relation.relkind IN ('r', 'p')) AS data_relations",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    // Migration 4 runs before migration 43 creates the membership table, so
    // account for the relation only when it exists.
    let source_bundle_units: i64 = if transaction
        .query_one(
            "SELECT to_regclass('_orna_kernel.source_bundle_units') IS NOT NULL",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?
        .get(0)
    {
        transaction
            .query_one("SELECT count(*) FROM _orna_kernel.source_bundle_units", &[])
            .await
            .map_err(PostgresKernelError::Database)?
            .get(0)
    } else {
        0
    };

    let count = |column| counts.get::<_, i64>(column);
    let fresh = [
        "bundles",
        "source_units",
        "source_revisions",
        "catalogue_revisions",
        "active_revisions",
        "schemas",
        "object_types",
        "fields",
        "expressions",
        "functions",
        "parameters",
        "return_columns",
        "function_revisions",
        "function_artifacts",
        "references",
        "data_relations",
    ]
    .iter()
    .all(|column| count(*column) == 0)
        && source_bundle_units == 0;
    if fresh {
        return Ok(None);
    }

    let supported_legacy_empty = count("bundles") == 1
        && count("source_units") == 0
        && count("source_revisions") == 1
        && count("catalogue_revisions") == 1
        && count("active_revisions") == 1
        && count("schemas") == 0
        && count("object_types") == 0
        && count("fields") == 0
        && count("expressions") == 0
        && count("functions") == 0
        && count("parameters") == 0
        && count("return_columns") == 0
        && count("function_revisions") == 0
        && count("function_artifacts") == 0
        && count("references") == 0
        && count("data_relations") == 0
        && source_bundle_units == 0;
    if !supported_legacy_empty {
        return Err(PostgresKernelError::CatalogueInvariant(
            "canonical hash migration only supports a fresh or empty legacy catalogue",
        ));
    }

    let row = transaction
        .query_opt(
            "SELECT
                bundle.id AS bundle_id,
                bundle.content_hash AS bundle_hash,
                bundle.hash_algorithm AS bundle_algorithm,
                bundle.hash_contract_version AS bundle_contract_version,
                source.id AS source_id,
                source.parent_source_revision_id AS source_parent_id,
                source.bundle_id AS source_bundle_id,
                source.content_hash AS source_hash,
                source.hash_algorithm AS source_algorithm,
                source.hash_contract_version AS source_contract_version,
                catalogue.id AS catalogue_id,
                catalogue.source_revision_id AS catalogue_source_id,
                catalogue.parent_catalogue_revision_id AS catalogue_parent_id,
                catalogue.content_hash AS catalogue_hash,
                catalogue.hash_algorithm AS catalogue_algorithm,
                catalogue.hash_contract_version AS catalogue_contract_version,
                active.source_revision_id AS active_source_id,
                active.catalogue_revision_id AS active_catalogue_id
             FROM _orna_kernel.source_bundles AS bundle
             JOIN _orna_kernel.source_revisions AS source ON source.bundle_id = bundle.id
             JOIN _orna_kernel.catalogue_revisions AS catalogue
               ON catalogue.source_revision_id = source.id
             JOIN _orna_kernel.active_revision AS active
               ON active.source_revision_id = source.id
              AND active.catalogue_revision_id = catalogue.id
             FOR UPDATE OF bundle, source, catalogue, active",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?
        .ok_or(PostgresKernelError::CatalogueInvariant(
            "canonical hash migration found an unsupported legacy revision graph",
        ))?;

    let bundle_bytes: Vec<u8> = row
        .try_get("bundle_id")
        .map_err(PostgresKernelError::Database)?;
    let source_bytes: Vec<u8> = row
        .try_get("source_id")
        .map_err(PostgresKernelError::Database)?;
    let catalogue_bytes: Vec<u8> = row
        .try_get("catalogue_id")
        .map_err(PostgresKernelError::Database)?;
    let bundle = SourceBundleId::from_bytes(exact_id_bytes(
        bundle_bytes.clone(),
        "canonical hash migration found a non-16-byte source bundle identity",
    )?);
    let source = SourceRevisionId::from_bytes(exact_id_bytes(
        source_bytes.clone(),
        "canonical hash migration found a non-16-byte source revision identity",
    )?);
    let catalogue = CatalogueRevisionId::from_bytes(exact_id_bytes(
        catalogue_bytes.clone(),
        "canonical hash migration found a non-16-byte catalogue revision identity",
    )?);
    let no_parent: Option<Vec<u8>> = row
        .try_get("source_parent_id")
        .map_err(PostgresKernelError::Database)?;
    let no_catalogue_parent: Option<Vec<u8>> = row
        .try_get("catalogue_parent_id")
        .map_err(PostgresKernelError::Database)?;
    let source_bundle: Vec<u8> = row
        .try_get("source_bundle_id")
        .map_err(PostgresKernelError::Database)?;
    let catalogue_source: Vec<u8> = row
        .try_get("catalogue_source_id")
        .map_err(PostgresKernelError::Database)?;
    let active_source: Vec<u8> = row
        .try_get("active_source_id")
        .map_err(PostgresKernelError::Database)?;
    let active_catalogue: Vec<u8> = row
        .try_get("active_catalogue_id")
        .map_err(PostgresKernelError::Database)?;
    let bundle_algorithm: String = row
        .try_get("bundle_algorithm")
        .map_err(PostgresKernelError::Database)?;
    let source_algorithm: String = row
        .try_get("source_algorithm")
        .map_err(PostgresKernelError::Database)?;
    let catalogue_algorithm: String = row
        .try_get("catalogue_algorithm")
        .map_err(PostgresKernelError::Database)?;
    let bundle_contract_version: i16 = row
        .try_get("bundle_contract_version")
        .map_err(PostgresKernelError::Database)?;
    let source_contract_version: i16 = row
        .try_get("source_contract_version")
        .map_err(PostgresKernelError::Database)?;
    let catalogue_contract_version: i16 = row
        .try_get("catalogue_contract_version")
        .map_err(PostgresKernelError::Database)?;
    if no_parent.is_some()
        || no_catalogue_parent.is_some()
        || source_bundle != bundle_bytes
        || catalogue_source != source_bytes
        || active_source != source_bytes
        || active_catalogue != catalogue_bytes
        || bundle_algorithm != "sha256"
        || source_algorithm != "sha256"
        || catalogue_algorithm != "sha256"
        || bundle_contract_version != 1
        || source_contract_version != 1
        || catalogue_contract_version != 1
    {
        return Err(PostgresKernelError::CatalogueInvariant(
            "canonical hash migration found an unsupported legacy revision graph",
        ));
    }

    Ok(Some(EmptyRevisionState {
        bundle,
        source,
        catalogue,
        bundle_hash: row
            .try_get("bundle_hash")
            .map_err(PostgresKernelError::Database)?,
        source_hash: row
            .try_get("source_hash")
            .map_err(PostgresKernelError::Database)?,
        catalogue_hash: row
            .try_get("catalogue_hash")
            .map_err(PostgresKernelError::Database)?,
    }))
}

pub(super) async fn load_or_seed_active_revision(
    transaction: &Transaction<'_>,
) -> Result<ActiveRevision, PostgresKernelError> {
    let active = transaction
        .query_opt(
            "SELECT source_revision_id, catalogue_revision_id
             FROM _orna_kernel.active_revision
             WHERE singleton = true",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    if let Some(row) = active {
        let active = active_from_row(&row)?;
        let recovered = recover_active_revision(transaction).await?;
        let pair = recovered.pair();
        if pair.source() != active.source || pair.catalogue() != active.catalogue {
            return Err(PostgresKernelError::CatalogueInvariant(
                "recovered active revision does not match the active revision pointer",
            ));
        }
        return Ok(active);
    }

    let counts = transaction
        .query_one(
            "SELECT
                (SELECT count(*) FROM _orna_kernel.source_bundles),
                (SELECT count(*) FROM _orna_kernel.source_revisions),
                (SELECT count(*) FROM _orna_kernel.catalogue_revisions)",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let durable_rows = counts.get::<_, i64>(0) + counts.get::<_, i64>(1) + counts.get::<_, i64>(2);
    if durable_rows != 0 {
        return Err(PostgresKernelError::CatalogueInvariant(
            "durable revisions exist without an active revision pointer",
        ));
    }

    let bundle = SourceBundleId::new();
    let source = SourceRevisionId::new();
    let catalogue = CatalogueRevisionId::new();
    let bundle_bytes = bundle.to_bytes().to_vec();
    let source_bytes = source.to_bytes().to_vec();
    let catalogue_bytes = catalogue.to_bytes().to_vec();
    let canonical_hashes = canonical_empty_hashes(bundle, catalogue)?;

    transaction
        .execute(
            "INSERT INTO _orna_kernel.source_bundles (id, content_hash) VALUES ($1, $2)",
            &[&bundle_bytes, &canonical_hashes.bundle],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    transaction
        .execute(
            "INSERT INTO _orna_kernel.source_revisions (id, bundle_id, content_hash)
             VALUES ($1, $2, $3)",
            &[&source_bytes, &bundle_bytes, &canonical_hashes.source],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    transaction
        .execute(
            "INSERT INTO _orna_kernel.catalogue_revisions
                (id, source_revision_id, content_hash)
             VALUES ($1, $2, $3)",
            &[&catalogue_bytes, &source_bytes, &canonical_hashes.catalogue],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    for function in [
        SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID,
        SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID,
        SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID,
    ] {
        let function_bytes = function.to_bytes().to_vec();
        transaction
            .execute(
                "INSERT INTO _orna_kernel.invocation_target_authorities
                    (catalogue_revision_id, function_id, target_class,
                     function_revision_id, standard_library_revision_id)
                 VALUES ($1, $2, 'system', $2, NULL)",
                &[&catalogue_bytes, &function_bytes],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }

    transaction
        .execute(
            "INSERT INTO _orna_kernel.active_revision
                (singleton, source_revision_id, catalogue_revision_id)
             VALUES (true, $1, $2)",
            &[&source_bytes, &catalogue_bytes],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    Ok(ActiveRevision { source, catalogue })
}

fn active_from_row(row: &Row) -> Result<ActiveRevision, PostgresKernelError> {
    Ok(ActiveRevision {
        source: SourceRevisionId::from_bytes(exact_id_bytes(
            row.get::<_, Vec<u8>>("source_revision_id"),
            "active source revision identity is not 16 bytes",
        )?),
        catalogue: CatalogueRevisionId::from_bytes(exact_id_bytes(
            row.get::<_, Vec<u8>>("catalogue_revision_id"),
            "active catalogue revision identity is not 16 bytes",
        )?),
    })
}

fn exact_id_bytes(bytes: Vec<u8>, message: &'static str) -> Result<[u8; 16], PostgresKernelError> {
    bytes
        .try_into()
        .map_err(|_| PostgresKernelError::CatalogueInvariant(message))
}
