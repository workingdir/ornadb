//! Persistence of candidate source, catalogue, and revision state.

use super::*;

pub(super) async fn persist_candidate(
    transaction: &Transaction<'_>,
    candidate: &DeployableRevision,
    encoder: &CandidateEncoder<'_>,
) -> Result<(), PostgresKernelError> {
    let source = candidate.source();
    persist_source(transaction, source, false).await?;
    transaction
        .execute(
            "INSERT INTO _orna_kernel.catalogue_revisions
                (id, source_revision_id, parent_catalogue_revision_id, content_hash,
                 hash_algorithm, hash_contract_version, canonical_hash_version,
                 standard_library_revision_id)
             VALUES ($1, $2, $3, $4, 'sha256', $5, $6, $7)",
            &[
                &bytes(candidate.candidate().revision()),
                &bytes(source.id()),
                &bytes(candidate.parent_catalogue()),
                &digest(candidate.catalogue_hash()),
                &CONTRACT_VERSION,
                &encoder.catalogue_hash_version()?,
                &encoder.standard_library_revision().map(bytes),
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    persist_semantics(transaction, candidate, encoder).await?;
    persist_revisions_and_references(transaction, candidate, encoder).await
}

pub(super) async fn persist_source(
    transaction: &Transaction<'_>,
    source: &orna_core::revision::StoredSourceRevision,
    reuse_existing_units: bool,
) -> Result<(), PostgresKernelError> {
    transaction
        .execute(
            "INSERT INTO _orna_kernel.source_bundles
                (id, content_hash, hash_algorithm, hash_contract_version)
             VALUES ($1, $2, 'sha256', $3)",
            &[
                &bytes(source.bundle()),
                &digest(source.bundle_hash()),
                &CONTRACT_VERSION,
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    for unit in source.units() {
        let existing = if reuse_existing_units {
            // The append-only standard parent edge (work ADR 0059): an
            // already-installed unit with the same reserved identity is the
            // retained parent unit. It must be byte-identical; the immutable
            // source-unit row is never re-parented. Membership in the bundle
            // relation below is authoritative.
            transaction
                .query_opt(
                    "SELECT ordinal, logical_path, content, content_hash
                     FROM _orna_kernel.source_units WHERE id = $1",
                    &[&bytes(unit.id())],
                )
                .await
                .map_err(PostgresKernelError::Database)?
        } else {
            None
        };
        if let Some(row) = existing {
            let ordinal: i64 = row.try_get(0).map_err(PostgresKernelError::Database)?;
            let logical_path: String = row.try_get(1).map_err(PostgresKernelError::Database)?;
            let content: String = row.try_get(2).map_err(PostgresKernelError::Database)?;
            let content_hash: Vec<u8> = row.try_get(3).map_err(PostgresKernelError::Database)?;
            if ordinal != i64::from(unit.ordinal())
                || logical_path != unit.logical_path()
                || content != unit.content()
                || content_hash != digest(unit.content_hash())
            {
                return Err(invariant(
                    "reused standard source unit must be byte-identical to the retained parent",
                ));
            }
        } else {
            transaction
                .execute(
                    "INSERT INTO _orna_kernel.source_units
                        (id, bundle_id, ordinal, logical_path, content, content_hash,
                         hash_algorithm, encoding, hash_contract_version)
                     VALUES ($1, $2, $3, $4, $5, $6, 'sha256', 'utf-8', $7)",
                    &[
                        &bytes(unit.id()),
                        &bytes(source.bundle()),
                        &i64::from(unit.ordinal()),
                        &unit.logical_path(),
                        &unit.content(),
                        &digest(unit.content_hash()),
                        &CONTRACT_VERSION,
                    ],
                )
                .await
                .map_err(PostgresKernelError::Database)?;
        }
        transaction
            .execute(
                "INSERT INTO _orna_kernel.source_bundle_units
                    (bundle_id, source_unit_id, ordinal)
                 VALUES ($1, $2, $3)",
                &[
                    &bytes(source.bundle()),
                    &bytes(unit.id()),
                    &i64::from(unit.ordinal()),
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    transaction
        .execute(
            "INSERT INTO _orna_kernel.source_revisions
                (id, parent_source_revision_id, bundle_id, content_hash,
                 hash_algorithm, hash_contract_version)
             VALUES ($1, $2, $3, $4, 'sha256', $5)",
            &[
                &bytes(source.id()),
                &source.parent().map(bytes),
                &bytes(source.bundle()),
                &digest(source.revision_hash()),
                &CONTRACT_VERSION,
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    Ok(())
}

async fn persist_semantics(
    transaction: &Transaction<'_>,
    candidate: &DeployableRevision,
    encoder: &CandidateEncoder<'_>,
) -> Result<(), PostgresKernelError> {
    let catalogue = candidate.candidate().revision();
    for schema in candidate.candidate().schemas() {
        let origin = origin(candidate.origins(), DefinitionIdentity::Schema(schema.id()))?;
        transaction.execute(
            "INSERT INTO _orna_kernel.catalogue_schemas
                (catalogue_revision_id, schema_id, name_parts, source_unit_id, source_start, source_end)
             VALUES ($1, $2, $3, $4, $5, $6)",
            &[
                &bytes(catalogue), &bytes(schema.id()), &schema.name().parts(),
                &bytes(origin.source_unit()), &i64::from(origin.byte_start()),
                &i64::from(origin.byte_end()),
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    }
    for expression in candidate.expressions() {
        let expression_origin = origin(
            candidate.origins(),
            DefinitionIdentity::Expression(expression.id()),
        )?;
        let version = positive_i32(expression.version(), "expression format version")?;
        transaction
            .execute(
                "INSERT INTO _orna_kernel.catalogue_expressions
                (catalogue_revision_id, expression_id, format, format_version, payload,
                 content_hash, hash_algorithm, hash_contract_version, source_unit_id,
                 source_start, source_end)
             VALUES ($1, $2, $3, $4, $5, $6, 'sha256', $7, $8, $9, $10)",
                &[
                    &bytes(catalogue),
                    &bytes(expression.id()),
                    &expression.format(),
                    &version,
                    &expression.payload(),
                    &digest(expression.content_hash()),
                    &CONTRACT_VERSION,
                    &bytes(expression_origin.source_unit()),
                    &i64::from(expression_origin.byte_start()),
                    &i64::from(expression_origin.byte_end()),
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    for enum_type in candidate.candidate().enum_types() {
        let schema = schema_for_name(candidate.candidate(), enum_type.name())?;
        let origin = origin(
            candidate.origins(),
            DefinitionIdentity::ValueType(enum_type.id()),
        )?;
        transaction
            .execute(
                "INSERT INTO _orna_kernel.catalogue_enum_types
                    (catalogue_revision_id, type_id, schema_id, name_parts, labels,
                     source_unit_id, source_start, source_end)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                &[
                    &bytes(catalogue),
                    &bytes(enum_type.id()),
                    &bytes(schema),
                    &enum_type.name().parts(),
                    &enum_type.labels(),
                    &bytes(origin.source_unit()),
                    &i64::from(origin.byte_start()),
                    &i64::from(origin.byte_end()),
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    for record_type in candidate.candidate().record_value_types() {
        let schema = schema_for_name(candidate.candidate(), record_type.name())?;
        let type_origin = origin(
            candidate.origins(),
            DefinitionIdentity::ValueType(record_type.id()),
        )?;
        transaction
            .execute(
                "INSERT INTO _orna_kernel.catalogue_record_value_types
                    (catalogue_revision_id, type_id, schema_id, name_parts,
                     value_kind, mutability, persistence,
                     source_unit_id, source_start, source_end)
                 VALUES ($1, $2, $3, $4, 'record', 'immutable', 'persistable',
                         $5, $6, $7)",
                &[
                    &bytes(catalogue),
                    &bytes(record_type.id()),
                    &bytes(schema),
                    &record_type.name().parts(),
                    &bytes(type_origin.source_unit()),
                    &i64::from(type_origin.byte_start()),
                    &i64::from(type_origin.byte_end()),
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;

        for field in record_type.fields() {
            let RecordValueFieldColumns {
                kind,
                value_type,
                value_standard_library_revision,
                application_enum_type,
                enum_standard_library_revision,
                standard_enum_type,
                application_record_type,
            } = encoder.record_value_field_columns(candidate, field.descriptor())?;
            let field_origin = origin(
                candidate.origins(),
                DefinitionIdentity::Field {
                    owner: record_type.id(),
                    field: field.id(),
                },
            )?;
            transaction
                .execute(
                    "INSERT INTO _orna_kernel.catalogue_record_value_fields
                        (catalogue_revision_id, owner_type_id, field_id, name, ordinal,
                         type_kind, value_type_id, value_standard_library_revision_id,
                         enum_type_id, enum_standard_library_revision_id,
                         standard_enum_type_id, record_type_id,
                         source_unit_id, source_start, source_end)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)",
                    &[
                        &bytes(catalogue),
                        &bytes(record_type.id()),
                        &bytes(field.id()),
                        &field.name(),
                        &i64::from(field.ordinal()),
                        &kind,
                        &value_type.map(bytes),
                        &value_standard_library_revision.map(bytes),
                        &application_enum_type.map(bytes),
                        &enum_standard_library_revision.map(bytes),
                        &standard_enum_type.map(bytes),
                        &application_record_type.map(bytes),
                        &bytes(field_origin.source_unit()),
                        &i64::from(field_origin.byte_start()),
                        &i64::from(field_origin.byte_end()),
                    ],
                )
                .await
                .map_err(PostgresKernelError::Database)?;
        }
    }
    for object in candidate.candidate().object_types() {
        let schema = schema_for_name(candidate.candidate(), object.name())?;
        let origin = origin(
            candidate.origins(),
            DefinitionIdentity::ObjectType(object.id()),
        )?;
        transaction
            .execute(
                "INSERT INTO _orna_kernel.catalogue_object_types
                (catalogue_revision_id, type_id, schema_id, name_parts,
                 source_unit_id, source_start, source_end)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
                &[
                    &bytes(catalogue),
                    &bytes(object.id()),
                    &bytes(schema),
                    &object.name().parts(),
                    &bytes(origin.source_unit()),
                    &i64::from(origin.byte_start()),
                    &i64::from(origin.byte_end()),
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    for object in candidate.candidate().object_types() {
        for field in object.fields() {
            let TypeColumns {
                kind,
                scalar,
                target,
                value_type,
                standard_library_revision,
                enum_type,
                record_type,
            } = encoder.type_columns(field.resolved_type(), false)?;
            let delete = on_delete(field.on_delete());
            let origin = origin(
                candidate.origins(),
                DefinitionIdentity::Field {
                    owner: object.id(),
                    field: field.id(),
                },
            )?;
            transaction
                .execute(
                    "INSERT INTO _orna_kernel.catalogue_fields
                    (catalogue_revision_id, owner_type_id, field_id, name, ordinal,
                     type_kind, scalar_type, target_type_id, value_type_id,
                     value_standard_library_revision_id, enum_type_id, record_type_id,
                     nullable, is_unique,
                     default_expression_id, on_delete, source_unit_id, source_start, source_end)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19)",
                    &[
                        &bytes(catalogue),
                        &bytes(object.id()),
                        &bytes(field.id()),
                        &field.name(),
                        &i64::from(field.ordinal()),
                        &kind,
                        &scalar,
                        &target.map(bytes),
                        &value_type.map(bytes),
                        &standard_library_revision.map(bytes),
                        &enum_type.map(bytes),
                        &record_type.map(bytes),
                        &field.nullable(),
                        &field.unique(),
                        &field.default_expression().map(bytes),
                        &delete,
                        &bytes(origin.source_unit()),
                        &i64::from(origin.byte_start()),
                        &i64::from(origin.byte_end()),
                    ],
                )
                .await
                .map_err(PostgresKernelError::Database)?;
        }
    }
    persist_functions(transaction, candidate, encoder).await
}

async fn persist_functions(
    transaction: &Transaction<'_>,
    candidate: &DeployableRevision,
    encoder: &CandidateEncoder<'_>,
) -> Result<(), PostgresKernelError> {
    let catalogue = candidate.candidate().revision();
    for function in candidate.candidate().functions() {
        let schema = schema_for_name(candidate.candidate(), function.name())?;
        let function_origin = origin(
            candidate.origins(),
            DefinitionIdentity::Function(function.id()),
        )?;
        let (
            shape,
            kind,
            scalar,
            target,
            value_type,
            standard_library_revision,
            enum_type,
            record_type,
        ) = match function.return_type() {
            FunctionReturn::Single(value) => {
                let TypeColumns {
                    kind,
                    scalar,
                    target,
                    value_type,
                    standard_library_revision,
                    enum_type,
                    record_type,
                } = encoder.function_type_columns(function.domain(), *value, true)?;
                (
                    "single",
                    Some(kind),
                    scalar,
                    target,
                    value_type,
                    standard_library_revision,
                    enum_type,
                    record_type,
                )
            }
            FunctionReturn::Rows(_) => ("rows", None, None, None, None, None, None, None),
            FunctionReturn::Stream(value) => {
                let TypeColumns {
                    kind,
                    scalar,
                    target,
                    value_type,
                    standard_library_revision,
                    enum_type,
                    record_type,
                } = encoder.function_type_columns(function.domain(), *value, false)?;
                (
                    "stream",
                    Some(kind),
                    scalar,
                    target,
                    value_type,
                    standard_library_revision,
                    enum_type,
                    record_type,
                )
            }
        };
        transaction
            .execute(
                "INSERT INTO _orna_kernel.catalogue_functions
                (catalogue_revision_id, function_id, schema_id, name_parts, domain,
                 security_mode, transaction_mode, volatility, return_shape,
                 return_type_kind, return_scalar_type, return_target_type_id,
                 return_value_type_id, return_standard_library_revision_id,
                 return_enum_type_id, return_record_type_id,
                 current_function_revision_id, source_unit_id, source_start, source_end)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20)",
                &[
                    &bytes(catalogue),
                    &bytes(function.id()),
                    &bytes(schema),
                    &function.name().parts(),
                    &function_domain(function.domain()),
                    &function_security(function.security()),
                    &function_transaction(function.transaction())?,
                    &function_volatility(function.volatility()),
                    &shape,
                    &kind,
                    &scalar,
                    &target.map(bytes),
                    &value_type.map(bytes),
                    &standard_library_revision.map(bytes),
                    &enum_type.map(bytes),
                    &record_type.map(bytes),
                    &bytes(function.current_revision()),
                    &bytes(function_origin.source_unit()),
                    &i64::from(function_origin.byte_start()),
                    &i64::from(function_origin.byte_end()),
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
        for parameter in function.parameters() {
            let TypeColumns {
                kind,
                scalar,
                target,
                value_type,
                standard_library_revision,
                enum_type,
                record_type,
            } = encoder.function_type_columns(
                function.domain(),
                parameter.resolved_type(),
                false,
            )?;
            let origin = origin(
                candidate.origins(),
                DefinitionIdentity::Parameter {
                    owner: function.id(),
                    parameter: parameter.id(),
                },
            )?;
            transaction
                .execute(
                    "INSERT INTO _orna_kernel.catalogue_function_parameters
                    (catalogue_revision_id, function_id, parameter_id, name, ordinal,
                     type_kind, scalar_type, target_type_id, value_type_id,
                     value_standard_library_revision_id, enum_type_id, record_type_id,
                     default_expression_id,
                     source_unit_id, source_start, source_end)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)",
                    &[
                        &bytes(catalogue),
                        &bytes(function.id()),
                        &bytes(parameter.id()),
                        &parameter.name(),
                        &i64::from(parameter.ordinal()),
                        &kind,
                        &scalar,
                        &target.map(bytes),
                        &value_type.map(bytes),
                        &standard_library_revision.map(bytes),
                        &enum_type.map(bytes),
                        &record_type.map(bytes),
                        &parameter.default_expression().map(bytes),
                        &bytes(origin.source_unit()),
                        &i64::from(origin.byte_start()),
                        &i64::from(origin.byte_end()),
                    ],
                )
                .await
                .map_err(PostgresKernelError::Database)?;
        }
        if let FunctionReturn::Rows(columns) = function.return_type() {
            for column in columns {
                let TypeColumns {
                    kind,
                    scalar,
                    target,
                    value_type,
                    standard_library_revision,
                    enum_type,
                    record_type,
                } = encoder.type_columns(column.resolved_type(), false)?;
                let origin = origin(
                    candidate.origins(),
                    DefinitionIdentity::FunctionReturnColumn {
                        owner: function.id(),
                        ordinal: column.ordinal(),
                    },
                )?;
                transaction
                    .execute(
                        "INSERT INTO _orna_kernel.catalogue_function_return_columns
                        (catalogue_revision_id, function_id, name, ordinal, type_kind,
                         scalar_type, target_type_id, value_type_id,
                         value_standard_library_revision_id, enum_type_id, record_type_id,
                         source_unit_id, source_start, source_end)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
                        &[
                            &bytes(catalogue),
                            &bytes(function.id()),
                            &column.name(),
                            &i64::from(column.ordinal()),
                            &kind,
                            &scalar,
                            &target.map(bytes),
                            &value_type.map(bytes),
                            &standard_library_revision.map(bytes),
                            &enum_type.map(bytes),
                            &record_type.map(bytes),
                            &bytes(origin.source_unit()),
                            &i64::from(origin.byte_start()),
                            &i64::from(origin.byte_end()),
                        ],
                    )
                    .await
                    .map_err(PostgresKernelError::Database)?;
            }
        }
    }
    Ok(())
}

async fn persist_revisions_and_references(
    transaction: &Transaction<'_>,
    candidate: &DeployableRevision,
    encoder: &CandidateEncoder<'_>,
) -> Result<(), PostgresKernelError> {
    let catalogue = candidate.candidate().revision();
    for revision in candidate.new_function_revisions() {
        let version = positive_i64(revision.revision_number(), "function revision number")?;
        transaction
            .execute(
                "INSERT INTO _orna_kernel.function_revisions
                (id, introduced_catalogue_revision_id, function_id, revision_number,
                 content_hash, semantic_ir_hash, hash_algorithm, language_version,
                 status, hash_contract_version, semantic_hash_version)
             VALUES ($1, $2, $3, $4, $5, $6, 'sha256', $7, 'candidate', $8, $9)",
                &[
                    &bytes(revision.id()),
                    &bytes(catalogue),
                    &bytes(revision.function()),
                    &version,
                    &digest(revision.declaration_content_hash()),
                    &digest(revision.semantic_hash()),
                    &revision.language_version(),
                    &CONTRACT_VERSION,
                    &semantic_hash_version(revision.semantic_hash_version())?,
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
        let artifact = revision.artifact();
        let version = positive_i32(artifact.version(), "function artifact format version")?;
        transaction
            .execute(
                "INSERT INTO _orna_kernel.function_artifacts
                (function_revision_id, artifact_kind, format, format_version, payload,
                 content_hash, hash_algorithm, hash_contract_version)
             VALUES ($1, $2, $3, $4, $5, $6, 'sha256', $7)",
                &[
                    &bytes(revision.id()),
                    &artifact_kind(artifact.kind()),
                    &artifact.format(),
                    &version,
                    &artifact.payload(),
                    &digest(artifact.content_hash()),
                    &CONTRACT_VERSION,
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    for reference in candidate.references() {
        let (
            target,
            kind,
            owner_type,
            owner_function,
            standard_library_revision,
            enum_catalogue_revision,
            record_catalogue_revision,
            record_field_catalogue_revision,
            record_field_owner_type,
        ) = encoder.reference_columns(reference)?;
        let reference_kind = reference_kind(reference.kind())?;
        let source = reference.source_origin();
        transaction
            .execute(
                "INSERT INTO _orna_kernel.definition_references
                (catalogue_revision_id, source_function_id, source_function_revision_id,
                 ordinal, target_definition_id, target_kind, target_owner_type_id,
                 target_owner_function_id, target_standard_library_revision_id,
                 target_enum_catalogue_revision_id, target_record_catalogue_revision_id,
                 target_record_field_catalogue_revision_id,
                 target_record_field_owner_type_id,
                 reference_kind, source_subobject_id,
                 source_unit_id, source_start, source_end)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, NULL, $15, $16, $17)",
                &[
                    &bytes(catalogue),
                    &bytes(reference.source_function()),
                    &bytes(reference.source_revision()),
                    &i64::from(reference.ordinal()),
                    &target,
                    &kind,
                    &owner_type,
                    &owner_function,
                    &standard_library_revision,
                    &enum_catalogue_revision,
                    &record_catalogue_revision,
                    &record_field_catalogue_revision,
                    &record_field_owner_type,
                    &reference_kind,
                    &bytes(source.source_unit()),
                    &i64::from(source.byte_start()),
                    &i64::from(source.byte_end()),
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    Ok(())
}

pub(super) async fn transition_revision_statuses(
    transaction: &Transaction<'_>,
    candidate: &DeployableRevision,
    locked: &ActiveDatabaseRevision,
    materialized: &Materialized,
) -> Result<(), PostgresKernelError> {
    let new_current = materialized
        .current
        .iter()
        .map(FunctionRevisionRecord::id)
        .collect::<BTreeSet<_>>();
    for revision in locked.function_revisions() {
        if !new_current.contains(&revision.id()) {
            let updated = transaction
                .execute(
                    "UPDATE _orna_kernel.function_revisions
                     SET status = 'retired'
                     WHERE id = $1 AND status = 'active'",
                    &[&bytes(revision.id())],
                )
                .await
                .map_err(PostgresKernelError::Database)?;
            require_one(updated, "active function revision retirement")?;
        }
    }
    let new_ids = candidate
        .new_function_revisions()
        .iter()
        .map(FunctionRevisionRecord::id)
        .collect::<BTreeSet<_>>();
    for revision in &materialized.current {
        if new_ids.contains(&revision.id()) {
            let updated = transaction
                .execute(
                    "UPDATE _orna_kernel.function_revisions
                     SET status = 'active'
                     WHERE id = $1 AND status = 'candidate'",
                    &[&bytes(revision.id())],
                )
                .await
                .map_err(PostgresKernelError::Database)?;
            require_one(updated, "candidate function revision activation")?;
        } else if locked
            .historical_function_revisions()
            .iter()
            .any(|old| old.id() == revision.id())
        {
            let updated = transaction
                .execute(
                    "UPDATE _orna_kernel.function_revisions
                     SET status = 'active'
                     WHERE id = $1 AND status = 'retired'",
                    &[&bytes(revision.id())],
                )
                .await
                .map_err(PostgresKernelError::Database)?;
            require_one(updated, "historical function revision activation")?;
        }
    }
    Ok(())
}

pub(super) async fn verify_revision_statuses(
    transaction: &Transaction<'_>,
    materialized: &Materialized,
) -> Result<(), PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT id, status FROM _orna_kernel.function_revisions ORDER BY id",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let expected = materialized
        .current
        .iter()
        .map(FunctionRevisionRecord::id)
        .collect::<BTreeSet<_>>();
    let record = DurableRecord::new("_orna_kernel.function_revisions", "status sweep");
    let mut actual = BTreeSet::new();
    for row in rows {
        let id = FunctionRevisionId::from_bytes(identity_bytes(
            record.column(
                &row,
                "id",
                "function revision identity must be exactly 16 bytes",
            )?,
            &record,
            "function revision identity must be exactly 16 bytes",
        )?);
        let status: String = record.column(
            &row,
            "status",
            "function revision status must be active or retired after apply",
        )?;
        match status.as_str() {
            "active" => {
                actual.insert(id);
            }
            "retired" => {}
            _ => {
                return Err(record
                    .invariant("function revision status must be active or retired after apply"));
            }
        }
    }
    if actual != expected {
        return Err(record.invariant(
            "active function revision identities must exactly equal candidate current identities",
        ));
    }
    Ok(())
}

pub(super) async fn update_active_pair(
    transaction: &Transaction<'_>,
    candidate: &DeployableRevision,
    expected: RevisionPair,
) -> Result<(), PostgresKernelError> {
    let updated = transaction
        .execute(
            "UPDATE _orna_kernel.active_revision
             SET source_revision_id = $1,
                 catalogue_revision_id = $2,
                 updated_at = transaction_timestamp()
             WHERE singleton = true
               AND source_revision_id = $3
               AND catalogue_revision_id = $4",
            &[
                &bytes(candidate.source().id()),
                &bytes(candidate.candidate().revision()),
                &bytes(expected.source()),
                &bytes(expected.catalogue()),
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    require_one(updated, "active revision pointer update")
}
