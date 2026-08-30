//! Function signature and resolved-type row recovery.

use super::*;

pub(in super::super) struct RecoveredFunction {
    pub(in super::super) schema: SchemaId,
    pub(in super::super) definition: FunctionDefinition,
}

struct RecoveredParameter {
    function: FunctionId,
    definition: ParameterDefinition,
    origin: DefinitionOrigin,
}

struct RecoveredReturnColumn {
    function: FunctionId,
    definition: FunctionReturnColumnDefinition,
    origin: DefinitionOrigin,
}

pub(in super::super) async fn load_catalogue_functions(
    transaction: &Transaction<'_>,
    catalogue: CatalogueRevisionId,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<(Vec<RecoveredFunction>, Vec<DefinitionOrigin>), PostgresKernelError> {
    let mut parameters = load_parameters(transaction, catalogue, catalogue_hash_context).await?;
    let mut returns = load_return_columns(transaction, catalogue, catalogue_hash_context).await?;
    let result = load_functions(
        transaction,
        catalogue,
        &mut parameters,
        &mut returns,
        catalogue_hash_context,
    )
    .await?;
    reject_leftover_members(&parameters, PARAMETER_RELATION, "parameter")?;
    reject_leftover_members(&returns, RETURN_RELATION, "return column")?;
    Ok(result)
}

async fn load_parameters(
    transaction: &Transaction<'_>,
    catalogue: CatalogueRevisionId,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<BTreeMap<FunctionId, Vec<RecoveredParameter>>, PostgresKernelError> {
    let rows = if catalogue_hash_context.standard().is_some() {
        transaction
            .query(
                "SELECT catalogue_revision_id, function_id, parameter_id, name, ordinal,
                        type_kind, scalar_type, target_type_id,
                        value_type_id, value_standard_library_revision_id,
                        enum_type_id, record_type_id,
                        default_expression_id, source_unit_id, source_start, source_end
                 FROM _orna_kernel.catalogue_function_parameters
                 WHERE catalogue_revision_id = $1
                 ORDER BY function_id, ordinal, parameter_id",
                &[&catalogue.to_bytes().to_vec()],
            )
            .await
            .map_err(PostgresKernelError::Database)?
    } else {
        transaction
            .query(
                "SELECT catalogue_revision_id, function_id, parameter_id, name, ordinal,
                        type_kind, scalar_type, target_type_id, default_expression_id,
                        source_unit_id, source_start, source_end
                 FROM _orna_kernel.catalogue_function_parameters
                 WHERE catalogue_revision_id = $1
                 ORDER BY function_id, ordinal, parameter_id",
                &[&catalogue.to_bytes().to_vec()],
            )
            .await
            .map_err(PostgresKernelError::Database)?
    };
    let mut parameters = BTreeMap::<FunctionId, Vec<RecoveredParameter>>::new();
    for (index, row) in rows.iter().enumerate() {
        let parameter = decode_parameter(row, index, catalogue, catalogue_hash_context)?;
        parameters
            .entry(parameter.function)
            .or_default()
            .push(parameter);
    }
    Ok(parameters)
}

fn decode_parameter(
    row: &Row,
    index: usize,
    catalogue: CatalogueRevisionId,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<RecoveredParameter, PostgresKernelError> {
    let row_record = DurableRecord::new(PARAMETER_RELATION, format!("row={index}"));
    require_catalogue(row, &row_record, catalogue, "parameter")?;
    let function = FunctionId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "function_id",
            "parameter owner identity must be 16 bytes",
        )?,
        &row_record,
        "parameter owner identity must be 16 bytes",
    )?);
    let id = ParameterId::from_bytes(identity_bytes(
        row_record.column(row, "parameter_id", "parameter identity must be 16 bytes")?,
        &row_record,
        "parameter identity must be 16 bytes",
    )?);
    let record = parameter_record(function, id);
    let name: String = record.column(row, "name", "parameter name must be PostgreSQL text")?;
    if name.is_empty() {
        return Err(record.invariant("parameter name must not be empty"));
    }
    let ordinal = u32_from_i64(
        record.column(row, "ordinal", "parameter ordinal must fit u32")?,
        &record,
        "parameter ordinal must fit u32",
    )?;
    let resolved_type = decode_type_columns(
        row,
        &record,
        LegacyResolvedTypeTupleMember::Parameter,
        catalogue_hash_context,
    )?;
    let default_expression = optional_identity_bytes(
        record.column(
            row,
            "default_expression_id",
            "parameter default expression identity must be null or 16 bytes",
        )?,
        &record,
        "parameter default expression identity must be null or 16 bytes",
    )?
    .map(ExpressionId::from_bytes);
    let origin = decode_origin(
        row,
        &record,
        DefinitionIdentity::Parameter {
            owner: function,
            parameter: id,
        },
    )?;
    Ok(RecoveredParameter {
        function,
        definition: ParameterDefinition::new(id, name, ordinal, resolved_type, default_expression),
        origin,
    })
}

