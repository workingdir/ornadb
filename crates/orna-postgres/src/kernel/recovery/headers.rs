//! Active revision headers and durable hash contracts.

use super::*;

#[derive(Clone, Copy)]
pub(super) enum HashAlgorithm {
    Sha256,
}

#[derive(Clone, Copy)]
pub(super) enum TextEncoding {
    Utf8,
}

pub(super) struct RecoveredRevisionHeader {
    pub(super) bundle: SourceBundleId,
    pub(super) source: SourceRevisionId,
    pub(super) source_parent: Option<SourceRevisionId>,
    pub(super) catalogue: CatalogueRevisionId,
    pub(super) bundle_hash: Sha256Digest,
    pub(super) source_hash: Sha256Digest,
    pub(super) catalogue_hash: Sha256Digest,
    pub(super) catalogue_hash_version: CatalogueHashVersion,
    pub(super) standard_library_revision: Option<StandardLibraryRevisionId>,
}

pub(super) async fn load_active_header(
    transaction: &Transaction<'_>,
) -> Result<RecoveredRevisionHeader, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT
                active.singleton,
                active.source_revision_id AS active_source_id,
                active.catalogue_revision_id AS active_catalogue_id,
                catalogue.id AS catalogue_id,
                catalogue.source_revision_id AS catalogue_source_id,
                catalogue.parent_catalogue_revision_id AS catalogue_parent_id,
                catalogue.content_hash AS catalogue_hash,
                catalogue.hash_algorithm AS catalogue_algorithm,
                catalogue.hash_contract_version AS catalogue_contract_version,
                catalogue.canonical_hash_version AS catalogue_canonical_hash_version,
                catalogue.standard_library_revision_id AS catalogue_standard_library_revision_id,
                parent_catalogue.source_revision_id AS parent_catalogue_source_id,
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
             FROM _orna_kernel.active_revision AS active
             JOIN _orna_kernel.catalogue_revisions AS catalogue
               ON catalogue.id = active.catalogue_revision_id
              AND catalogue.source_revision_id = active.source_revision_id
             JOIN _orna_kernel.source_revisions AS source
               ON source.id = active.source_revision_id
             JOIN _orna_kernel.source_bundles AS bundle
               ON bundle.id = source.bundle_id
             LEFT JOIN _orna_kernel.catalogue_revisions AS parent_catalogue
               ON parent_catalogue.id = catalogue.parent_catalogue_revision_id",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    if rows.len() != 1 {
        return Err(PostgresKernelError::DurableInvariant {
            relation: ACTIVE_RELATION,
            record: "singleton=true".into(),
            rule: "exactly one active catalogue, source revision, and source bundle join must exist",
        });
    }

    decode_active_header(&rows[0])
}

