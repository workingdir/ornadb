//! Durable catalogue schema and type row recovery.

use super::*;

pub(super) struct RecoveredSchema {
    pub(super) definition: SchemaDefinition,
    pub(super) origin: DefinitionOrigin,
}

pub(super) struct RecoveredObjectType {
    pub(super) id: TypeId,
    pub(super) schema: SchemaId,
    pub(super) name: QualifiedSemanticName,
    pub(super) origin: DefinitionOrigin,
}

pub(super) struct RecoveredEnumType {
    pub(super) schema: SchemaId,
    pub(super) definition: EnumTypeDefinition,
    pub(super) origin: DefinitionOrigin,
}

pub(super) struct RecoveredRecordValueType {
    pub(super) id: TypeId,
    pub(super) schema: SchemaId,
    pub(super) name: QualifiedSemanticName,
    pub(super) origin: DefinitionOrigin,
}

pub(super) async fn load_schemas(
    transaction: &Transaction<'_>,
    catalogue: CatalogueRevisionId,
) -> Result<Vec<RecoveredSchema>, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT
                catalogue_revision_id,
                schema_id,
                name_parts,
                source_unit_id,
                source_start,
                source_end
             FROM _orna_kernel.catalogue_schemas
             WHERE catalogue_revision_id = $1
             ORDER BY schema_id",
            &[&catalogue.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    rows.iter()
        .enumerate()
        .map(|(index, row)| decode_schema(row, index, catalogue))
        .collect()
}

