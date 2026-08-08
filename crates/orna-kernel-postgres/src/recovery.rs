use std::collections::HashSet;

use orna_core::{
    CatalogueRevisionId, SourceBundleId, SourceRevisionId, SourceUnitId,
    canonical_hash::{
        catalogue_digest, source_bundle_digest, source_revision_digest, source_unit_content_digest,
    },
    catalogue::CatalogueSnapshot,
    revision::{
        ActiveDatabaseRevision, RevisionPair, Sha256Digest, StoredSourceRevision, StoredSourceUnit,
    },
};
use tokio_postgres::{Client, IsolationLevel, Row, Transaction};

use crate::{
    PostgresKernel, PostgresKernelError,
    bootstrap::require_current_migrations,
    decode::{
        DurableRecord, digest_bytes, exact_enum, identity_bytes, optional_identity_bytes,
        u32_from_i64, u64_from_i64,
    },
};

const ACTIVE_RELATION: &str = "_orna_kernel.active_revision";
const SOURCE_UNIT_RELATION: &str = "_orna_kernel.source_units";

#[derive(Clone, Copy)]
enum HashAlgorithm {
    Sha256,
}

#[derive(Clone, Copy)]
enum TextEncoding {
    Utf8,
}

struct RecoveredRevisionHeader {
    bundle: SourceBundleId,
    source: SourceRevisionId,
    source_parent: Option<SourceRevisionId>,
    catalogue: CatalogueRevisionId,
    bundle_hash: Sha256Digest,
    source_hash: Sha256Digest,
    catalogue_hash: Sha256Digest,
}