async fn load_return_columns(
    transaction: &Transaction<'_>,
    catalogue: CatalogueRevisionId,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<BTreeMap<FunctionId, Vec<RecoveredReturnColumn>>, PostgresKernelError> {
    let rows = if catalogue_hash_context.standard().is_some() {
        transaction
            .query(
                "SELECT catalogue_revision_id, function_id, name, ordinal,
                        type_kind, scalar_type, target_type_id,
                        value_type_id, value_standard_library_revision_id,
                        enum_type_id, record_type_id,
                        source_unit_id, source_start, source_end
                 FROM _orna_kernel.catalogue_function_return_columns
                 WHERE catalogue_revision_id = $1
                 ORDER BY function_id, ordinal",
                &[&catalogue.to_bytes().to_vec()],
            )
            .await
            .map_err(PostgresKernelError::Database)?
    } else {
        transaction
            .query(
                "SELECT catalogue_revision_id, function_id, name, ordinal,
                        type_kind, scalar_type, target_type_id,
                        source_unit_id, source_start, source_end
                 FROM _orna_kernel.catalogue_function_return_columns
                 WHERE catalogue_revision_id = $1
                 ORDER BY function_id, ordinal",
                &[&catalogue.to_bytes().to_vec()],
            )
            .await
            .map_err(PostgresKernelError::Database)?
    };
    let mut columns = BTreeMap::<FunctionId, Vec<RecoveredReturnColumn>>::new();
    for (index, row) in rows.iter().enumerate() {
        let column = decode_return_column(row, index, catalogue, catalogue_hash_context)?;
        columns.entry(column.function).or_default().push(column);
    }
    Ok(columns)
}

fn decode_return_column(
    row: &Row,
    index: usize,
    catalogue: CatalogueRevisionId,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<RecoveredReturnColumn, PostgresKernelError> {
    let row_record = DurableRecord::new(RETURN_RELATION, format!("row={index}"));
    require_catalogue(row, &row_record, catalogue, "return column")?;
    let function = FunctionId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "function_id",
            "return column owner identity must be 16 bytes",
        )?,
        &row_record,
        "return column owner identity must be 16 bytes",
    )?);
    let ordinal = u32_from_i64(
        row_record.column(row, "ordinal", "return column ordinal must fit u32")?,
        &row_record,
        "return column ordinal must fit u32",
    )?;
    let record = DurableRecord::new(
        RETURN_RELATION,
        format!("function={} ordinal={ordinal}", function.canonical()),
    );
    let name: String = record.column(row, "name", "return column name must be PostgreSQL text")?;
    if name.is_empty() {
        return Err(record.invariant("return column name must not be empty"));
    }
    let resolved_type = decode_type_columns(
        row,
        &record,
        LegacyResolvedTypeTupleMember::ReturnColumn,
        catalogue_hash_context,
    )?;
    let origin = decode_origin(
        row,
        &record,
        DefinitionIdentity::FunctionReturnColumn {
            owner: function,
            ordinal,
        },
    )?;
    Ok(RecoveredReturnColumn {
        function,
        definition: FunctionReturnColumnDefinition::new(name, ordinal, resolved_type),
        origin,
    })
}

