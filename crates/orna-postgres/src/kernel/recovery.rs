// Result APIs intentionally preserve the accepted public `PostgresKernelError` layout.
#![allow(clippy::result_large_err)]
use std::collections::{BTreeMap, BTreeSet, HashSet};

#[path = "recovery/active_revision.rs"]
mod active_revision;
#[path = "recovery/assembly.rs"]
mod assembly;
#[path = "recovery/functions.rs"]
mod functions;
#[path = "recovery/headers.rs"]
mod headers;
#[path = "recovery/revision_history.rs"]
mod revision_history;
#[path = "recovery/sources.rs"]
mod sources;
#[path = "recovery/standard.rs"]
mod standard;

pub(crate) use active_revision::recover_active_revision;
use active_revision::recover_client;
#[cfg(test)]
use assembly::{RecoveredCatalogueSemantics, assemble_catalogue_semantics, validate_function_type};
use assembly::{assemble_revision, load_catalogue_semantics};
use headers::{
    HashAlgorithm, RecoveredRevisionHeader, TextEncoding, catalogue_hash_context_for,
    decode_catalogue_hash_version, decode_durable_version, load_active_catalogue_hash_context,
    load_active_header, require_hash_contract,
};
pub use revision_history::RevisionPairHistoryEntry;
#[cfg(test)]
use revision_history::{decode_revision_pair_values, validate_revision_pair_listing};
use sources::load_source_units;
pub(crate) use standard::load_verified_standard_library;
#[cfg(test)]
use standard::{
    decode_standard_binding_target, recovered_standard_value_definition,
    verify_recovered_standard_snapshot,
};

#[cfg(feature = "test-hooks")]
use orna_core::canonical_hash::{
    verify_standard_library_snapshot as verify_structural_standard_library_snapshot,
    verify_standard_library_v2_snapshot as verify_structural_standard_library_v2_snapshot,
};
use orna_core::{
    CatalogueRevisionId, ExpressionId, FieldId, FunctionId, FunctionRevisionId, ParameterId,
    SchemaId, SourceBundleId, SourceRevisionId, SourceUnitId, StandardLibraryRevisionId,
    TypeBindingId, TypeId,
    canonical_hash::{
        artifact_payload_digest, catalogue_digest_with_context, source_bundle_digest,
        source_revision_digest, source_revision_record_digest, source_unit_content_digest,
    },
    catalogue::{
        CatalogueSnapshot, EnumTypeDefinition, FieldDefinition, FunctionDefinition, FunctionDomain,
        FunctionReturn, FunctionSecurity, FunctionTransaction, FunctionVolatility,
        ObjectTypeDefinition, OnDeleteAction, ParameterDefinition, PreludeTypeName,
        QualifiedSemanticName, RecordValueFieldDefinition, RecordValueTypeDefinition,
        SchemaDefinition, TypeBinding, TypeBindingKind, ValueTypeDefinition, ValueTypeKind,
        ValueTypeMutability, ValueTypePersistence,
    },
    revision::{
        ActiveDatabaseRevision, ActiveDatabaseRevisionInput, ActiveRevisionContent,
        CatalogueHashContext, CatalogueHashVersion, DefinitionIdentity, DefinitionOrigin,
        DefinitionReference, DefinitionReferenceKind, DefinitionReferenceTarget,
        ExecutableArtifact, ExecutableArtifactKind, ExpressionArtifact, FunctionRevisionRecord,
        FunctionSemanticHashVersion, RevisionPair, Sha256Digest, SourceOrigin, StandardExecutable,
        StandardLibraryDigestVersion, StandardLibrarySnapshot, StoredSourceRevision,
        StoredSourceUnit, VerifiedStandardLibrarySnapshot,
    },
    system::SYS_INSPECT_INVOCATION_TYPE_ID,
    types::{ResolvedType, StandardScalar, TypeDescriptor},
};

