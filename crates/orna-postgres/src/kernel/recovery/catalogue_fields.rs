//! Durable catalogue field and resolved-type tuple recovery.

use super::*;

pub(super) struct RecoveredRecordValueField {
    pub(super) owner: TypeId,
    pub(super) definition: RecordValueFieldDefinition,
    pub(super) origin: DefinitionOrigin,
}

pub(super) struct RecordValueFieldTypeTuple {
    pub(super) kind: Option<String>,
    pub(super) value_type: Option<TypeId>,
    pub(super) value_standard_library_revision: Option<StandardLibraryRevisionId>,
    pub(super) application_enum_type: Option<TypeId>,
    pub(super) enum_standard_library_revision: Option<StandardLibraryRevisionId>,
    pub(super) standard_enum_type: Option<TypeId>,
    pub(super) application_record_type: Option<TypeId>,
}

pub(super) struct RecoveredField {
    pub(super) owner: TypeId,
    pub(super) definition: FieldDefinition,
    pub(super) origin: DefinitionOrigin,
}

pub(super) async fn load_record_value_fields(
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

pub(super) fn decode_record_value_field_descriptor(
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

pub(super) async fn load_fields(
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