async fn load_functions(
    transaction: &Transaction<'_>,
    catalogue: CatalogueRevisionId,
    parameters: &mut BTreeMap<FunctionId, Vec<RecoveredParameter>>,
    returns: &mut BTreeMap<FunctionId, Vec<RecoveredReturnColumn>>,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<(Vec<RecoveredFunction>, Vec<DefinitionOrigin>), PostgresKernelError> {
    let rows = if catalogue_hash_context.standard().is_some() {
        transaction
            .query(
                "SELECT catalogue_revision_id, function_id, schema_id, name_parts,
                        domain, security_mode, transaction_mode, volatility,
                        return_shape, return_type_kind AS type_kind,
                        return_scalar_type AS scalar_type,
                        return_target_type_id AS target_type_id,
                        return_value_type_id AS value_type_id,
                        return_standard_library_revision_id AS value_standard_library_revision_id,
                        return_enum_type_id AS enum_type_id,
                        return_record_type_id AS record_type_id,
                        current_function_revision_id,
                        source_unit_id, source_start, source_end
                 FROM _orna_kernel.catalogue_functions
                 WHERE catalogue_revision_id = $1
                 ORDER BY function_id",
                &[&catalogue.to_bytes().to_vec()],
            )
            .await
            .map_err(PostgresKernelError::Database)?
    } else {
        transaction
            .query(
                "SELECT catalogue_revision_id, function_id, schema_id, name_parts,
                        domain, security_mode, transaction_mode, volatility,
                        return_shape, return_type_kind AS type_kind,
                        return_scalar_type AS scalar_type,
                        return_target_type_id AS target_type_id,
                        current_function_revision_id,
                        source_unit_id, source_start, source_end
                 FROM _orna_kernel.catalogue_functions
                 WHERE catalogue_revision_id = $1
                 ORDER BY function_id",
                &[&catalogue.to_bytes().to_vec()],
            )
            .await
            .map_err(PostgresKernelError::Database)?
    };
    let mut functions = Vec::with_capacity(rows.len());
    let mut origins = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let (function, mut function_origins) = decode_function(
            row,
            index,
            catalogue,
            parameters,
            returns,
            catalogue_hash_context,
        )?;
        origins.append(&mut function_origins);
        functions.push(function);
    }
    Ok((functions, origins))
}