use orna_standard::{
    STANDARD_LIBRARY_REVISION_ID, STANDARD_LIBRARY_V2_REVISION_ID, STANDARD_LIBRARY_V3_REVISION_ID,
    STANDARD_LIBRARY_V4_REVISION_ID, STANDARD_LIBRARY_V5_REVISION_ID,
    STANDARD_LIBRARY_V6_REVISION_ID, STANDARD_LIBRARY_V7_REVISION_ID,
    STANDARD_LIBRARY_V8_REVISION_ID, STANDARD_LIBRARY_V9_REVISION_ID,
    verify_standard_library_snapshot, verify_standard_library_v2_snapshot,
    verify_standard_library_v3_snapshot, verify_standard_library_v4_snapshot,
    verify_standard_library_v5_snapshot, verify_standard_library_v6_snapshot,
    verify_standard_library_v7_snapshot, verify_standard_library_v8_snapshot,
    verify_standard_library_v9_snapshot,
};
use tokio_postgres::{Client, IsolationLevel, Row, Transaction};

use crate::{
    PostgresKernel, PostgresKernelError,
    bootstrap::require_current_migrations,
    decode::{
        DurableRecord, digest_bytes, exact_enum, identity_bytes, optional_identity_bytes,
        u32_from_i64, u64_from_i64,
    },
    is_sealed_inspect_type_id,
    physical::{establish_trusted_search_path, verify_physical_catalogue},
};

use self::functions::{
    RecoveredFunctionState, load_catalogue_current_revisions, load_catalogue_functions,
    load_function_state, load_references, validate_reference_sources,
};

const ACTIVE_RELATION: &str = "_orna_kernel.active_revision";
const SOURCE_UNIT_RELATION: &str = "_orna_kernel.source_units";
const SOURCE_REVISION_RELATION: &str = "_orna_kernel.source_revisions";
const CATALOGUE_REVISION_RELATION: &str = "_orna_kernel.catalogue_revisions";

struct RecoveredSchema {
    definition: SchemaDefinition,
    origin: DefinitionOrigin,
}

struct RecoveredObjectType {
    id: TypeId,
    schema: SchemaId,
    name: QualifiedSemanticName,
    origin: DefinitionOrigin,
}

struct RecoveredEnumType {
    schema: SchemaId,
    definition: EnumTypeDefinition,
    origin: DefinitionOrigin,
}

struct RecoveredRecordValueType {
    id: TypeId,
    schema: SchemaId,
    name: QualifiedSemanticName,
    origin: DefinitionOrigin,
}

struct RecoveredRecordValueField {
    owner: TypeId,
    definition: RecordValueFieldDefinition,
    origin: DefinitionOrigin,
}

struct RecordValueFieldTypeTuple {
    kind: Option<String>,
    value_type: Option<TypeId>,
    value_standard_library_revision: Option<StandardLibraryRevisionId>,
    application_enum_type: Option<TypeId>,
    enum_standard_library_revision: Option<StandardLibraryRevisionId>,
    standard_enum_type: Option<TypeId>,
    application_record_type: Option<TypeId>,
}

struct RecoveredField {
    owner: TypeId,
    definition: FieldDefinition,
    origin: DefinitionOrigin,
}

struct RecoveredExpression {
    artifact: ExpressionArtifact,
    origin: DefinitionOrigin,
}

