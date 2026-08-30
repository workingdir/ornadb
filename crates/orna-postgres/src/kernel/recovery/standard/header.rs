//! Standard-library revision header and source-chain recovery.

use super::*;

pub(super) struct RecoveredStandardHeader {
    pub(super) revision: StandardLibraryRevisionId,
    pub(super) bundle: SourceBundleId,
    pub(super) source: SourceRevisionId,
    pub(super) source_parent: Option<SourceRevisionId>,
    pub(super) catalogue: CatalogueRevisionId,
    pub(super) digest_version: StandardLibraryDigestVersion,
    pub(super) language_version: String,
    pub(super) bundle_hash: Sha256Digest,
    pub(super) source_hash: Sha256Digest,
    pub(super) digest: Sha256Digest,
}

pub(super) async fn load_standard_header(
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
