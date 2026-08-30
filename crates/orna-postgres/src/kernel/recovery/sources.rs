//! Durable source-unit loading and row decoding.

use super::*;

pub(super) async fn load_source_units(
    transaction: &Transaction<'_>,
    bundle: SourceBundleId,
) -> Result<Vec<StoredSourceUnit>, PostgresKernelError> {
    let rows = transaction
        .query(
            "WITH RECURSIVE source_ancestry(
                 source_revision_id,
                 bundle_id,
                 parent_source_revision_id,
                 path,
                 has_cycle
             ) AS (
                 SELECT
                     source.id,
                     source.bundle_id,
                     source.parent_source_revision_id,
                     ARRAY[source.id],
                     false
                 FROM _orna_kernel.source_revisions AS source
                 WHERE source.bundle_id = $1
                 UNION ALL
                 SELECT
                     parent.id,
                     parent.bundle_id,
                     parent.parent_source_revision_id,
                     array_append(child.path, parent.id),
                     parent.id = ANY(child.path)
                 FROM _orna_kernel.source_revisions AS parent
                 JOIN source_ancestry AS child
                   ON parent.id = child.parent_source_revision_id
                 WHERE NOT child.has_cycle
             )
             SELECT
                membership.bundle_id AS bundle_id,
                membership.source_unit_id AS id,
                membership.ordinal AS ordinal,
                unit.logical_path,
                unit.content,
                unit.content_hash,
                unit.hash_algorithm,
                unit.hash_contract_version,
                unit.encoding,
                unit.bundle_id AS legacy_bundle_id,
                EXISTS (
                    SELECT 1
                    FROM source_ancestry
                    WHERE source_ancestry.bundle_id = unit.bundle_id
                ) AS legacy_bundle_is_ancestor,
                COALESCE(
                    (
                        SELECT bool_or(source_ancestry.has_cycle)
                        FROM source_ancestry
                    ),
                    false
                ) AS source_ancestry_has_cycle
             FROM _orna_kernel.source_bundle_units AS membership
             JOIN _orna_kernel.source_units AS unit
               ON unit.id = membership.source_unit_id
             WHERE membership.bundle_id = $1
             ORDER BY membership.ordinal",
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
    let source_ancestry_has_cycle: bool = record.column(
        row,
        "source_ancestry_has_cycle",
        "source revision ancestry cycle flag must be boolean",
    )?;
    if source_ancestry_has_cycle {
        return Err(
            record.invariant("source revision ancestry must terminate without repeated identities")
        );
    }
    let _legacy_bundle = SourceBundleId::from_bytes(identity_bytes(
        record.column(
            row,
            "legacy_bundle_id",
            "source unit legacy bundle identity must be 16 bytes",
        )?,
        &record,
        "source unit legacy bundle identity must be 16 bytes",
    )?);
    let legacy_bundle_is_ancestor: bool = record.column(
        row,
        "legacy_bundle_is_ancestor",
        "source unit legacy bundle ancestry flag must be boolean",
    )?;
    if !legacy_bundle_is_ancestor {
        return Err(record.invariant(
            "source unit legacy bundle identity must remain in the source revision ancestry",
        ));
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