impl PostgresKernel {
    /// Reconstructs and validates the complete active durable database revision.
    ///
    /// This recovery slice supports schemas, object and record value types,
    /// fields, expression artifacts, compiler-deployable functions, immutable
    /// function history, and active definition references. It fails closed on
    /// any semantic, source, hash-chain, or physical-layout state it cannot
    /// prove complete.
    pub async fn recover(&self) -> Result<ActiveDatabaseRevision, PostgresKernelError> {
        let mut session = self.open().await?;
        let recovery_result = recover_client(&mut session.client)
            .await
            .map_err(super::map_recovery_client_error);
        let shutdown_result = session.shutdown_for_source_apply().await;

        match (recovery_result, shutdown_result) {
            (Ok(active), Ok(())) => Ok(active),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }
}

async fn load_schemas(
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

async fn load_object_types(
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

async fn load_enum_types(
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

async fn load_record_value_types(
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

async fn load_record_value_fields(
    transaction: &Transaction<'_>,
    catalogue: CatalogueRevisionId,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<BTreeMap<TypeId, Vec<RecoveredRecordValueField>>, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT catalogue_revision_id, owner_type_id, field_id, name, ordinal,
                    type_kind, value_type_id, value_standard_library_revision_id,
                    enum_type_id, enum_standard_library_revision_id,
                    standard_enum_type_id, record_type_id,
                    source_unit_id, source_start, source_end
             FROM _orna_kernel.catalogue_record_value_fields
             WHERE catalogue_revision_id = $1
             ORDER BY owner_type_id, ordinal, field_id",
            &[&catalogue.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    let mut fields = BTreeMap::<TypeId, Vec<RecoveredRecordValueField>>::new();
    for (index, row) in rows.iter().enumerate() {
        let field = decode_record_value_field(row, index, catalogue, catalogue_hash_context)?;
        fields.entry(field.owner).or_default().push(field);
    }
    Ok(fields)
}

fn decode_record_value_field(
    row: &Row,
    row_index: usize,
    expected_catalogue: CatalogueRevisionId,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<RecoveredRecordValueField, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.catalogue_record_value_fields";
    let row_record = DurableRecord::new(RELATION, format!("row={row_index}"));
    require_catalogue_identity(row, &row_record, expected_catalogue, "record value field")?;
    let owner = TypeId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "owner_type_id",
            "record value field owner identity must be 16 bytes",
        )?,
        &row_record,
        "record value field owner identity must be 16 bytes",
    )?);
    let id = FieldId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "field_id",
            "record value field identity must be 16 bytes",
        )?,
        &row_record,
        "record value field identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(
        RELATION,
        format!("owner={} field={}", owner.canonical(), id.canonical()),
    );
    let name: String = record.column(row, "name", "record value field name must be text")?;
    if name.is_empty() {
        return Err(record.invariant("record value field name must not be empty"));
    }
    let ordinal = u32_from_i64(
        record.column(row, "ordinal", "record value field ordinal must fit u32")?,
        &record,
        "record value field ordinal must fit u32",
    )?;
    let kind: Option<String> = record.column(
        row,
        "type_kind",
        "record value field kind must be value, enum, or record",
    )?;
    let value_type = optional_identity_bytes(
        record.column(
            row,
            "value_type_id",
            "record value field standard type identity must be null or 16 bytes",
        )?,
        &record,
        "record value field standard type identity must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    let standard_library_revision = optional_identity_bytes(
        record.column(
            row,
            "value_standard_library_revision_id",
            "record value field standard revision must be null or 16 bytes",
        )?,
        &record,
        "record value field standard revision must be null or 16 bytes",
    )?
    .map(StandardLibraryRevisionId::from_bytes);
    let enum_type = optional_identity_bytes(
        record.column(
            row,
            "enum_type_id",
            "record value field enum identity must be null or 16 bytes",
        )?,
        &record,
        "record value field enum identity must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    let enum_standard_library_revision = optional_identity_bytes(
        record.column(
            row,
            "enum_standard_library_revision_id",
            "record value field standard enum revision must be null or 16 bytes",
        )?,
        &record,
        "record value field standard enum revision must be null or 16 bytes",
    )?
    .map(StandardLibraryRevisionId::from_bytes);
    let standard_enum_type = optional_identity_bytes(
        record.column(
            row,
            "standard_enum_type_id",
            "record value field standard enum identity must be null or 16 bytes",
        )?,
        &record,
        "record value field standard enum identity must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    let record_type = optional_identity_bytes(
        record.column(
            row,
            "record_type_id",
            "record value field record identity must be null or 16 bytes",
        )?,
        &record,
        "record value field record identity must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    let descriptor = decode_record_value_field_descriptor(
        RecordValueFieldTypeTuple {
            kind,
            value_type,
            value_standard_library_revision: standard_library_revision,
            application_enum_type: enum_type,
            enum_standard_library_revision,
            standard_enum_type,
            application_record_type: record_type,
        },
        catalogue_hash_context,
        &record,
    )?;
    let origin = decode_origin(row, &record, DefinitionIdentity::Field { owner, field: id })?;
    let definition = RecordValueFieldDefinition::try_new_descriptor(id, name, ordinal, descriptor)
        .map_err(|_| record.invariant("record value field tuple must use one flat descriptor"))?;

    Ok(RecoveredRecordValueField {
        owner,
        definition,
        origin,
    })
}

fn decode_record_value_field_descriptor(
    tuple: RecordValueFieldTypeTuple,
    catalogue_hash_context: &CatalogueHashContext,
    record: &DurableRecord,
) -> Result<TypeDescriptor, PostgresKernelError> {
    if tuple.enum_standard_library_revision.is_some() || tuple.standard_enum_type.is_some() {
        let (Some(standard_library_revision), Some(enum_type)) = (
            tuple.enum_standard_library_revision,
            tuple.standard_enum_type,
        ) else {
            return Err(record.invariant(
                "record value field type columns must form one exact pinned standard value, application enum, pinned standard enum, or application record tuple",
            ));
        };
        if tuple.kind.as_deref() != Some("enum")
            || tuple.value_type.is_some()
            || tuple.value_standard_library_revision.is_some()
            || tuple.application_enum_type.is_some()
            || tuple.application_record_type.is_some()
        {
            return Err(record.invariant(
                "record value field type columns must form one exact pinned standard value, application enum, pinned standard enum, or application record tuple",
            ));
        }
        let standard = catalogue_hash_context.standard().ok_or_else(|| {
            record.invariant(
                "record value field standard enum requires a version 2 catalogue context",
            )
        })?;
        if standard_library_revision != standard.revision() {
            return Err(record.invariant(
                "record value field standard enum revision must equal the selected catalogue pin",
            ));
        }
        if standard.catalogue().enum_type_by_id(enum_type).is_none() {
            return Err(record.invariant(
                "record value field standard enum must identify one enum in the selected pinned standard library",
            ));
        }
        return Ok(TypeDescriptor::named(enum_type));
    }

    let resolved_type = decode_resolved_type_tuple(
        ResolvedTypeTuple {
            kind: tuple.kind,
            scalar: None,
            target: None,
            value_type: tuple.value_type,
            standard_library_revision: tuple.value_standard_library_revision,
            enum_type: tuple.application_enum_type,
            record_type: tuple.application_record_type,
        },
        catalogue_hash_context,
        record,
        LegacyResolvedTypeTupleMember::Field,
    )?;
    match resolved_type {
        ResolvedType::Named(type_id) | ResolvedType::Value(type_id) => {
            Ok(TypeDescriptor::named(type_id))
        }
        ResolvedType::Scalar(_) | ResolvedType::Reference { .. } => Err(record
            .invariant("record value field tuple must decode to one named descriptor identity")),
    }
}

async fn load_fields(
    transaction: &Transaction<'_>,
    catalogue: CatalogueRevisionId,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<BTreeMap<TypeId, Vec<RecoveredField>>, PostgresKernelError> {
    let rows = if catalogue_hash_context.standard().is_some() {
        transaction
            .query(
                "SELECT catalogue_revision_id, owner_type_id, field_id, name, ordinal,
                        type_kind, scalar_type, target_type_id,
                        value_type_id, value_standard_library_revision_id,
                        enum_type_id, record_type_id,
                        nullable, is_unique, default_expression_id, on_delete,
                        source_unit_id, source_start, source_end
                 FROM _orna_kernel.catalogue_fields
                 WHERE catalogue_revision_id = $1
                 ORDER BY owner_type_id, ordinal, field_id",
                &[&catalogue.to_bytes().to_vec()],
            )
            .await
            .map_err(PostgresKernelError::Database)?
    } else {
        transaction
            .query(
                "SELECT catalogue_revision_id, owner_type_id, field_id, name, ordinal,
                        type_kind, scalar_type, target_type_id, nullable, is_unique,
                        default_expression_id, on_delete,
                        source_unit_id, source_start, source_end
                 FROM _orna_kernel.catalogue_fields
                 WHERE catalogue_revision_id = $1
                 ORDER BY owner_type_id, ordinal, field_id",
                &[&catalogue.to_bytes().to_vec()],
            )
            .await
            .map_err(PostgresKernelError::Database)?
    };

    let mut fields = BTreeMap::<TypeId, Vec<RecoveredField>>::new();
    for (index, row) in rows.iter().enumerate() {
        let field = decode_field(row, index, catalogue, catalogue_hash_context)?;
        fields.entry(field.owner).or_default().push(field);
    }
    Ok(fields)
}

/// One current SQL tuple member that stores a legacy resolved type.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum LegacyResolvedTypeTupleMember {
    Field,
    Parameter,
    ReturnColumn,
    SingleReturn,
    StreamReturn,
}

impl LegacyResolvedTypeTupleMember {
    pub(super) const fn tuple_rule(self) -> &'static str {
        match self {
            Self::Field => {
                "field type kind, scalar type, and target identity must form one exact supported tuple"
            }
            Self::Parameter => "parameter type columns must form one exact resolved type tuple",
            Self::ReturnColumn => {
                "return column type columns must form one exact resolved type tuple"
            }
            Self::SingleReturn => {
                "function return type columns must form one exact resolved type tuple"
            }
            Self::StreamReturn => {
                "stream item type columns must form one exact resolved type tuple"
            }
        }
    }

