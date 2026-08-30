//! Durable definition-reference recovery and source validation.

use super::*;

pub(in super::super) async fn load_references(
    transaction: &Transaction<'_>,
    catalogue: CatalogueRevisionId,
    expected_standard_library_revision: Option<StandardLibraryRevisionId>,
) -> Result<Vec<DefinitionReference>, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT catalogue_revision_id, source_function_id,
                    source_function_revision_id, ordinal,
                    target_definition_id, target_kind, reference_kind,
                    source_subobject_id, target_owner_type_id,
                    target_owner_function_id, target_standard_library_revision_id,
                    target_enum_catalogue_revision_id,
                    target_record_catalogue_revision_id,
                    target_record_field_catalogue_revision_id,
                    target_record_field_owner_type_id,
                    source_unit_id, source_start, source_end
             FROM _orna_kernel.definition_references
             WHERE catalogue_revision_id = $1
             ORDER BY source_function_revision_id, ordinal",
            &[&catalogue.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let mut references = Vec::with_capacity(rows.len());
    let mut expected_ordinals = BTreeMap::<FunctionRevisionId, u32>::new();
    for (index, row) in rows.iter().enumerate() {
        let reference =
            decode_reference(row, index, catalogue, expected_standard_library_revision)?;
        let expected = expected_ordinals
            .entry(reference.source_revision())
            .or_default();
        if reference.ordinal() != *expected {
            return Err(DurableRecord::new(
                REFERENCE_RELATION,
                format!(
                    "revision={} ordinal={}",
                    reference.source_revision().canonical(),
                    reference.ordinal()
                ),
            )
            .invariant("definition reference ordinals must be contiguous from zero"));
        }
        *expected = expected.checked_add(1).ok_or_else(|| {
            DurableRecord::new(REFERENCE_RELATION, reference.source_revision().canonical())
                .invariant("definition reference ordinal count must fit u32")
        })?;
        references.push(reference);
    }
    Ok(references)
}