fn decode_function(
    row: &Row,
    index: usize,
    catalogue: CatalogueRevisionId,
    parameters: &mut BTreeMap<FunctionId, Vec<RecoveredParameter>>,
    returns: &mut BTreeMap<FunctionId, Vec<RecoveredReturnColumn>>,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<(RecoveredFunction, Vec<DefinitionOrigin>), PostgresKernelError> {
    let row_record = DurableRecord::new(FUNCTION_RELATION, format!("row={index}"));
    require_catalogue(row, &row_record, catalogue, "function")?;
    let id = FunctionId::from_bytes(identity_bytes(
        row_record.column(row, "function_id", "function identity must be 16 bytes")?,
        &row_record,
        "function identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(FUNCTION_RELATION, id.canonical());
    let schema = SchemaId::from_bytes(identity_bytes(
        record.column(
            row,
            "schema_id",
            "function schema identity must be 16 bytes",
        )?,
        &record,
        "function schema identity must be 16 bytes",
    )?);
    let name_parts: Vec<String> = record.column(
        row,
        "name_parts",
        "function name parts must be an exact PostgreSQL text array",
    )?;
    let name = QualifiedSemanticName::new(name_parts)
        .map_err(|_| record.invariant("function name parts must form one exact semantic name"))?;
    let domain_name: String = record.column(row, "domain", "function domain must decode")?;
    let domain = exact_enum(
        &domain_name,
        &[
            ("server", FunctionDomain::Server),
            ("client", FunctionDomain::Client),
        ],
        &record,
        "function domain must be server or client",
    )?;
    let security_name: String =
        record.column(row, "security_mode", "function security mode must decode")?;
    let security = exact_enum(
        &security_name,
        &[
            ("invoker", FunctionSecurity::Invoker),
            ("definer", FunctionSecurity::Definer),
        ],
        &record,
        "function security mode must be invoker or definer",
    )?;
    let transaction_name: Option<String> = record.column(
        row,
        "transaction_mode",
        "function transaction mode must be null, atomic, or read_only",
    )?;
    let transaction = match transaction_name.as_deref() {
        None => None,
        Some("atomic") => Some(FunctionTransaction::Atomic),
        Some("read_only") => Some(FunctionTransaction::ReadOnly),
        Some(_) => {
            return Err(record.invariant(
                "function transaction mode must be null, atomic, or read_only; manual is unsupported",
            ));
        }
    };
    if domain == FunctionDomain::Client && transaction.is_some() {
        return Err(record.invariant("client functions must not declare transaction behaviour"));
    }
    let volatility_name: String =
        record.column(row, "volatility", "function volatility must decode")?;
    let volatility = exact_enum(
        &volatility_name,
        &[
            ("immutable", FunctionVolatility::Immutable),
            ("stable", FunctionVolatility::Stable),
            ("volatile", FunctionVolatility::Volatile),
        ],
        &record,
        "function volatility must be immutable, stable, or volatile",
    )?;
    let current_revision = FunctionRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "current_function_revision_id",
            "current function revision identity must be 16 bytes",
        )?,
        &record,
        "current function revision identity must be 16 bytes",
    )?);
    let recovered_parameters = parameters.remove(&id).unwrap_or_default();
    let mut member_origins = recovered_parameters
        .iter()
        .map(|parameter| parameter.origin.clone())
        .collect::<Vec<_>>();
    let parameter_definitions = recovered_parameters
        .into_iter()
        .map(|parameter| parameter.definition)
        .collect();
    let recovered_returns = returns.remove(&id).unwrap_or_default();
    let return_shape: String =
        record.column(row, "return_shape", "function return shape must decode")?;
    let return_type = match return_shape.as_str() {
        "single" if recovered_returns.is_empty() => FunctionReturn::Single(decode_type_columns(
            row,
            &record,
            LegacyResolvedTypeTupleMember::SingleReturn,
            catalogue_hash_context,
        )?),
        "rows" => {
            require_null_type_columns(row, &record, catalogue_hash_context)?;
            member_origins.extend(recovered_returns.iter().map(|column| column.origin.clone()));
            FunctionReturn::Rows(
                recovered_returns
                    .into_iter()
                    .map(|column| column.definition)
                    .collect(),
            )
        }
        "single" => {
            return Err(record.invariant("SINGLE functions must not have ROWS return columns"));
        }
        "stream" if recovered_returns.is_empty() => FunctionReturn::Stream(decode_type_columns(
            row,
            &record,
            LegacyResolvedTypeTupleMember::StreamReturn,
            catalogue_hash_context,
        )?),
        "stream" => {
            return Err(record.invariant("STREAM functions must not have ROWS return columns"));
        }
        _ => return Err(record.invariant("function return shape must be single, rows, or stream")),
    };
    let origin = decode_origin(row, &record, DefinitionIdentity::Function(id))?;
    member_origins.push(origin);
    Ok((
        RecoveredFunction {
            schema,
            definition: FunctionDefinition::new(
                id,
                name,
                domain,
                parameter_definitions,
                return_type,
                current_revision,
                security,
                transaction,
                volatility,
            ),
        },
        member_origins,
    ))
}

fn reject_leftover_members<T>(
    members: &BTreeMap<FunctionId, Vec<T>>,
    relation: &'static str,
    member: &'static str,
) -> Result<(), PostgresKernelError> {
    if let Some((function, _)) = members.first_key_value() {
        return Err(
            DurableRecord::new(relation, format!("function={}", function.canonical())).invariant(
                match member {
                    "parameter" => "every parameter owner must be an active function",
                    _ => "every return column owner must be an active function",
                },
            ),
        );
    }
    Ok(())
}

