//! Active revision recovery transaction and ancestry validation.

use super::*;

pub(super) async fn recover_client(
    client: &mut Client,
) -> Result<ActiveDatabaseRevision, PostgresKernelError> {
    let transaction = client
        .build_transaction()
        .isolation_level(IsolationLevel::RepeatableRead)
        .read_only(true)
        .start()
        .await
        .map_err(PostgresKernelError::Database)?;

    let active = recover_active_revision(&transaction).await?;
    crate::security::recover_invocation_audit_events(&transaction, &active).await?;
    crate::inspect::recover_inspect_relations(&transaction, &active).await?;

    transaction
        .commit()
        .await
        .map_err(PostgresKernelError::Database)?;
    Ok(active)
}

pub(crate) async fn recover_active_revision(
    transaction: &Transaction<'_>,
) -> Result<ActiveDatabaseRevision, PostgresKernelError> {
    establish_trusted_search_path(transaction).await?;
    require_current_migrations(transaction).await?;
    let header = load_active_header(transaction).await?;
    let catalogue_hash_context = load_active_catalogue_hash_context(transaction, &header).await?;
    let active_ancestry =
        validate_revision_ancestry(transaction, header.catalogue, header.source).await?;
    validate_source_ancestry(transaction, &active_ancestry).await?;
    validate_catalogue_ancestry(transaction, header.catalogue, &active_ancestry).await?;
    let units = load_source_units(transaction, header.bundle).await?;
    let mut function_state = load_function_state(
        transaction,
        header.catalogue,
        &active_ancestry,
        &catalogue_hash_context,
    )
    .await?;
    let functions = std::mem::take(&mut function_state.functions);
    let function_origins = std::mem::take(&mut function_state.origins);
    let semantics = load_catalogue_semantics(
        transaction,
        header.catalogue,
        functions,
        function_origins,
        &catalogue_hash_context,
    )
    .await?;
    let active = assemble_revision(
        header,
        units,
        semantics,
        function_state,
        catalogue_hash_context,
    )?;
    verify_physical_catalogue(transaction, &active).await?;

    Ok(active)
}