fn decode_reference(
    row: &Row,
    index: usize,
    catalogue: CatalogueRevisionId,
    expected_standard_library_revision: Option<StandardLibraryRevisionId>,
) -> Result<DefinitionReference, PostgresKernelError> {
    let row_record = DurableRecord::new(REFERENCE_RELATION, format!("row={index}"));
    require_catalogue(row, &row_record, catalogue, "reference")?;
    let source_function = FunctionId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "source_function_id",
            "reference source function identity must be 16 bytes",
        )?,
        &row_record,
        "reference source function identity must be 16 bytes",
    )?);
    let source_revision = FunctionRevisionId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "source_function_revision_id",
            "reference source revision identity must be 16 bytes",
        )?,
        &row_record,
        "reference source revision identity must be 16 bytes",
    )?);
    let ordinal = u32_from_i64(
        row_record.column(row, "ordinal", "reference ordinal must fit u32")?,
        &row_record,
        "reference ordinal must fit u32",
    )?;
    let record = DurableRecord::new(
        REFERENCE_RELATION,
        format!("revision={} ordinal={ordinal}", source_revision.canonical()),
    );
    let source_subobject: Option<Vec<u8>> = record.column(
        row,
        "source_subobject_id",
        "reference source subobject identity must be null",
    )?;
    if source_subobject.is_some() {
        return Err(record.invariant(
            "compiler-deployable definition references must not contain a stored source subobject",
        ));
    }
    let target_bytes = identity_bytes(
        record.column(
            row,
            "target_definition_id",
            "reference target identity must be 16 bytes",
        )?,
        &record,
        "reference target identity must be 16 bytes",
    )?;
    let owner_type = optional_identity_bytes(
        record.column(
            row,
            "target_owner_type_id",
            "reference target type owner must be null or 16 bytes",
        )?,
        &record,
        "reference target type owner must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    let owner_function = optional_identity_bytes(
        record.column(
            row,
            "target_owner_function_id",
            "reference target function owner must be null or 16 bytes",
        )?,
        &record,
        "reference target function owner must be null or 16 bytes",
    )?
    .map(FunctionId::from_bytes);
    let target_standard_library_revision = optional_identity_bytes(
        record.column(
            row,
            "target_standard_library_revision_id",
            "reference target standard library revision identity must be null or 16 bytes",
        )?,
        &record,
        "reference target standard library revision identity must be null or 16 bytes",
    )?
    .map(StandardLibraryRevisionId::from_bytes);
    let target_enum_catalogue_revision = optional_identity_bytes(
        record.column(
            row,
            "target_enum_catalogue_revision_id",
            "reference target enum catalogue revision identity must be null or 16 bytes",
        )?,
        &record,
        "reference target enum catalogue revision identity must be null or 16 bytes",
    )?
    .map(CatalogueRevisionId::from_bytes);
    let target_record_catalogue_revision = optional_identity_bytes(
        record.column(
            row,
            "target_record_catalogue_revision_id",
            "reference target record catalogue revision identity must be null or 16 bytes",
        )?,
        &record,
        "reference target record catalogue revision identity must be null or 16 bytes",
    )?
    .map(CatalogueRevisionId::from_bytes);
    let target_record_field_catalogue_revision = optional_identity_bytes(
        record.column(
            row,
            "target_record_field_catalogue_revision_id",
            "reference target record field catalogue revision identity must be null or 16 bytes",
        )?,
        &record,
        "reference target record field catalogue revision identity must be null or 16 bytes",
    )?
    .map(CatalogueRevisionId::from_bytes);
    let target_record_field_owner_type = optional_identity_bytes(
        record.column(
            row,
            "target_record_field_owner_type_id",
            "reference target record field owner must be null or 16 bytes",
        )?,
        &record,
        "reference target record field owner must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    let target_kind: String =
        record.column(row, "target_kind", "reference target kind must decode")?;
    let target_type = TypeId::from_bytes(target_bytes);
    let target = match (
        target_kind.as_str(),
        owner_type,
        owner_function,
        target_standard_library_revision,
        target_enum_catalogue_revision,
        target_record_catalogue_revision,
        target_record_field_catalogue_revision,
        target_record_field_owner_type,
    ) {
        ("object_type", None, None, None, None, None, None, None) => {
            DefinitionReferenceTarget::ObjectType(TypeId::from_bytes(target_bytes))
        }
        ("field", Some(owner), None, None, None, None, None, None) => {
            DefinitionReferenceTarget::Field {
                owner,
                field: orna_core::FieldId::from_bytes(target_bytes),
            }
        }
        ("record_field", None, None, None, None, None, Some(revision), Some(owner))
            if revision == catalogue =>
        {
            DefinitionReferenceTarget::Field {
                owner,
                field: orna_core::FieldId::from_bytes(target_bytes),
            }
        }
        ("function", None, None, None, None, None, None, None) => {
            DefinitionReferenceTarget::Function(FunctionId::from_bytes(target_bytes))
        }
        ("parameter", None, Some(owner), None, None, None, None, None) => {
            DefinitionReferenceTarget::Parameter {
                owner,
                parameter: ParameterId::from_bytes(target_bytes),
            }
        }
        ("expression", None, None, None, None, None, None, None) => {
            DefinitionReferenceTarget::Expression(ExpressionId::from_bytes(target_bytes))
        }
        ("value_type", None, None, None, None, None, None, None)
            if is_sealed_inspect_type_id(target_type) =>
        {
            DefinitionReferenceTarget::ValueType(target_type)
        }
        ("value_type", None, None, Some(revision), None, None, None, None)
            if !is_sealed_inspect_type_id(target_type)
                && Some(revision) == expected_standard_library_revision =>
        {
            DefinitionReferenceTarget::ValueType(target_type)
        }
        ("enum_type", None, None, None, Some(revision), None, None, None)
            if revision == catalogue =>
        {
            DefinitionReferenceTarget::ValueType(TypeId::from_bytes(target_bytes))
        }
        ("record_type", None, None, None, None, Some(revision), None, None)
            if revision == catalogue =>
        {
            DefinitionReferenceTarget::ValueType(TypeId::from_bytes(target_bytes))
        }
        _ => {
            return Err(record.invariant(
                "reference target kind and owner columns must form one exact owner-qualified target",
            ));
        }
    };
    let kind_name: String = record.column(row, "reference_kind", "reference kind must decode")?;
    let kind = decode_reference_kind(&kind_name, &record)?;
    if !reference_kind_matches_target(kind, target) {
        return Err(
            record.invariant("reference kind must be compatible with its exact target kind")
        );
    }
    let source_origin = decode_reference_origin(row, &record)?;
    Ok(DefinitionReference::new(
        source_function,
        source_revision,
        ordinal,
        target,
        kind,
        source_origin,
    ))
}