fn decode_active_header(row: &Row) -> Result<RecoveredRevisionHeader, PostgresKernelError> {
    let active_record = DurableRecord::new(ACTIVE_RELATION, "singleton=true");
    let singleton: bool = active_record.column(
        row,
        "singleton",
        "the active revision singleton flag must be true",
    )?;
    if !singleton {
        return Err(active_record.invariant("the active revision singleton flag must be true"));
    }

    let active_source = SourceRevisionId::from_bytes(identity_bytes(
        active_record.column(
            row,
            "active_source_id",
            "active source revision identity must be 16 bytes",
        )?,
        &active_record,
        "active source revision identity must be 16 bytes",
    )?);
    let active_catalogue = CatalogueRevisionId::from_bytes(identity_bytes(
        active_record.column(
            row,
            "active_catalogue_id",
            "active catalogue revision identity must be 16 bytes",
        )?,
        &active_record,
        "active catalogue revision identity must be 16 bytes",
    )?);
    let catalogue_record = DurableRecord::new(
        "_orna_kernel.catalogue_revisions",
        active_catalogue.canonical(),
    );
    let catalogue = CatalogueRevisionId::from_bytes(identity_bytes(
        catalogue_record.column(
            row,
            "catalogue_id",
            "joined catalogue revision identity must be 16 bytes",
        )?,
        &catalogue_record,
        "joined catalogue revision identity must be 16 bytes",
    )?);
    let catalogue_source = SourceRevisionId::from_bytes(identity_bytes(
        catalogue_record.column(
            row,
            "catalogue_source_id",
            "catalogue source revision identity must be 16 bytes",
        )?,
        &catalogue_record,
        "catalogue source revision identity must be 16 bytes",
    )?);
    let source_record =
        DurableRecord::new("_orna_kernel.source_revisions", active_source.canonical());
    let source = SourceRevisionId::from_bytes(identity_bytes(
        source_record.column(
            row,
            "source_id",
            "joined source revision identity must be 16 bytes",
        )?,
        &source_record,
        "joined source revision identity must be 16 bytes",
    )?);
    let source_bundle = SourceBundleId::from_bytes(identity_bytes(
        source_record.column(
            row,
            "source_bundle_id",
            "source revision bundle identity must be 16 bytes",
        )?,
        &source_record,
        "source revision bundle identity must be 16 bytes",
    )?);
    let bundle_record =
        DurableRecord::new("_orna_kernel.source_bundles", source_bundle.canonical());
    let bundle = SourceBundleId::from_bytes(identity_bytes(
        bundle_record.column(
            row,
            "bundle_id",
            "joined source bundle identity must be 16 bytes",
        )?,
        &bundle_record,
        "joined source bundle identity must be 16 bytes",
    )?);

    if active_source != source || catalogue_source != source {
        return Err(active_record.invariant(
            "active and catalogue source links must identify the joined source revision",
        ));
    }
    if active_catalogue != catalogue {
        return Err(active_record
            .invariant("the active catalogue link must identify the joined catalogue revision"));
    }
    if source_bundle != bundle {
        return Err(source_record
            .invariant("the source revision bundle link must identify the joined source bundle"));
    }

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

    if catalogue_parent == Some(catalogue) {
        return Err(catalogue_record.invariant("the catalogue revision must not be its own parent"));
    }
    match (catalogue_parent, parent_catalogue_source) {
        (None, None) if source_parent.is_none() => {}
        (Some(_), Some(parent_source)) if source_parent == Some(parent_source) => {}
        _ => {
            return Err(catalogue_record.invariant(
                "the parent catalogue source link must equal the active source parent link",
            ));
        }
    }

    require_hash_contract(
        row,
        &catalogue_record,
        "catalogue_algorithm",
        "catalogue_contract_version",
        "catalogue hash algorithm must be sha256",
        "catalogue hash contract version must be 1",
    )?;
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

    let catalogue_hash_version = decode_catalogue_hash_version(
        catalogue_record.column(
            row,
            "catalogue_canonical_hash_version",
            "catalogue canonical hash version must be a supported smallint",
        )?,
        &catalogue_record,
    )?;
    let standard_library_revision = optional_identity_bytes(
        catalogue_record.column(
            row,
            "catalogue_standard_library_revision_id",
            "catalogue standard library revision identity must be null or 16 bytes",
        )?,
        &catalogue_record,
        "catalogue standard library revision identity must be null or 16 bytes",
    )?
    .map(StandardLibraryRevisionId::from_bytes);
    match (catalogue_hash_version, standard_library_revision) {
        (CatalogueHashVersion::Version1, None) | (CatalogueHashVersion::Version2, Some(_)) => {}
        _ => {
            return Err(catalogue_record.invariant(
                "catalogue hash version and standard library revision must form one exact context",
            ));
        }
    }

    Ok(RecoveredRevisionHeader {
        bundle,
        source,
        source_parent,
        catalogue,
        bundle_hash: Sha256Digest::from_bytes(digest_bytes(
            bundle_record.column(row, "bundle_hash", "source bundle digest must be 32 bytes")?,
            &bundle_record,
            "source bundle digest must be 32 bytes",
        )?),
        source_hash: Sha256Digest::from_bytes(digest_bytes(
            source_record.column(
                row,
                "source_hash",
                "source revision digest must be 32 bytes",
            )?,
            &source_record,
            "source revision digest must be 32 bytes",
        )?),
        catalogue_hash: Sha256Digest::from_bytes(digest_bytes(
            catalogue_record.column(
                row,
                "catalogue_hash",
                "catalogue revision digest must be 32 bytes",
            )?,
            &catalogue_record,
            "catalogue revision digest must be 32 bytes",
        )?),
        catalogue_hash_version,
        standard_library_revision,
    })
}

