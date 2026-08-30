//! Standard-library schemas, types, and bindings.

use super::*;

pub(super) struct RecoveredStandardSchema {
    pub(super) definition: SchemaDefinition,
    pub(super) origin: DefinitionOrigin,
}

pub(super) struct RecoveredStandardValueType {
    pub(super) schema: SchemaId,
    pub(super) definition: ValueTypeDefinition,
    pub(super) origin: DefinitionOrigin,
}

pub(super) struct RecoveredStandardEnumType {
    pub(super) schema: SchemaId,
    pub(super) definition: EnumTypeDefinition,
    pub(super) origin: DefinitionOrigin,
}

pub(super) struct RecoveredStandardTypeBinding {
    pub(super) binding: TypeBinding,
    pub(super) origin: DefinitionOrigin,
}

pub(super) async fn load_standard_schemas(
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

pub(super) async fn load_standard_value_types(
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

pub(in super::super::super) fn recovered_standard_value_definition(
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

pub(super) async fn load_standard_enum_types(
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

pub(super) async fn load_standard_type_bindings(
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

pub(in super::super::super) fn decode_standard_binding_target(
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
