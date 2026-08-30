//! Catalogue and active revision assembly after durable rows are decoded.

use super::*;

pub(super) struct RecoveredCatalogueSemantics {
    pub(super) catalogue: CatalogueSnapshot,
    pub(super) expressions: Vec<ExpressionArtifact>,
    pub(super) origins: Vec<DefinitionOrigin>,
}

pub(super) async fn load_catalogue_semantics(
    transaction: &Transaction<'_>,
    catalogue: CatalogueRevisionId,
    functions: Vec<functions::RecoveredFunction>,
    function_origins: Vec<DefinitionOrigin>,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<RecoveredCatalogueSemantics, PostgresKernelError> {
    assemble_catalogue_semantics(
        catalogue,
        load_schemas(transaction, catalogue).await?,
        load_object_types(transaction, catalogue).await?,
        load_enum_types(transaction, catalogue).await?,
        load_record_value_types(transaction, catalogue).await?,
        load_fields(transaction, catalogue, catalogue_hash_context).await?,
        load_record_value_fields(transaction, catalogue, catalogue_hash_context).await?,
        load_expressions(transaction, catalogue).await?,
        functions,
        function_origins,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn assemble_catalogue_semantics(
    catalogue_id: CatalogueRevisionId,
    schemas: Vec<RecoveredSchema>,
    objects: Vec<RecoveredObjectType>,
    enum_types: Vec<RecoveredEnumType>,
    record_value_types: Vec<RecoveredRecordValueType>,
    mut fields: BTreeMap<TypeId, Vec<RecoveredField>>,
    mut record_value_fields: BTreeMap<TypeId, Vec<RecoveredRecordValueField>>,
    expressions: Vec<RecoveredExpression>,
    functions: Vec<functions::RecoveredFunction>,
    mut function_origins: Vec<DefinitionOrigin>,
) -> Result<RecoveredCatalogueSemantics, PostgresKernelError> {
    let schema_names = schemas
        .iter()
        .map(|schema| (schema.definition.id(), schema.definition.name().clone()))
        .collect::<BTreeMap<_, _>>();
    let mut origins = Vec::new();
    let schemas = schemas
        .into_iter()
        .map(|schema| {
            origins.push(schema.origin);
            schema.definition
        })
        .collect::<Vec<_>>();
    let mut object_definitions = Vec::with_capacity(objects.len());
    for object in objects {
        let record =
            DurableRecord::new("_orna_kernel.catalogue_object_types", object.id.canonical());
        let schema_name = schema_names.get(&object.schema).ok_or_else(|| {
            record.invariant("object stored schema identity must identify a recovered schema")
        })?;
        let object_parts = object.name.parts();
        let namespace = object_parts
            .get(..object_parts.len().saturating_sub(1))
            .filter(|parts| !parts.is_empty())
            .ok_or_else(|| {
                record.invariant("object qualified name must contain a schema namespace")
            })?;
        if namespace != schema_name.parts() {
            return Err(record.invariant(
                "object stored schema identity must equal the schema named by its namespace",
            ));
        }

        let recovered_fields = fields.remove(&object.id).unwrap_or_default();
        let mut definitions = Vec::with_capacity(recovered_fields.len());
        for field in recovered_fields {
            origins.push(field.origin);
            definitions.push(field.definition);
        }
        origins.push(object.origin);
        object_definitions.push(ObjectTypeDefinition::new(
            object.id,
            object.name,
            definitions,
        ));
    }
    if let Some((owner, _)) = fields.first_key_value() {
        return Err(DurableRecord::new(
            "_orna_kernel.catalogue_fields",
            format!("owner={}", owner.canonical()),
        )
        .invariant("every recovered field owner must be an active object type"));
    }

    let mut enum_definitions = Vec::with_capacity(enum_types.len());
    for enum_type in enum_types {
        let record = DurableRecord::new(
            "_orna_kernel.catalogue_enum_types",
            enum_type.definition.id().canonical(),
        );
        let schema_name = schema_names.get(&enum_type.schema).ok_or_else(|| {
            record.invariant("enum stored schema identity must identify a recovered schema")
        })?;
        let parts = enum_type.definition.name().parts();
        let namespace = parts
            .get(..parts.len().saturating_sub(1))
            .filter(|parts| !parts.is_empty())
            .ok_or_else(|| {
                record.invariant("enum qualified name must contain a schema namespace")
            })?;
        if namespace != schema_name.parts() {
            return Err(record.invariant(
                "enum stored schema identity must equal the schema named by its namespace",
            ));
        }
        origins.push(enum_type.origin);
        enum_definitions.push(enum_type.definition);
    }

    let mut record_value_definitions = Vec::with_capacity(record_value_types.len());
    for record_value_type in record_value_types {
        let record = DurableRecord::new(
            "_orna_kernel.catalogue_record_value_types",
            record_value_type.id.canonical(),
        );
        let schema_name = schema_names.get(&record_value_type.schema).ok_or_else(|| {
            record.invariant("record value stored schema identity must identify a recovered schema")
        })?;
        let parts = record_value_type.name.parts();
        let namespace = parts
            .get(..parts.len().saturating_sub(1))
            .filter(|parts| !parts.is_empty())
            .ok_or_else(|| {
                record.invariant("record value qualified name must contain a schema namespace")
            })?;
        if namespace != schema_name.parts() {
            return Err(record.invariant(
                "record value stored schema identity must equal the schema named by its namespace",
            ));
        }

        let recovered_fields = record_value_fields
            .remove(&record_value_type.id)
            .unwrap_or_default();
        let mut definitions = Vec::with_capacity(recovered_fields.len());
        for field in recovered_fields {
            origins.push(field.origin);
            definitions.push(field.definition);
        }
        origins.push(record_value_type.origin);
        record_value_definitions.push(RecordValueTypeDefinition::new(
            record_value_type.id,
            record_value_type.name,
            definitions,
        ));
    }
    if let Some((owner, _)) = record_value_fields.first_key_value() {
        return Err(DurableRecord::new(
            "_orna_kernel.catalogue_record_value_fields",
            format!("owner={}", owner.canonical()),
        )
        .invariant("every recovered record field owner must be an active record value type"));
    }

    let mut expression_artifacts = Vec::with_capacity(expressions.len());
    for expression in expressions {
        origins.push(expression.origin);
        expression_artifacts.push(expression.artifact);
    }
    let mut function_definitions = Vec::with_capacity(functions.len());
    for function in functions {
        let record = DurableRecord::new(
            "_orna_kernel.catalogue_functions",
            function.definition.id().canonical(),
        );
        let schema_name = schema_names.get(&function.schema).ok_or_else(|| {
            record.invariant("function stored schema identity must identify a recovered schema")
        })?;
        let parts = function.definition.name().parts();
        let namespace = parts
            .get(..parts.len().saturating_sub(1))
            .filter(|parts| !parts.is_empty())
            .ok_or_else(|| {
                record.invariant("function qualified name must contain a schema namespace")
            })?;
        if namespace != schema_name.parts() {
            return Err(record.invariant(
                "function stored schema identity must equal the schema named by its namespace",
            ));
        }
        function_definitions.push(function.definition);
    }
    origins.append(&mut function_origins);
    let catalogue = CatalogueSnapshot::new_with_functions_and_record_value_types(
        catalogue_id,
        schemas,
        object_definitions,
        Vec::new(),
        enum_definitions,
        record_value_definitions,
        Vec::new(),
        function_definitions,
    )
    .map_err(PostgresKernelError::CatalogueSnapshot)?;
    validate_field_links(&catalogue, &expression_artifacts)?;
    validate_function_links(&catalogue, &expression_artifacts)?;
    Ok(RecoveredCatalogueSemantics {
        catalogue,
        expressions: expression_artifacts,
        origins,
    })
}

pub(super) fn assemble_revision(
    header: RecoveredRevisionHeader,
    units: Vec<StoredSourceUnit>,
    semantics: RecoveredCatalogueSemantics,
    function_state: RecoveredFunctionState,
    catalogue_hash_context: CatalogueHashContext,
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

    let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            RevisionPair::new(header.source, header.catalogue),
            source,
            semantics.catalogue,
            header.catalogue_hash,
            ActiveRevisionContent::new(
                semantics.expressions,
                function_state.active_revisions,
                semantics.origins,
                function_state.references,
            )
            .with_history(function_state.historical_revisions),
        ),
        catalogue_hash_context,
    )
    .map_err(PostgresKernelError::RevisionInvariant)?;
    let computed_catalogue_hash = catalogue_digest_with_context(
        active.catalogue_hash_context(),
        active.catalogue(),
        active.function_revisions(),
        active.expressions(),
        active.origins(),
        active.references(),
    )
    .map_err(PostgresKernelError::CanonicalHash)?;
    if computed_catalogue_hash != active.catalogue_hash() {
        let catalogue_record = DurableRecord::new(
            "_orna_kernel.catalogue_revisions",
            header.catalogue.canonical(),
        );
        return Err(catalogue_record
            .invariant("catalogue digest must match the exact recovered semantic catalogue"));
    }

    if let Some(introduction) = function_state.introductions.get(&header.catalogue)
        && (introduction.catalogue_hash != active.catalogue_hash()
            || introduction.source.id() != active.source().id())
    {
        return Err(DurableRecord::new(
            "_orna_kernel.catalogue_revisions",
            header.catalogue.canonical(),
        )
        .invariant(
            "active function introduction must join the exact validated catalogue and source hashes",
        ));
    }

    Ok(active)
}

fn validate_function_links(
    catalogue: &CatalogueSnapshot,
    expressions: &[ExpressionArtifact],
) -> Result<(), PostgresKernelError> {
    let expression_ids = expressions
        .iter()
        .map(ExpressionArtifact::id)
        .collect::<BTreeSet<_>>();
    for function in catalogue.functions() {
        for parameter in function.parameters() {
            let record = DurableRecord::new(
                "_orna_kernel.catalogue_function_parameters",
                format!(
                    "function={} parameter={}",
                    function.id().canonical(),
                    parameter.id().canonical()
                ),
            );
            validate_function_type(catalogue, parameter.resolved_type(), &record)?;
            if let Some(expression) = parameter.default_expression()
                && !expression_ids.contains(&expression)
            {
                return Err(record.invariant(
                    "every parameter default must identify a recovered expression artifact",
                ));
            }
        }
        match function.return_type() {
            orna_core::catalogue::FunctionReturn::Single(resolved_type)
            | orna_core::catalogue::FunctionReturn::Stream(resolved_type) => {
                validate_function_type(
                    catalogue,
                    *resolved_type,
                    &DurableRecord::new(
                        "_orna_kernel.catalogue_functions",
                        function.id().canonical(),
                    ),
                )?;
            }
            orna_core::catalogue::FunctionReturn::Rows(columns) => {
                for column in columns {
                    validate_function_type(
                        catalogue,
                        column.resolved_type(),
                        &DurableRecord::new(
                            "_orna_kernel.catalogue_function_return_columns",
                            format!(
                                "function={} ordinal={}",
                                function.id().canonical(),
                                column.ordinal()
                            ),
                        ),
                    )?;
                }
            }
        }
    }
    Ok(())
}

pub(super) fn validate_function_type(
    catalogue: &CatalogueSnapshot,
    resolved_type: ResolvedType,
    record: &DurableRecord,
) -> Result<(), PostgresKernelError> {
    if resolved_type.legacy_scalar().is_some() {
        return Ok(());
    }
    if let Some(target) = resolved_type.named_type() {
        if catalogue.object_type_by_id(target).is_none()
            && catalogue.enum_type_by_id(target).is_none()
            && catalogue.record_value_type_by_id(target).is_none()
        {
            return Err(record.invariant(
                "every named function type target must be an active object, enum, or record type",
            ));
        }
        return Ok(());
    }
    if let Some(target) = resolved_type.reference_target() {
        if target == SYS_INSPECT_INVOCATION_TYPE_ID {
            return Ok(());
        }
        if catalogue.object_type_by_id(target).is_none() {
            return Err(record
                .invariant("every reference function type target must be an active object type"));
        }
        return Ok(());
    }
    if resolved_type.value_type().is_some() {
        return Ok(());
    }
    Err(record.invariant("function resolved types are not supported by active recovery"))
}

fn validate_field_links(
    catalogue: &CatalogueSnapshot,
    expressions: &[ExpressionArtifact],
) -> Result<(), PostgresKernelError> {
    let expression_ids = expressions
        .iter()
        .map(ExpressionArtifact::id)
        .collect::<BTreeSet<_>>();
    for object in catalogue.object_types() {
        for field in object.fields() {
            let record = DurableRecord::new(
                "_orna_kernel.catalogue_fields",
                format!(
                    "owner={} field={}",
                    object.id().canonical(),
                    field.id().canonical()
                ),
            );
            if let Some(target) = field.resolved_type().reference_target()
                && catalogue.object_type_by_id(target).is_none()
            {
                return Err(
                    record.invariant("every reference field target must be an active object type")
                );
            }
            if let Some(expression) = field.default_expression()
                && !expression_ids.contains(&expression)
            {
                return Err(record.invariant(
                    "every field default must identify a recovered expression artifact",
                ));
            }
        }
    }
    Ok(())
}