    const fn value_tuple_rule(self) -> &'static str {
        match self {
            Self::Field => {
                "field type kind and identity columns must form one exact supported scalar, object, value, enum, or record tuple"
            }
            Self::Parameter => {
                "parameter type columns must form one exact supported scalar, object, value, enum, or record tuple"
            }
            Self::ReturnColumn => {
                "return column type columns must form one exact supported scalar, object, value, enum, or record tuple"
            }
            Self::SingleReturn => {
                "function return type columns must form one exact supported scalar, object, value, enum, or record tuple"
            }
            Self::StreamReturn => {
                "stream item type columns must form one exact supported scalar, object, value, enum, or record tuple"
            }
        }
    }

    const fn scalar_rule(self) -> &'static str {
        match self {
            Self::Field => "field scalar type must be an exact standard scalar name",
            Self::Parameter | Self::ReturnColumn | Self::SingleReturn | Self::StreamReturn => {
                "resolved scalar type must be an exact standard scalar name"
            }
        }
    }

    const fn allows_void(self) -> bool {
        matches!(self, Self::Field | Self::SingleReturn)
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum LegacyResolvedTypeTupleKind {
    Scalar,
    Named,
    Reference,
}

/// The stored columns that describe one version-2 resolved type.
///
/// This is the only recovery projection that combines legacy type columns with
/// a standard value identity and its standard-library revision pin.
pub(super) struct ResolvedTypeTuple {
    pub(super) kind: Option<String>,
    pub(super) scalar: Option<String>,
    pub(super) target: Option<TypeId>,
    pub(super) value_type: Option<TypeId>,
    pub(super) standard_library_revision: Option<StandardLibraryRevisionId>,
    pub(super) enum_type: Option<TypeId>,
    pub(super) record_type: Option<TypeId>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum LegacyResolvedTypeTuple {
    Scalar(StandardScalar),
    Named(TypeId),
    Reference(TypeId),
}

impl LegacyResolvedTypeTuple {
    fn into_resolved_type(self) -> ResolvedType {
        match self {
            Self::Scalar(scalar) => ResolvedType::scalar(scalar),
            Self::Named(target) => ResolvedType::named(target),
            Self::Reference(target) => ResolvedType::reference(target),
        }
    }
}

/// Decodes the current scalar, named, or reference SQL kind before tuple data.
pub(super) fn decode_legacy_resolved_type_tuple_kind(
    value: Option<&str>,
    record: &DurableRecord,
    member: LegacyResolvedTypeTupleMember,
) -> Result<LegacyResolvedTypeTupleKind, PostgresKernelError> {
    let rule = if member == LegacyResolvedTypeTupleMember::Field {
        "field type kind must be scalar, named, or reference"
    } else {
        member.tuple_rule()
    };
    let value = value.ok_or_else(|| record.invariant(rule))?;
    exact_enum(
        value,
        &[
            ("scalar", LegacyResolvedTypeTupleKind::Scalar),
            ("named", LegacyResolvedTypeTupleKind::Named),
            ("reference", LegacyResolvedTypeTupleKind::Reference),
        ],
        record,
        rule,
    )
}

/// Decodes and projects one current legacy SQL resolved-type tuple.
///
/// The later value-tuple decoder remains separate. This decoder rejects every
/// value shape until that later recovery row explicitly enables it.
pub(super) fn decode_legacy_resolved_type_tuple(
    kind: LegacyResolvedTypeTupleKind,
    scalar: Option<&str>,
    target: Option<TypeId>,
    record: &DurableRecord,
    member: LegacyResolvedTypeTupleMember,
) -> Result<ResolvedType, PostgresKernelError> {
    if kind == LegacyResolvedTypeTupleKind::Scalar
        && let Some(name) = scalar
        && target.is_none()
    {
        return decode_legacy_scalar(name, record, member)
            .map(LegacyResolvedTypeTuple::Scalar)
            .map(LegacyResolvedTypeTuple::into_resolved_type);
    }
    if kind == LegacyResolvedTypeTupleKind::Named
        && scalar.is_none()
        && let Some(target) = target
    {
        if member == LegacyResolvedTypeTupleMember::Field {
            return Err(record.invariant("named field types are not supported by active recovery"));
        }
        return Ok(LegacyResolvedTypeTuple::Named(target).into_resolved_type());
    }
    if kind == LegacyResolvedTypeTupleKind::Reference
        && scalar.is_none()
        && let Some(target) = target
    {
        return Ok(LegacyResolvedTypeTuple::Reference(target).into_resolved_type());
    }
    Err(record.invariant(member.tuple_rule()))
}

/// Decodes one complete version-2 stored resolved-type tuple.
///
/// The selected catalogue context provides the one verified standard snapshot.
/// This function does not query or verify a second standard snapshot.
pub(super) fn decode_resolved_type_tuple(
    tuple: ResolvedTypeTuple,
    catalogue_hash_context: &CatalogueHashContext,
    record: &DurableRecord,
    member: LegacyResolvedTypeTupleMember,
) -> Result<ResolvedType, PostgresKernelError> {
    let standard = catalogue_hash_context.standard().ok_or_else(|| {
        record.invariant("resolved value type tuple requires a version 2 catalogue context")
    })?;

    if tuple.kind.as_deref() == Some("enum") {
        let Some(enum_type) = tuple.enum_type else {
            return Err(record.invariant(member.value_tuple_rule()));
        };
        if tuple.scalar.is_some()
            || tuple.target.is_some()
            || tuple.value_type.is_some()
            || tuple.standard_library_revision.is_some()
            || tuple.record_type.is_some()
        {
            return Err(record.invariant(member.value_tuple_rule()));
        }
        return Ok(ResolvedType::named(enum_type));
    }

    if tuple.kind.as_deref() == Some("record") {
        let Some(record_type) = tuple.record_type else {
            return Err(record.invariant(member.value_tuple_rule()));
        };
        if tuple.scalar.is_some()
            || tuple.target.is_some()
            || tuple.value_type.is_some()
            || tuple.standard_library_revision.is_some()
            || tuple.enum_type.is_some()
        {
            return Err(record.invariant(member.value_tuple_rule()));
        }
        return Ok(ResolvedType::named(record_type));
    }

    if tuple.kind.as_deref() == Some("value") {
        let Some(value_type) = tuple.value_type else {
            return Err(record.invariant(member.value_tuple_rule()));
        };
        if tuple.scalar.is_some()
            || tuple.target.is_some()
            || tuple.enum_type.is_some()
            || tuple.record_type.is_some()
        {
            return Err(record.invariant(member.value_tuple_rule()));
        }
        if is_sealed_inspect_type_id(value_type) {
            if !matches!(
                member,
                LegacyResolvedTypeTupleMember::Parameter
                    | LegacyResolvedTypeTupleMember::SingleReturn
                    | LegacyResolvedTypeTupleMember::StreamReturn
            ) {
                return Err(record.invariant(member.value_tuple_rule()));
            }
            if tuple.standard_library_revision.is_some() {
                return Err(record.invariant(
                    "sealed Inspector value types must not retain a standard library revision",
                ));
            }
            return Ok(ResolvedType::value(value_type));
        }
        let Some(standard_library_revision) = tuple.standard_library_revision else {
            return Err(record.invariant(member.value_tuple_rule()));
        };
        if standard_library_revision != standard.revision() {
            return Err(record.invariant(
                "resolved value type standard library revision must equal the selected catalogue pin",
            ));
        }
        if standard.catalogue().value_type_by_id(value_type).is_none() {
            return Err(record.invariant(
                "resolved value type must identify one value type in the selected pinned standard library",
            ));
        }
        return Ok(ResolvedType::value(value_type));
    }

    if tuple.value_type.is_some()
        || tuple.standard_library_revision.is_some()
        || tuple.enum_type.is_some()
        || tuple.record_type.is_some()
    {
        return Err(record.invariant(member.value_tuple_rule()));
    }
    let kind = decode_legacy_resolved_type_tuple_kind(tuple.kind.as_deref(), record, member)?;
    decode_legacy_resolved_type_tuple(kind, tuple.scalar.as_deref(), tuple.target, record, member)
}

fn decode_legacy_scalar(
    name: &str,
    record: &DurableRecord,
    member: LegacyResolvedTypeTupleMember,
) -> Result<StandardScalar, PostgresKernelError> {
    let scalar = exact_enum(
        name,
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
        member.scalar_rule(),
    )?;
    if scalar == StandardScalar::Void && !member.allows_void() {
        return Err(record.invariant(
            "void is valid only as a SINGLE function return, never as a parameter or ROWS column",
        ));
    }
    Ok(scalar)
}

fn decode_field(
    row: &Row,
    row_index: usize,
    expected_catalogue: CatalogueRevisionId,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<RecoveredField, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.catalogue_fields";
    let row_record = DurableRecord::new(RELATION, format!("row={row_index}"));
    require_catalogue_identity(row, &row_record, expected_catalogue, "field")?;
    let owner = TypeId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "owner_type_id",
            "field owner identity must be 16 bytes",
        )?,
        &row_record,
        "field owner identity must be 16 bytes",
    )?);
    let id = FieldId::from_bytes(identity_bytes(
        row_record.column(row, "field_id", "field identity must be 16 bytes")?,
        &row_record,
        "field identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(
        RELATION,
        format!("owner={} field={}", owner.canonical(), id.canonical()),
    );
    let name: String = record.column(row, "name", "field name must be PostgreSQL text")?;
    if name.is_empty() {
        return Err(record.invariant("field name must not be empty"));
    }
    let ordinal = u32_from_i64(
        record.column(row, "ordinal", "field ordinal must fit u32")?,
        &record,
        "field ordinal must fit u32",
    )?;
    let resolved_type = if catalogue_hash_context.standard().is_some() {
        decode_version_two_field_type_columns(row, &record, catalogue_hash_context)?
    } else {
        decode_legacy_field_type_columns(row, &record)?
    };
    let nullable: bool = record.column(row, "nullable", "field nullability must be boolean")?;
    let unique: bool = record.column(row, "is_unique", "field uniqueness must be boolean")?;
    let default_expression = optional_identity_bytes(
        record.column(
            row,
            "default_expression_id",
            "field default expression identity must be null or 16 bytes",
        )?,
        &record,
        "field default expression identity must be null or 16 bytes",
    )?
    .map(ExpressionId::from_bytes);
    let delete_name: Option<String> = record.column(
        row,
        "on_delete",
        "field delete action must be null, restrict, set_null, or cascade",
    )?;
    let on_delete = decode_on_delete(delete_name.as_deref(), resolved_type, nullable, &record)?;
    let origin = decode_origin(row, &record, DefinitionIdentity::Field { owner, field: id })?;

    Ok(RecoveredField {
        owner,
        definition: FieldDefinition::new(
            id,
            name,
            ordinal,
            resolved_type,
            nullable,
            unique,
            default_expression,
            on_delete,
        ),
        origin,
    })
}