async fn validate_revision_ancestry(
    transaction: &Transaction<'_>,
    active_catalogue: CatalogueRevisionId,
    active_source: SourceRevisionId,
) -> Result<BTreeSet<(CatalogueRevisionId, SourceRevisionId)>, PostgresKernelError> {
    let mut catalogue = active_catalogue;
    let mut source = active_source;
    let mut seen_catalogues = HashSet::new();
    let mut seen_sources = HashSet::new();
    let mut ancestry = BTreeSet::new();

    loop {
        let catalogue_record =
            DurableRecord::new("_orna_kernel.catalogue_revisions", catalogue.canonical());
        if !seen_catalogues.insert(catalogue) || !seen_sources.insert(source) {
            return Err(catalogue_record.invariant(
                "catalogue and source revision ancestry must terminate without repeated identities",
            ));
        }
        ancestry.insert((catalogue, source));

        let rows = transaction
            .query(
                "SELECT
                    catalogue.parent_catalogue_revision_id AS catalogue_parent_id,
                    source.parent_source_revision_id AS source_parent_id,
                    parent_catalogue.source_revision_id AS parent_catalogue_source_id
                 FROM _orna_kernel.catalogue_revisions AS catalogue
                 JOIN _orna_kernel.source_revisions AS source
                   ON source.id = catalogue.source_revision_id
                 LEFT JOIN _orna_kernel.catalogue_revisions AS parent_catalogue
                   ON parent_catalogue.id = catalogue.parent_catalogue_revision_id
                 WHERE catalogue.id = $1
                   AND source.id = $2",
                &[&catalogue.to_bytes().to_vec(), &source.to_bytes().to_vec()],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
        if rows.len() != 1 {
            return Err(catalogue_record.invariant(
                "each catalogue ancestor must join exactly one corresponding source revision",
            ));
        }

        let row = &rows[0];
        let catalogue_parent = optional_identity_bytes(
            catalogue_record.column(
                row,
                "catalogue_parent_id",
                "catalogue parent identity must be null or 16 bytes",
            )?,
            &catalogue_record,
            "catalogue parent identity must be null or 16 bytes",
        )?
        .map(CatalogueRevisionId::from_bytes);
        let source_record = DurableRecord::new("_orna_kernel.source_revisions", source.canonical());
        let source_parent = optional_identity_bytes(
            source_record.column(
                row,
                "source_parent_id",
                "source parent identity must be null or 16 bytes",
            )?,
            &source_record,
            "source parent identity must be null or 16 bytes",
        )?
        .map(SourceRevisionId::from_bytes);
        let parent_catalogue_source = optional_identity_bytes(
            catalogue_record.column(
                row,
                "parent_catalogue_source_id",
                "parent catalogue source identity must be null or 16 bytes",
            )?,
            &catalogue_record,
            "parent catalogue source identity must be null or 16 bytes",
        )?
        .map(SourceRevisionId::from_bytes);

        match (catalogue_parent, source_parent, parent_catalogue_source) {
            (None, None, None) => return Ok(ancestry),
            (Some(parent_catalogue), Some(parent_source), Some(joined_parent_source))
                if parent_source == joined_parent_source =>
            {
                catalogue = parent_catalogue;
                source = parent_source;
            }
            _ => {
                return Err(catalogue_record.invariant(
                    "each catalogue parent must exist and identify the corresponding source parent",
                ));
            }
        }
    }
}

async fn validate_source_ancestry(
    transaction: &Transaction<'_>,
    ancestry: &BTreeSet<(CatalogueRevisionId, SourceRevisionId)>,
) -> Result<(), PostgresKernelError> {
    let mut validated = HashSet::new();
    for (_, source) in ancestry {
        let mut path = HashSet::new();
        let mut current = *source;
        loop {
            let source_record = DurableRecord::new(SOURCE_REVISION_RELATION, current.canonical());
            if !path.insert(current) {
                return Err(source_record.invariant(
                    "source revision ancestry must terminate without repeated identities",
                ));
            }
            if !validated.insert(current) {
                break;
            }
            let Some(parent) = validate_source_revision(transaction, current).await? else {
                break;
            };
            current = parent;
        }
    }
    Ok(())
}

async fn validate_catalogue_ancestry(
    transaction: &Transaction<'_>,
    active_catalogue: CatalogueRevisionId,
    active_ancestry: &BTreeSet<(CatalogueRevisionId, SourceRevisionId)>,
) -> Result<(), PostgresKernelError> {
    for (catalogue, _) in active_ancestry {
        if *catalogue == active_catalogue {
            continue;
        }

        let record = DurableRecord::new(CATALOGUE_REVISION_RELATION, catalogue.canonical());
        let rows = transaction
            .query(
                "SELECT content_hash,
                        hash_algorithm,
                        hash_contract_version,
                        canonical_hash_version,
                        standard_library_revision_id
                 FROM _orna_kernel.catalogue_revisions
                 WHERE id = $1",
                &[&catalogue.to_bytes().to_vec()],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
        if rows.len() != 1 {
            return Err(record.invariant(
                "each reachable catalogue ancestor must have exactly one durable revision row",
            ));
        }

        let row = &rows[0];
        require_hash_contract(
            row,
            &record,
            "hash_algorithm",
            "hash_contract_version",
            "catalogue hash algorithm must be sha256",
            "catalogue hash contract version must be 1",
        )?;
        let hash_version = decode_catalogue_hash_version(
            record.column(
                row,
                "canonical_hash_version",
                "catalogue canonical hash version must be a supported smallint",
            )?,
            &record,
        )?;
        let standard_library_revision = optional_identity_bytes(
            record.column(
                row,
                "standard_library_revision_id",
                "catalogue standard library revision identity must be null or 16 bytes",
            )?,
            &record,
            "catalogue standard library revision identity must be null or 16 bytes",
        )?
        .map(StandardLibraryRevisionId::from_bytes);
        let verified_standard = match standard_library_revision {
            Some(revision) => Some(load_verified_standard_library(transaction, revision).await?),
            None => None,
        };
        let catalogue_hash_context = catalogue_hash_context_for(
            hash_version,
            standard_library_revision,
            verified_standard.as_ref(),
            &record,
        )?;
        let stored_hash = Sha256Digest::from_bytes(digest_bytes(
            record.column(
                row,
                "content_hash",
                "catalogue revision digest must be 32 bytes",
            )?,
            &record,
            "catalogue revision digest must be 32 bytes",
        )?);

        let (functions, origins) =
            load_catalogue_functions(transaction, *catalogue, &catalogue_hash_context).await?;
        let references = load_references(
            transaction,
            *catalogue,
            catalogue_hash_context
                .standard()
                .map(|standard| standard.revision()),
        )
        .await?;
        validate_reference_sources(&functions, &references)?;
        let semantics = load_catalogue_semantics(
            transaction,
            *catalogue,
            functions,
            origins,
            &catalogue_hash_context,
        )
        .await?;

        let revisions = load_catalogue_current_revisions(transaction, *catalogue).await?;
        let mut current_revisions = Vec::with_capacity(semantics.catalogue.functions().len());
        for function in semantics.catalogue.functions() {
            let revision = revisions
                .get(&function.current_revision())
                .ok_or_else(|| {
                    DurableRecord::new(
                        "_orna_kernel.function_revisions",
                        function.current_revision().canonical(),
                    )
                        .invariant(
                            "every reachable catalogue function must resolve its immutable current revision",
                        )
                })?;
            if revision.function() != function.id() {
                return Err(DurableRecord::new(
                    "_orna_kernel.function_revisions",
                    revision.id().canonical(),
                )
                .invariant(
                    "reachable catalogue current revision must belong to its exact function",
                ));
            }
            current_revisions.push(revision.clone());
        }

        let computed_hash = catalogue_digest_with_context(
            &catalogue_hash_context,
            &semantics.catalogue,
            &current_revisions,
            &semantics.expressions,
            &semantics.origins,
            &references,
        )
        .map_err(PostgresKernelError::CanonicalHash)?;
        if computed_hash != stored_hash {
            return Err(record.invariant(
                "catalogue revision digest must match its complete recovered semantic catalogue",
            ));
        }
    }
    Ok(())
}

async fn validate_source_revision(
    transaction: &Transaction<'_>,
    source: SourceRevisionId,
) -> Result<Option<SourceRevisionId>, PostgresKernelError> {
    let source_record = DurableRecord::new(SOURCE_REVISION_RELATION, source.canonical());
    let rows = transaction
        .query(
            "SELECT
                source.parent_source_revision_id AS source_parent_id,
                source.bundle_id AS source_bundle_id,
                source.content_hash AS source_hash,
                source.hash_algorithm AS source_algorithm,
                source.hash_contract_version AS source_contract_version,
                bundle.id AS bundle_id,
                bundle.content_hash AS bundle_hash,
                bundle.hash_algorithm AS bundle_algorithm,
                bundle.hash_contract_version AS bundle_contract_version
             FROM _orna_kernel.source_revisions AS source
             JOIN _orna_kernel.source_bundles AS bundle
               ON bundle.id = source.bundle_id
             WHERE source.id = $1",
            &[&source.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    if rows.len() != 1 {
        return Err(source_record
            .invariant("each retained source revision must join exactly one source bundle"));
    }

    let row = &rows[0];
    let parent = optional_identity_bytes(
        source_record.column(
            row,
            "source_parent_id",
            "source parent identity must be null or 16 bytes",
        )?,
        &source_record,
        "source parent identity must be null or 16 bytes",
    )?
    .map(SourceRevisionId::from_bytes);
    let bundle = SourceBundleId::from_bytes(identity_bytes(
        source_record.column(
            row,
            "source_bundle_id",
            "source bundle identity must be 16 bytes",
        )?,
        &source_record,
        "source bundle identity must be 16 bytes",
    )?);
    let bundle_record = DurableRecord::new("_orna_kernel.source_bundles", bundle.canonical());
    let joined_bundle = SourceBundleId::from_bytes(identity_bytes(
        bundle_record.column(row, "bundle_id", "source bundle identity must be 16 bytes")?,
        &bundle_record,
        "source bundle identity must be 16 bytes",
    )?);
    if joined_bundle != bundle {
        return Err(
            source_record.invariant("source revision must join its exact source bundle identity")
        );
    }
    let source_hash = Sha256Digest::from_bytes(digest_bytes(
        source_record.column(
            row,
            "source_hash",
            "source revision digest must be 32 bytes",
        )?,
        &source_record,
        "source revision digest must be 32 bytes",
    )?);
    let bundle_hash = Sha256Digest::from_bytes(digest_bytes(
        bundle_record.column(row, "bundle_hash", "source bundle digest must be 32 bytes")?,
        &bundle_record,
        "source bundle digest must be 32 bytes",
    )?);

    require_hash_contract(
        row,
        &source_record,
        "source_algorithm",
        "source_contract_version",
        "source revision hash algorithm must be sha256",
        "source revision hash contract version must be 1",
    )?;
    require_hash_contract(
        row,
        &bundle_record,
        "bundle_algorithm",
        "bundle_contract_version",
        "source bundle hash algorithm must be sha256",
        "source bundle hash contract version must be 1",
    )?;

    let units = load_source_units(transaction, bundle).await?;
    let computed_bundle_hash =
        source_bundle_digest(&units).map_err(PostgresKernelError::CanonicalHash)?;
    if computed_bundle_hash != bundle_hash {
        return Err(bundle_record
            .invariant("source bundle digest must match the ordered source unit records"));
    }
    let computed_source_hash = source_revision_record_digest(bundle, parent, bundle_hash)
        .map_err(PostgresKernelError::CanonicalHash)?;
    if computed_source_hash != source_hash {
        return Err(source_record
            .invariant("source revision digest must match its bundle, parent, and bundle digest"));
    }
    Ok(parent)
}