impl PostgresKernel {
    /// Reconstructs and validates the complete active durable database revision.
    ///
    /// This first recovery slice supports an empty semantic catalogue. It
    /// fails closed when the active revision contains semantic or physical
    /// members that this binary cannot reconstruct completely.
    pub async fn recover(&self) -> Result<ActiveDatabaseRevision, PostgresKernelError> {
        let mut session = self.open().await?;
        let recovery_result = recover_client(&mut session.client).await;
        let shutdown_result = session.shutdown().await;

        match (recovery_result, shutdown_result) {
            (Ok(active), Ok(())) => Ok(active),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }
}

async fn recover_client(
    client: &mut Client,
) -> Result<ActiveDatabaseRevision, PostgresKernelError> {
    let transaction = client
        .build_transaction()
        .isolation_level(IsolationLevel::RepeatableRead)
        .read_only(true)
        .start()
        .await
        .map_err(PostgresKernelError::Database)?;

    require_current_migrations(&transaction).await?;
    let header = load_active_header(&transaction).await?;
    validate_revision_ancestry(&transaction, header.catalogue, header.source).await?;
    reject_unsupported_durable_state(&transaction, header.catalogue).await?;
    let units = load_source_units(&transaction, header.bundle).await?;
    let active = assemble_empty_revision(header, units)?;

    transaction
        .commit()
        .await
        .map_err(PostgresKernelError::Database)?;
    Ok(active)
}

async fn validate_revision_ancestry(
    transaction: &Transaction<'_>,
    active_catalogue: CatalogueRevisionId,
    active_source: SourceRevisionId,
) -> Result<(), PostgresKernelError> {
    let mut catalogue = active_catalogue;
    let mut source = active_source;
    let mut seen_catalogues = HashSet::new();
    let mut seen_sources = HashSet::new();

    loop {
        let catalogue_record =
            DurableRecord::new("_orna_kernel.catalogue_revisions", catalogue.canonical());
        if !seen_catalogues.insert(catalogue) || !seen_sources.insert(source) {
            return Err(catalogue_record.invariant(
                "catalogue and source revision ancestry must terminate without repeated identities",
            ));
        }

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
            (None, None, None) => return Ok(()),
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

async fn load_active_header(
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
    })
}

fn require_hash_contract(
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

async fn reject_unsupported_durable_state(
    transaction: &Transaction<'_>,
    catalogue: CatalogueRevisionId,
) -> Result<(), PostgresKernelError> {
    let catalogue_bytes = catalogue.to_bytes().to_vec();
    let row = transaction
        .query_one(
            "SELECT
                (SELECT count(*) FROM _orna_kernel.catalogue_schemas
                 WHERE catalogue_revision_id = $1) AS catalogue_schemas,
                (SELECT count(*) FROM _orna_kernel.catalogue_object_types
                 WHERE catalogue_revision_id = $1) AS catalogue_object_types,
                (SELECT count(*) FROM _orna_kernel.catalogue_fields
                 WHERE catalogue_revision_id = $1) AS catalogue_fields,
                (SELECT count(*) FROM _orna_kernel.catalogue_expressions
                 WHERE catalogue_revision_id = $1) AS catalogue_expressions,
                (SELECT count(*) FROM _orna_kernel.catalogue_functions
                 WHERE catalogue_revision_id = $1) AS catalogue_functions,
                (SELECT count(*) FROM _orna_kernel.catalogue_function_parameters
                 WHERE catalogue_revision_id = $1) AS catalogue_function_parameters,
                (SELECT count(*) FROM _orna_kernel.catalogue_function_return_columns
                 WHERE catalogue_revision_id = $1) AS catalogue_function_return_columns,
                (SELECT count(*) FROM _orna_kernel.function_revisions) AS function_revisions,
                (SELECT count(*) FROM _orna_kernel.function_artifacts) AS function_artifacts,
                (SELECT count(*) FROM _orna_kernel.definition_references) AS definition_references,
                (SELECT count(*)
                 FROM pg_class AS relation
                 JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
                 WHERE namespace.nspname = '_orna_data') AS data_relations",
            &[&catalogue_bytes],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let unsupported = [
        ("_orna_kernel.catalogue_schemas", "catalogue_schemas"),
        (
            "_orna_kernel.catalogue_object_types",
            "catalogue_object_types",
        ),
        ("_orna_kernel.catalogue_fields", "catalogue_fields"),
        (
            "_orna_kernel.catalogue_expressions",
            "catalogue_expressions",
        ),
        ("_orna_kernel.catalogue_functions", "catalogue_functions"),
        (
            "_orna_kernel.catalogue_function_parameters",
            "catalogue_function_parameters",
        ),
        (
            "_orna_kernel.catalogue_function_return_columns",
            "catalogue_function_return_columns",
        ),
        ("_orna_kernel.function_revisions", "function_revisions"),
        ("_orna_kernel.function_artifacts", "function_artifacts"),
        (
            "_orna_kernel.definition_references",
            "definition_references",
        ),
        ("_orna_data", "data_relations"),
    ];

    for (relation, column) in unsupported {
        let count_record = DurableRecord::new(relation, catalogue.canonical());
        let count = u64_from_i64(
            count_record.column(
                &row,
                column,
                "durable relation count must be a non-negative bigint",
            )?,
            &count_record,
            "durable relation count must be non-negative",
        )?;
        if count != 0 {
            return Err(PostgresKernelError::DurableInvariant {
                relation,
                record: catalogue.canonical(),
                rule: "empty semantic catalogue recovery cannot omit present durable records",
            });
        }
    }
    Ok(())
}

async fn load_source_units(
    transaction: &Transaction<'_>,
    bundle: SourceBundleId,
) -> Result<Vec<StoredSourceUnit>, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT
                id,
                bundle_id,
                ordinal,
                logical_path,
                content,
                content_hash,
                hash_algorithm,
                hash_contract_version,
                encoding
             FROM _orna_kernel.source_units
             WHERE bundle_id = $1
             ORDER BY ordinal",
            &[&bundle.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    rows.iter()
        .enumerate()
        .map(|(index, row)| decode_source_unit(row, index, bundle))
        .collect()
}

fn decode_source_unit(
    row: &Row,
    row_index: usize,
    expected_bundle: SourceBundleId,
) -> Result<StoredSourceUnit, PostgresKernelError> {
    let record = DurableRecord::new(SOURCE_UNIT_RELATION, format!("row={row_index}"));
    let id = SourceUnitId::from_bytes(identity_bytes(
        record.column(row, "id", "source unit identity must be 16 bytes")?,
        &record,
        "source unit identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(SOURCE_UNIT_RELATION, id.canonical());
    let bundle = SourceBundleId::from_bytes(identity_bytes(
        record.column(
            row,
            "bundle_id",
            "source unit bundle identity must be 16 bytes",
        )?,
        &record,
        "source unit bundle identity must be 16 bytes",
    )?);
    if bundle != expected_bundle {
        return Err(record.invariant("source unit must belong to the selected source bundle"));
    }

    let ordinal = u32_from_i64(
        record.column(row, "ordinal", "source unit ordinal must fit u32")?,
        &record,
        "source unit ordinal must fit u32",
    )?;
    let logical_path: String = record.column(
        row,
        "logical_path",
        "source unit logical path must be PostgreSQL text",
    )?;
    let content: String = record.column(
        row,
        "content",
        "source unit content must be exact PostgreSQL UTF-8 text",
    )?;
    let content_hash = Sha256Digest::from_bytes(digest_bytes(
        record.column(row, "content_hash", "source unit digest must be 32 bytes")?,
        &record,
        "source unit digest must be 32 bytes",
    )?);
    let algorithm: String = record.column(
        row,
        "hash_algorithm",
        "source unit hash algorithm must be sha256",
    )?;
    exact_enum(
        &algorithm,
        &[("sha256", HashAlgorithm::Sha256)],
        &record,
        "source unit hash algorithm must be sha256",
    )?;
    let contract_version: i16 = record.column(
        row,
        "hash_contract_version",
        "source unit hash contract version must be 1",
    )?;
    if contract_version != 1 {
        return Err(record.invariant("source unit hash contract version must be 1"));
    }
    let encoding: String = record.column(row, "encoding", "source unit encoding must be utf-8")?;
    exact_enum(
        &encoding,
        &[("utf-8", TextEncoding::Utf8)],
        &record,
        "source unit encoding must be utf-8",
    )?;

    let computed_hash =
        source_unit_content_digest(&content).map_err(PostgresKernelError::CanonicalHash)?;
    if computed_hash != content_hash {
        return Err(record.invariant("source unit digest must match its exact UTF-8 content"));
    }

    StoredSourceUnit::new(id, ordinal, logical_path, content, content_hash)
        .map_err(PostgresKernelError::RevisionInvariant)
}

fn assemble_empty_revision(
    header: RecoveredRevisionHeader,
    units: Vec<StoredSourceUnit>,
) -> Result<ActiveDatabaseRevision, PostgresKernelError> {
    let bundle_record =
        DurableRecord::new("_orna_kernel.source_bundles", header.bundle.canonical());
    let source_record =
        DurableRecord::new("_orna_kernel.source_revisions", header.source.canonical());
    let computed_bundle_hash =
        source_bundle_digest(&units).map_err(PostgresKernelError::CanonicalHash)?;
    if computed_bundle_hash != header.bundle_hash {
        return Err(bundle_record
            .invariant("source bundle digest must match the ordered source unit records"));
    }

    let source = StoredSourceRevision::new(
        header.bundle,
        header.source,
        header.source_parent,
        units,
        header.bundle_hash,
        header.source_hash,
    )
    .map_err(PostgresKernelError::RevisionInvariant)?;
    let computed_source_hash =
        source_revision_digest(&source).map_err(PostgresKernelError::CanonicalHash)?;
    if computed_source_hash != header.source_hash {
        return Err(source_record
            .invariant("source revision digest must match its bundle, parent, and bundle digest"));
    }

    let catalogue = CatalogueSnapshot::new(header.catalogue, Vec::new(), Vec::new())
        .map_err(PostgresKernelError::CatalogueSnapshot)?;
    let computed_catalogue_hash = catalogue_digest(&catalogue, &[], &[], &[], &[])
        .map_err(PostgresKernelError::CanonicalHash)?;
    if computed_catalogue_hash != header.catalogue_hash {
        let catalogue_record = DurableRecord::new(
            "_orna_kernel.catalogue_revisions",
            header.catalogue.canonical(),
        );
        return Err(catalogue_record
            .invariant("catalogue digest must match the exact empty semantic catalogue"));
    }

    ActiveDatabaseRevision::new_with_history(
        RevisionPair::new(header.source, header.catalogue),
        source,
        catalogue,
        header.catalogue_hash,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .map_err(PostgresKernelError::RevisionInvariant)
}

#[cfg(test)]
mod tests {
    use orna_core::{
        CatalogueRevisionId, SourceBundleId, SourceRevisionId, SourceUnitId,
        canonical_hash::{
            catalogue_digest, source_bundle_digest, source_revision_record_digest,
            source_unit_content_digest,
        },
        catalogue::CatalogueSnapshot,
        revision::StoredSourceUnit,
    };

    use super::{RecoveredRevisionHeader, assemble_empty_revision};

    #[test]
    fn assembles_the_exact_empty_semantic_revision() {
        let bundle = SourceBundleId::from_bytes([1; 16]);
        let source = SourceRevisionId::from_bytes([2; 16]);
        let catalogue = CatalogueRevisionId::from_bytes([3; 16]);
        let bundle_hash = source_bundle_digest(&[]).expect("empty source bundle hash");
        let source_hash = source_revision_record_digest(bundle, None, bundle_hash)
            .expect("empty source revision hash");
        let empty_catalogue =
            CatalogueSnapshot::new(catalogue, Vec::new(), Vec::new()).expect("empty catalogue");
        let catalogue_hash =
            catalogue_digest(&empty_catalogue, &[], &[], &[], &[]).expect("empty catalogue hash");

        let recovered = assemble_empty_revision(
            RecoveredRevisionHeader {
                bundle,
                source,
                source_parent: None,
                catalogue,
                bundle_hash,
                source_hash,
                catalogue_hash,
            },
            Vec::new(),
        )
        .expect("exact empty revision");

        assert_eq!(recovered.pair().source(), source);
        assert_eq!(recovered.pair().catalogue(), catalogue);
        assert!(recovered.source().units().is_empty());
        assert!(recovered.catalogue().schemas().is_empty());
        assert!(recovered.catalogue().object_types().is_empty());
        assert!(recovered.catalogue().functions().is_empty());
        assert!(recovered.function_revisions().is_empty());
        assert!(recovered.historical_function_revisions().is_empty());
    }

    #[test]
    fn rejects_an_empty_catalogue_with_a_different_digest() {
        let bundle = SourceBundleId::from_bytes([4; 16]);
        let source = SourceRevisionId::from_bytes([5; 16]);
        let catalogue = CatalogueRevisionId::from_bytes([6; 16]);
        let bundle_hash = source_bundle_digest(&[]).expect("empty source bundle hash");
        let source_hash = source_revision_record_digest(bundle, None, bundle_hash)
            .expect("empty source revision hash");

        assert!(
            assemble_empty_revision(
                RecoveredRevisionHeader {
                    bundle,
                    source,
                    source_parent: None,
                    catalogue,
                    bundle_hash,
                    source_hash,
                    catalogue_hash: bundle_hash,
                },
                Vec::new(),
            )
            .is_err()
        );
    }

    #[test]
    fn assembles_an_empty_semantic_revision_with_exact_source_content() {
        let bundle = SourceBundleId::from_bytes([7; 16]);
        let source = SourceRevisionId::from_bytes([8; 16]);
        let catalogue = CatalogueRevisionId::from_bytes([9; 16]);
        let content = "schema app";
        let unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes([10; 16]),
            0,
            "schema.orna",
            content,
            source_unit_content_digest(content).expect("source content hash"),
        )
        .expect("stored source unit");
        let units = vec![unit];
        let bundle_hash = source_bundle_digest(&units).expect("source bundle hash");
        let source_hash =
            source_revision_record_digest(bundle, None, bundle_hash).expect("source revision hash");
        let empty_catalogue =
            CatalogueSnapshot::new(catalogue, Vec::new(), Vec::new()).expect("empty catalogue");
        let catalogue_hash =
            catalogue_digest(&empty_catalogue, &[], &[], &[], &[]).expect("empty catalogue hash");

        let recovered = assemble_empty_revision(
            RecoveredRevisionHeader {
                bundle,
                source,
                source_parent: None,
                catalogue,
                bundle_hash,
                source_hash,
                catalogue_hash,
            },
            units,
        )
        .expect("empty semantic revision with source");

        assert_eq!(recovered.source().units().len(), 1);
        assert_eq!(recovered.source().units()[0].content(), content);
    }
}