fn decode_legacy_field_type_columns(
    row: &Row,
    record: &DurableRecord,
) -> Result<ResolvedType, PostgresKernelError> {
    let kind_name: String = record.column(
        row,
        "type_kind",
        "field type kind must be scalar, named, or reference",
    )?;
    let kind = decode_legacy_resolved_type_tuple_kind(
        Some(&kind_name),
        record,
        LegacyResolvedTypeTupleMember::Field,
    )?;
    let scalar_name: Option<String> = record.column(
        row,
        "scalar_type",
        "field scalar type must be null or an exact standard scalar name",
    )?;
    let target = optional_identity_bytes(
        record.column(
            row,
            "target_type_id",
            "field target identity must be null or 16 bytes",
        )?,
        record,
        "field target identity must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    decode_legacy_resolved_type_tuple(
        kind,
        scalar_name.as_deref(),
        target,
        record,
        LegacyResolvedTypeTupleMember::Field,
    )
}

fn decode_version_two_field_type_columns(
    row: &Row,
    record: &DurableRecord,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<ResolvedType, PostgresKernelError> {
    let kind: Option<String> = record.column(
        row,
        "type_kind",
        "field type kind must be scalar, named, reference, value, or enum",
    )?;
    let scalar: Option<String> = record.column(
        row,
        "scalar_type",
        "field scalar type must be null or an exact standard scalar name",
    )?;
    let target = optional_identity_bytes(
        record.column(
            row,
            "target_type_id",
            "field target identity must be null or 16 bytes",
        )?,
        record,
        "field target identity must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    let value_type = optional_identity_bytes(
        record.column(
            row,
            "value_type_id",
            "field value type identity must be null or 16 bytes",
        )?,
        record,
        "field value type identity must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    let standard_library_revision = optional_identity_bytes(
        record.column(
            row,
            "value_standard_library_revision_id",
            "field value type standard library revision identity must be null or 16 bytes",
        )?,
        record,
        "field value type standard library revision identity must be null or 16 bytes",
    )?
    .map(StandardLibraryRevisionId::from_bytes);
    let enum_type = optional_identity_bytes(
        record.column(
            row,
            "enum_type_id",
            "field enum type identity must be null or 16 bytes",
        )?,
        record,
        "field enum type identity must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    let record_type = optional_identity_bytes(
        record.column(
            row,
            "record_type_id",
            "field record type identity must be null or 16 bytes",
        )?,
        record,
        "field record type identity must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    decode_resolved_type_tuple(
        ResolvedTypeTuple {
            kind,
            scalar,
            target,
            value_type,
            standard_library_revision,
            enum_type,
            record_type,
        },
        catalogue_hash_context,
        record,
        LegacyResolvedTypeTupleMember::Field,
    )
}

fn decode_on_delete(
    value: Option<&str>,
    resolved_type: ResolvedType,
    nullable: bool,
    record: &DurableRecord,
) -> Result<Option<OnDeleteAction>, PostgresKernelError> {
    if resolved_type.reference_target().is_none() {
        return value
            .is_none()
            .then_some(None)
            .ok_or_else(|| record.invariant("only reference fields may declare a delete action"));
    }
    let action = match value {
        None => None,
        Some("restrict") => Some(OnDeleteAction::Restrict),
        Some("set_null") => Some(OnDeleteAction::SetNull),
        Some("cascade") => Some(OnDeleteAction::Cascade),
        Some(_) => {
            return Err(record.invariant(
                "reference delete action must be null, restrict, set_null, or cascade",
            ));
        }
    };
    if action == Some(OnDeleteAction::SetNull) && !nullable {
        return Err(record.invariant("SET NULL reference fields must be nullable"));
    }
    Ok(action)
}

async fn load_expressions(
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

fn require_catalogue_identity(
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

fn decode_origin(
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

#[cfg(test)]
#[path = "recovery/tests.rs"]
mod tests;