pub(super) fn decode_catalogue_hash_version(
    value: i16,
    record: &DurableRecord,
) -> Result<CatalogueHashVersion, PostgresKernelError> {
    let value = decode_durable_version(
        value,
        record,
        "catalogue canonical hash version must be a supported smallint",
    )?;
    CatalogueHashVersion::try_from(value)
        .map_err(|_| record.invariant("catalogue canonical hash version must be 1 or 2"))
}

pub(super) fn decode_durable_version(
    value: i16,
    record: &DurableRecord,
    smallint_rule: &'static str,
) -> Result<u32, PostgresKernelError> {
    u32_from_i64(i64::from(value), record, smallint_rule)
}

pub(super) async fn load_active_catalogue_hash_context(
    transaction: &Transaction<'_>,
    header: &RecoveredRevisionHeader,
) -> Result<CatalogueHashContext, PostgresKernelError> {
    let standard = match header.standard_library_revision {
        Some(revision) => Some(load_verified_standard_library(transaction, revision).await?),
        None => None,
    };
    let record = DurableRecord::new(
        "_orna_kernel.catalogue_revisions",
        header.catalogue.canonical(),
    );
    catalogue_hash_context_for(
        header.catalogue_hash_version,
        header.standard_library_revision,
        standard.as_ref(),
        &record,
    )
}

pub(super) fn catalogue_hash_context_for(
    version: CatalogueHashVersion,
    standard_revision: Option<StandardLibraryRevisionId>,
    verified_standard: Option<&VerifiedStandardLibrarySnapshot>,
    record: &DurableRecord,
) -> Result<CatalogueHashContext, PostgresKernelError> {
    match (version, standard_revision, verified_standard) {
        (CatalogueHashVersion::Version1, None, _) => Ok(CatalogueHashContext::version_one()),
        (CatalogueHashVersion::Version2, Some(revision), Some(standard))
            if standard.revision() == revision =>
        {
            Ok(CatalogueHashContext::version_two(standard.clone()))
        }
        (CatalogueHashVersion::Version1, Some(_), _) | (CatalogueHashVersion::Version2, None, _) => Err(record.invariant(
            "catalogue hash version and standard library revision must form one exact context",
        )),
        (CatalogueHashVersion::Version2, Some(_), None) => Err(record.invariant(
            "version 2 catalogue standard library revision must be recovered and verified",
        )),
        (CatalogueHashVersion::Version2, Some(_), Some(_)) => Err(record.invariant(
            "version 2 catalogue standard library revision must equal the recovered standard revision",
        )),
        _ => Err(record.invariant("catalogue hash version is unsupported")),
    }
}

pub(super) fn require_hash_contract(
    row: &Row,
    record: &DurableRecord,
    algorithm_column: &'static str,
    version_column: &'static str,
    algorithm_rule: &'static str,
    version_rule: &'static str,
) -> Result<(), PostgresKernelError> {
    let algorithm: String = record.column(row, algorithm_column, algorithm_rule)?;
    exact_enum(
        &algorithm,
        &[("sha256", HashAlgorithm::Sha256)],
        record,
        algorithm_rule,
    )?;
    let version: i16 = record.column(row, version_column, version_rule)?;
    if version != 1 {
        return Err(record.invariant(version_rule));
    }
    Ok(())
}