fn decode_schema(
    row: &Row,
    row_index: usize,
    expected_catalogue: CatalogueRevisionId,
) -> Result<RecoveredSchema, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.catalogue_schemas";
    let record = DurableRecord::new(RELATION, format!("row={row_index}"));
    let catalogue = CatalogueRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "catalogue_revision_id",
            "schema catalogue revision identity must be 16 bytes",
        )?,
        &record,
        "schema catalogue revision identity must be 16 bytes",
    )?);
    if catalogue != expected_catalogue {
        return Err(record.invariant("schema must belong to the selected catalogue revision"));
    }

    let id = SchemaId::from_bytes(identity_bytes(
        record.column(row, "schema_id", "schema identity must be 16 bytes")?,
        &record,
        "schema identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(RELATION, id.canonical());
    let name_parts: Vec<String> = record.column(
        row,
        "name_parts",
        "schema name parts must be an exact PostgreSQL text array",
    )?;
    let name = QualifiedSemanticName::new(name_parts)
        .map_err(|_| record.invariant("schema name parts must form one exact semantic name"))?;

    let source_unit: Option<Vec<u8>> = record.column(
        row,
        "source_unit_id",
        "schema source origin must contain a source unit identity",
    )?;
    let source_start: Option<i64> = record.column(
        row,
        "source_start",
        "schema source origin start must be a non-negative bigint",
    )?;
    let source_end: Option<i64> = record.column(
        row,
        "source_end",
        "schema source origin end must be a non-negative bigint",
    )?;
    let (source_unit, source_start, source_end) = match (source_unit, source_start, source_end) {
        (Some(source_unit), Some(source_start), Some(source_end)) => {
            (source_unit, source_start, source_end)
        }
        _ => {
            return Err(record.invariant(
                "schema source origin must contain source unit, start, and end values",
            ));
        }
    };
    let source_unit = SourceUnitId::from_bytes(identity_bytes(
        source_unit,
        &record,
        "schema source unit identity must be 16 bytes",
    )?);
    let source_start = u32_from_i64(
        source_start,
        &record,
        "schema source origin start must fit u32",
    )?;
    let source_end = u32_from_i64(source_end, &record, "schema source origin end must fit u32")?;
    let origin = SourceOrigin::new(source_unit, source_start, source_end)
        .map_err(PostgresKernelError::RevisionInvariant)?;

    Ok(RecoveredSchema {
        definition: SchemaDefinition::new(id, name),
        origin: DefinitionOrigin::new(DefinitionIdentity::Schema(id), origin),
    })
}

pub(super) async fn load_object_types(
    transaction: &Transaction<'_>,
    catalogue: CatalogueRevisionId,
) -> Result<Vec<RecoveredObjectType>, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT catalogue_revision_id, type_id, schema_id, name_parts,
                    source_unit_id, source_start, source_end
             FROM _orna_kernel.catalogue_object_types
             WHERE catalogue_revision_id = $1
             ORDER BY type_id",
            &[&catalogue.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    rows.iter()
        .enumerate()
        .map(|(index, row)| decode_object_type(row, index, catalogue))
        .collect()
}

fn decode_object_type(
    row: &Row,
    row_index: usize,
    expected_catalogue: CatalogueRevisionId,
) -> Result<RecoveredObjectType, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.catalogue_object_types";
    let row_record = DurableRecord::new(RELATION, format!("row={row_index}"));
    require_catalogue_identity(row, &row_record, expected_catalogue, "object type")?;
    let id = TypeId::from_bytes(identity_bytes(
        row_record.column(row, "type_id", "object type identity must be 16 bytes")?,
        &row_record,
        "object type identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(RELATION, id.canonical());
    let schema = SchemaId::from_bytes(identity_bytes(
        record.column(row, "schema_id", "object schema identity must be 16 bytes")?,
        &record,
        "object schema identity must be 16 bytes",
    )?);
    let name_parts: Vec<String> = record.column(
        row,
        "name_parts",
        "object name parts must be an exact PostgreSQL text array",
    )?;
    let name = QualifiedSemanticName::new(name_parts)
        .map_err(|_| record.invariant("object name parts must form one exact semantic name"))?;
    let origin = decode_origin(row, &record, DefinitionIdentity::ObjectType(id))?;

    Ok(RecoveredObjectType {
        id,
        schema,
        name,
        origin,
    })
}

pub(super) async fn load_enum_types(
    transaction: &Transaction<'_>,
    catalogue: CatalogueRevisionId,
) -> Result<Vec<RecoveredEnumType>, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT catalogue_revision_id, type_id, schema_id, name_parts, labels,
                    source_unit_id, source_start, source_end
             FROM _orna_kernel.catalogue_enum_types
             WHERE catalogue_revision_id = $1
             ORDER BY type_id",
            &[&catalogue.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    rows.iter()
        .enumerate()
        .map(|(index, row)| decode_enum_type(row, index, catalogue))
        .collect()
}

fn decode_enum_type(
    row: &Row,
    row_index: usize,
    expected_catalogue: CatalogueRevisionId,
) -> Result<RecoveredEnumType, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.catalogue_enum_types";
    let row_record = DurableRecord::new(RELATION, format!("row={row_index}"));
    require_catalogue_identity(row, &row_record, expected_catalogue, "enum type")?;
    let id = TypeId::from_bytes(identity_bytes(
        row_record.column(row, "type_id", "enum type identity must be 16 bytes")?,
        &row_record,
        "enum type identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(RELATION, id.canonical());
    let schema = SchemaId::from_bytes(identity_bytes(
        record.column(row, "schema_id", "enum schema identity must be 16 bytes")?,
        &record,
        "enum schema identity must be 16 bytes",
    )?);
    let name_parts: Vec<String> = record.column(
        row,
        "name_parts",
        "enum name parts must be an exact PostgreSQL text array",
    )?;
    let name = QualifiedSemanticName::new(name_parts)
        .map_err(|_| record.invariant("enum name parts must form one exact semantic name"))?;
    let labels: Vec<String> = record.column(
        row,
        "labels",
        "enum labels must be one exact PostgreSQL text array",
    )?;
    let origin = decode_origin(row, &record, DefinitionIdentity::ValueType(id))?;

    Ok(RecoveredEnumType {
        schema,
        definition: EnumTypeDefinition::new(id, name, labels),
        origin,
    })
}

pub(super) async fn load_record_value_types(
    transaction: &Transaction<'_>,
    catalogue: CatalogueRevisionId,
) -> Result<Vec<RecoveredRecordValueType>, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT catalogue_revision_id, type_id, schema_id, name_parts,
                    value_kind, mutability, persistence,
                    source_unit_id, source_start, source_end
             FROM _orna_kernel.catalogue_record_value_types
             WHERE catalogue_revision_id = $1
             ORDER BY type_id",
            &[&catalogue.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    rows.iter()
        .enumerate()
        .map(|(index, row)| decode_record_value_type(row, index, catalogue))
        .collect()
}

fn decode_record_value_type(
    row: &Row,
    row_index: usize,
    expected_catalogue: CatalogueRevisionId,
) -> Result<RecoveredRecordValueType, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.catalogue_record_value_types";
    let row_record = DurableRecord::new(RELATION, format!("row={row_index}"));
    require_catalogue_identity(row, &row_record, expected_catalogue, "record value type")?;
    let id = TypeId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "type_id",
            "record value type identity must be 16 bytes",
        )?,
        &row_record,
        "record value type identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(RELATION, id.canonical());
    let schema = SchemaId::from_bytes(identity_bytes(
        record.column(
            row,
            "schema_id",
            "record value schema identity must be 16 bytes",
        )?,
        &record,
        "record value schema identity must be 16 bytes",
    )?);
    let name_parts: Vec<String> = record.column(
        row,
        "name_parts",
        "record value name parts must be an exact PostgreSQL text array",
    )?;
    let name = QualifiedSemanticName::new(name_parts).map_err(|_| {
        record.invariant("record value name parts must form one exact semantic name")
    })?;
    for (column, expected, rule) in [
        ("value_kind", "record", "record value kind must be record"),
        (
            "mutability",
            "immutable",
            "record value mutability must be immutable",
        ),
        (
            "persistence",
            "persistable",
            "record value persistence must be persistable",
        ),
    ] {
        let actual: String = record.column(row, column, rule)?;
        if actual != expected {
            return Err(record.invariant(rule));
        }
    }
    let origin = decode_origin(row, &record, DefinitionIdentity::ValueType(id))?;

    Ok(RecoveredRecordValueType {
        id,
        schema,
        name,
        origin,
    })
}