pub(super) fn decode_reference_kind(
    name: &str,
    record: &DurableRecord,
) -> Result<DefinitionReferenceKind, PostgresKernelError> {
    exact_enum(
        name,
        SUPPORTED_REFERENCE_KINDS,
        record,
        "reference kind must be one exact supported semantic relation",
    )
}

pub(super) const SUPPORTED_REFERENCE_KINDS: &[(&str, DefinitionReferenceKind)] = &[
    ("function_call", DefinitionReferenceKind::FunctionCall),
    ("named_type", DefinitionReferenceKind::NamedType),
    ("object_reference", DefinitionReferenceKind::ObjectReference),
    ("parameter_read", DefinitionReferenceKind::ParameterRead),
    ("query_object", DefinitionReferenceKind::QueryObject),
    ("query_field", DefinitionReferenceKind::QueryField),
    ("expression", DefinitionReferenceKind::Expression),
    ("write_object", DefinitionReferenceKind::WriteObject),
    ("write_field", DefinitionReferenceKind::WriteField),
];

fn decode_reference_origin(
    row: &Row,
    record: &DurableRecord,
) -> Result<SourceOrigin, PostgresKernelError> {
    let unit = SourceUnitId::from_bytes(identity_bytes(
        record.column(
            row,
            "source_unit_id",
            "reference source unit identity must be 16 bytes",
        )?,
        record,
        "reference source unit identity must be 16 bytes",
    )?);
    let start = u32_from_i64(
        record.column(row, "source_start", "reference source start must fit u32")?,
        record,
        "reference source start must fit u32",
    )?;
    let end = u32_from_i64(
        record.column(row, "source_end", "reference source end must fit u32")?,
        record,
        "reference source end must fit u32",
    )?;
    SourceOrigin::new(unit, start, end).map_err(PostgresKernelError::RevisionInvariant)
}

pub(super) const fn reference_kind_matches_target(
    kind: DefinitionReferenceKind,
    target: DefinitionReferenceTarget,
) -> bool {
    matches!(
        (kind, target),
        (
            DefinitionReferenceKind::FunctionCall,
            DefinitionReferenceTarget::Function(_)
        ) | (
            DefinitionReferenceKind::NamedType
                | DefinitionReferenceKind::ObjectReference
                | DefinitionReferenceKind::QueryObject,
            DefinitionReferenceTarget::ObjectType(_)
        ) | (
            DefinitionReferenceKind::NamedType,
            DefinitionReferenceTarget::ValueType(_)
        ) | (
            DefinitionReferenceKind::ParameterRead,
            DefinitionReferenceTarget::Parameter { .. }
        ) | (
            DefinitionReferenceKind::QueryField,
            DefinitionReferenceTarget::Field { .. }
        ) | (
            DefinitionReferenceKind::Expression,
            DefinitionReferenceTarget::Expression(_)
        ) | (
            DefinitionReferenceKind::WriteObject,
            DefinitionReferenceTarget::ObjectType(_)
        ) | (
            DefinitionReferenceKind::WriteField,
            DefinitionReferenceTarget::Field { .. }
        )
    )
}

pub(in super::super) fn validate_reference_sources(
    functions: &[RecoveredFunction],
    references: &[DefinitionReference],
) -> Result<(), PostgresKernelError> {
    let current = functions
        .iter()
        .map(|function| {
            (
                function.definition.id(),
                function.definition.current_revision(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for reference in references {
        let record = DurableRecord::new(
            REFERENCE_RELATION,
            format!(
                "revision={} ordinal={}",
                reference.source_revision().canonical(),
                reference.ordinal()
            ),
        );
        if current.get(&reference.source_function()) != Some(&reference.source_revision()) {
            return Err(record.invariant(
                "reference source function and revision must be the active current pair",
            ));
        }
    }
    Ok(())
}
