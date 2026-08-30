//! Durable catalogue expression and definition-origin row recovery.

use super::*;

pub(super) struct RecoveredExpression {
    pub(super) artifact: ExpressionArtifact,
    pub(super) origin: DefinitionOrigin,
}

pub(super) async fn load_expressions(
    transaction: &Transaction<'_>,
    catalogue: CatalogueRevisionId,
) -> Result<Vec<RecoveredExpression>, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT catalogue_revision_id, expression_id, format,
                    format_version::bigint AS format_version, payload, content_hash,
                    hash_algorithm, hash_contract_version,
                    source_unit_id, source_start, source_end
             FROM _orna_kernel.catalogue_expressions
             WHERE catalogue_revision_id = $1
             ORDER BY expression_id",
            &[&catalogue.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    rows.iter()
        .enumerate()
        .map(|(index, row)| decode_expression(row, index, catalogue))
        .collect()
}

fn decode_expression(
    row: &Row,
    row_index: usize,
    expected_catalogue: CatalogueRevisionId,
) -> Result<RecoveredExpression, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.catalogue_expressions";
    let row_record = DurableRecord::new(RELATION, format!("row={row_index}"));
    require_catalogue_identity(row, &row_record, expected_catalogue, "expression")?;
    let id = ExpressionId::from_bytes(identity_bytes(
        row_record.column(row, "expression_id", "expression identity must be 16 bytes")?,
        &row_record,
        "expression identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(RELATION, id.canonical());
    require_hash_contract(
        row,
        &record,
        "hash_algorithm",
        "hash_contract_version",
        "expression hash algorithm must be sha256",
        "expression hash contract version must be 1",
    )?;
    let format: String =
        record.column(row, "format", "expression format must be PostgreSQL text")?;
    let version = u32_from_i64(
        record.column(
            row,
            "format_version",
            "expression format version must fit u32",
        )?,
        &record,
        "expression format version must fit u32",
    )?;
    let payload: Vec<u8> =
        record.column(row, "payload", "expression payload must be exact bytes")?;
    let content_hash = Sha256Digest::from_bytes(digest_bytes(
        record.column(row, "content_hash", "expression digest must be 32 bytes")?,
        &record,
        "expression digest must be 32 bytes",
    )?);
    let computed_hash =
        artifact_payload_digest(&payload).map_err(PostgresKernelError::CanonicalHash)?;
    if computed_hash != content_hash {
        return Err(record.invariant("expression digest must match its exact artifact payload"));
    }
    let artifact = ExpressionArtifact::new(id, format, version, payload, content_hash)
        .map_err(PostgresKernelError::RevisionInvariant)?;
    let origin = decode_origin(row, &record, DefinitionIdentity::Expression(id))?;
    Ok(RecoveredExpression { artifact, origin })
}

pub(super) fn require_catalogue_identity(
    row: &Row,
    record: &DurableRecord,
    expected: CatalogueRevisionId,
    member: &'static str,
) -> Result<(), PostgresKernelError> {
    let catalogue = CatalogueRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "catalogue_revision_id",
            "catalogue member revision identity must be 16 bytes",
        )?,
        record,
        "catalogue member revision identity must be 16 bytes",
    )?);
    if catalogue != expected {
        return Err(record.invariant(match member {
            "object type" => "object type must belong to the selected catalogue revision",
            "field" => "field must belong to the selected catalogue revision",
            "expression" => "expression must belong to the selected catalogue revision",
            _ => "catalogue member must belong to the selected catalogue revision",
        }));
    }
    Ok(())
}

pub(super) fn decode_origin(
    row: &Row,
    record: &DurableRecord,
    identity: DefinitionIdentity,
) -> Result<DefinitionOrigin, PostgresKernelError> {
    let unit: Option<Vec<u8>> = record.column(
        row,
        "source_unit_id",
        "definition origin must contain a source unit identity",
    )?;
    let start: Option<i64> = record.column(
        row,
        "source_start",
        "definition origin start must be a non-negative bigint",
    )?;
    let end: Option<i64> = record.column(
        row,
        "source_end",
        "definition origin end must be a non-negative bigint",
    )?;
    let (unit, start, end) = match (unit, start, end) {
        (Some(unit), Some(start), Some(end)) => (unit, start, end),
        _ => {
            return Err(record
                .invariant("definition origin must contain source unit, start, and end values"));
        }
    };
    let unit = SourceUnitId::from_bytes(identity_bytes(
        unit,
        record,
        "definition origin source unit identity must be 16 bytes",
    )?);
    let start = u32_from_i64(start, record, "definition origin start must fit u32")?;
    let end = u32_from_i64(end, record, "definition origin end must fit u32")?;
    let source =
        SourceOrigin::new(unit, start, end).map_err(PostgresKernelError::RevisionInvariant)?;
    Ok(DefinitionOrigin::new(identity, source))
}