fn decode_type_columns(
    row: &Row,
    record: &DurableRecord,
    member: LegacyResolvedTypeTupleMember,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<ResolvedType, PostgresKernelError> {
    if catalogue_hash_context.standard().is_some() {
        return decode_version_two_type_columns(row, record, member, catalogue_hash_context);
    }
    let kind: Option<String> = record.column(
        row,
        "type_kind",
        "resolved type kind must be scalar, named, or reference",
    )?;
    let scalar: Option<String> = record.column(
        row,
        "scalar_type",
        "resolved scalar type must be null or an exact standard scalar name",
    )?;
    let target = optional_identity_bytes(
        record.column(
            row,
            "target_type_id",
            "resolved target identity must be null or 16 bytes",
        )?,
        record,
        "resolved target identity must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    let kind = decode_legacy_resolved_type_tuple_kind(kind.as_deref(), record, member)?;
    decode_legacy_resolved_type_tuple(kind, scalar.as_deref(), target, record, member)
}

fn decode_version_two_type_columns(
    row: &Row,
    record: &DurableRecord,
    member: LegacyResolvedTypeTupleMember,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<ResolvedType, PostgresKernelError> {
    let kind: Option<String> = record.column(
        row,
        "type_kind",
        "resolved type kind must be scalar, named, reference, value, or enum",
    )?;
    let scalar: Option<String> = record.column(
        row,
        "scalar_type",
        "resolved scalar type must be null or an exact standard scalar name",
    )?;
    let target = optional_identity_bytes(
        record.column(
            row,
            "target_type_id",
            "resolved target identity must be null or 16 bytes",
        )?,
        record,
        "resolved target identity must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    let value_type = optional_identity_bytes(
        record.column(
            row,
            "value_type_id",
            "resolved value type identity must be null or 16 bytes",
        )?,
        record,
        "resolved value type identity must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    let standard_library_revision = optional_identity_bytes(
        record.column(
            row,
            "value_standard_library_revision_id",
            "resolved value type standard library revision identity must be null or 16 bytes",
        )?,
        record,
        "resolved value type standard library revision identity must be null or 16 bytes",
    )?
    .map(StandardLibraryRevisionId::from_bytes);
    let enum_type = optional_identity_bytes(
        record.column(
            row,
            "enum_type_id",
            "resolved enum type identity must be null or 16 bytes",
        )?,
        record,
        "resolved enum type identity must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    let record_type = optional_identity_bytes(
        record.column(
            row,
            "record_type_id",
            "resolved record type identity must be null or 16 bytes",
        )?,
        record,
        "resolved record type identity must be null or 16 bytes",
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
        member,
    )
}

fn require_null_type_columns(
    row: &Row,
    record: &DurableRecord,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<(), PostgresKernelError> {
    let kind: Option<String> = record.column(row, "type_kind", "ROWS type kind must be null")?;
    let scalar: Option<String> =
        record.column(row, "scalar_type", "ROWS scalar type must be null")?;
    let target: Option<Vec<u8>> =
        record.column(row, "target_type_id", "ROWS target type must be null")?;
    let value_type: Option<Vec<u8>> = if catalogue_hash_context.standard().is_some() {
        record.column(
            row,
            "value_type_id",
            "ROWS value type identity must be null",
        )?
    } else {
        None
    };
    let standard_library_revision: Option<Vec<u8>> = if catalogue_hash_context.standard().is_some()
    {
        record.column(
            row,
            "value_standard_library_revision_id",
            "ROWS value type standard library revision identity must be null",
        )?
    } else {
        None
    };
    let enum_type: Option<Vec<u8>> = if catalogue_hash_context.standard().is_some() {
        record.column(row, "enum_type_id", "ROWS enum type identity must be null")?
    } else {
        None
    };
    let record_type: Option<Vec<u8>> = if catalogue_hash_context.standard().is_some() {
        record.column(
            row,
            "record_type_id",
            "ROWS record type identity must be null",
        )?
    } else {
        None
    };
    if kind.is_some()
        || scalar.is_some()
        || target.is_some()
        || value_type.is_some()
        || standard_library_revision.is_some()
        || enum_type.is_some()
        || record_type.is_some()
    {
        return Err(record.invariant("ROWS functions must not store one SINGLE return type tuple"));
    }
    Ok(())
}

pub(super) fn require_catalogue(
    row: &Row,
    record: &DurableRecord,
    expected: CatalogueRevisionId,
    member: &'static str,
) -> Result<(), PostgresKernelError> {
    let catalogue = CatalogueRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "catalogue_revision_id",
            "function catalogue member revision identity must be 16 bytes",
        )?,
        record,
        "function catalogue member revision identity must be 16 bytes",
    )?);
    if catalogue != expected {
        return Err(record.invariant(match member {
            "function" => "function must belong to the selected catalogue revision",
            "parameter" => "parameter must belong to the selected catalogue revision",
            "return column" => "return column must belong to the selected catalogue revision",
            _ => "reference must belong to the selected catalogue revision",
        }));
    }
    Ok(())
}

fn parameter_record(function: FunctionId, parameter: ParameterId) -> DurableRecord {
    DurableRecord::new(
        PARAMETER_RELATION,
        format!(
            "function={} parameter={}",
            function.canonical(),
            parameter.canonical()
        ),
    )
}
