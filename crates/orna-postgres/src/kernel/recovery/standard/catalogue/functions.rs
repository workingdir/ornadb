//! Standard-library function signatures and parameters.

use super::*;

pub(super) struct RecoveredStandardFunction {
    pub(super) schema: SchemaId,
    pub(super) id: FunctionId,
    pub(super) name: QualifiedSemanticName,
    pub(super) domain: FunctionDomain,
    pub(super) security: FunctionSecurity,
    pub(super) transaction: Option<FunctionTransaction>,
    pub(super) volatility: FunctionVolatility,
    pub(super) return_type: FunctionReturn,
    pub(super) current_revision: FunctionRevisionId,
    pub(super) origin: DefinitionOrigin,
}

#[derive(Clone)]
pub(super) struct RecoveredStandardParameter {
    pub(super) function: FunctionId,
    pub(super) definition: ParameterDefinition,
    pub(super) origin: DefinitionOrigin,
}

pub(super) async fn load_standard_functions(
    transaction: &Transaction<'_>,
    standard: StandardLibraryRevisionId,
    value_type_ids: &HashSet<TypeId>,
) -> Result<Vec<RecoveredStandardFunction>, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.standard_catalogue_functions";
    let rows = transaction
        .query(
            "SELECT standard_library_revision_id, function_id, schema_id, name_parts,
                    domain, security_mode, transaction_mode, volatility, return_shape,
                    return_type_kind, return_scalar_type, return_value_type_id,
                    current_function_revision_id, source_unit_id, source_start, source_end
             FROM _orna_kernel.standard_catalogue_functions
             WHERE standard_library_revision_id = $1
             ORDER BY function_id",
            &[&standard.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let mut functions = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        functions.push(decode_standard_function(
            row,
            index,
            standard,
            RELATION,
            value_type_ids,
        )?);
    }
    Ok(functions)
}

fn decode_standard_function(
    row: &Row,
    index: usize,
    expected_standard: StandardLibraryRevisionId,
    relation: &'static str,
    value_type_ids: &HashSet<TypeId>,
) -> Result<RecoveredStandardFunction, PostgresKernelError> {
    let row_record = DurableRecord::new(relation, format!("row={index}"));
    require_standard_library_revision(row, &row_record, expected_standard, "function")?;
    let id = FunctionId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "function_id",
            "standard function identity must be 16 bytes",
        )?,
        &row_record,
        "standard function identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(relation, id.canonical());
    let schema = SchemaId::from_bytes(identity_bytes(
        record.column(
            row,
            "schema_id",
            "standard function schema identity must be 16 bytes",
        )?,
        &record,
        "standard function schema identity must be 16 bytes",
    )?);
    let name_parts: Vec<String> = record.column(
        row,
        "name_parts",
        "standard function name parts must be an exact PostgreSQL text array",
    )?;
    let name = QualifiedSemanticName::new(name_parts).map_err(|_| {
        record.invariant("standard function name parts must form one exact semantic name")
    })?;
    let domain_name: String =
        record.column(row, "domain", "standard function domain must decode")?;
    let domain = exact_enum(
        &domain_name,
        &[
            ("server", FunctionDomain::Server),
            ("client", FunctionDomain::Client),
        ],
        &record,
        "standard function domain must be server or client",
    )?;
    let security_name: String = record.column(
        row,
        "security_mode",
        "standard function security must decode",
    )?;
    let security = exact_enum(
        &security_name,
        &[
            ("invoker", FunctionSecurity::Invoker),
            ("definer", FunctionSecurity::Definer),
        ],
        &record,
        "standard function security must be invoker or definer",
    )?;
    let transaction_name: Option<String> = record.column(
        row,
        "transaction_mode",
        "standard function transaction mode must decode",
    )?;
    let transaction = transaction_name
        .map(|name| {
            exact_enum(
                &name,
                &[
                    ("atomic", FunctionTransaction::Atomic),
                    ("read_only", FunctionTransaction::ReadOnly),
                ],
                &record,
                "standard function transaction mode must be atomic or read_only",
            )
        })
        .transpose()?;
    let volatility_name: String = record.column(
        row,
        "volatility",
        "standard function volatility must decode",
    )?;
    let volatility = exact_enum(
        &volatility_name,
        &[
            ("immutable", FunctionVolatility::Immutable),
            ("stable", FunctionVolatility::Stable),
            ("volatile", FunctionVolatility::Volatile),
        ],
        &record,
        "standard function volatility must be immutable, stable, or volatile",
    )?;
    let return_type = decode_standard_function_return(row, &record, value_type_ids)?;
    let current_revision = FunctionRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "current_function_revision_id",
            "standard function current revision identity must be 16 bytes",
        )?,
        &record,
        "standard function current revision identity must be 16 bytes",
    )?);
    let origin = decode_origin(row, &record, DefinitionIdentity::Function(id))?;
    Ok(RecoveredStandardFunction {
        schema,
        id,
        name,
        domain,
        security,
        transaction,
        volatility,
        return_type,
        current_revision,
        origin,
    })
}

fn decode_standard_function_return(
    row: &Row,
    record: &DurableRecord,
    value_type_ids: &HashSet<TypeId>,
) -> Result<FunctionReturn, PostgresKernelError> {
    let shape: String = record.column(
        row,
        "return_shape",
        "standard function return shape must decode",
    )?;
    if shape != "single" {
        return Err(record.invariant(
            "standard catalogue functions with ROWS results are not supported by standard persistence",
        ));
    }
    let kind: Option<String> = record.column(
        row,
        "return_type_kind",
        "standard function return type kind must decode",
    )?;
    let scalar: Option<String> = record.column(
        row,
        "return_scalar_type",
        "standard function return scalar type must decode",
    )?;
    let value_type: Option<Vec<u8>> = record.column(
        row,
        "return_value_type_id",
        "standard function return value type identity must be null or exact bytes",
    )?;
    let resolved = decode_standard_resolved_type(
        kind,
        scalar,
        value_type,
        Some(value_type_ids),
        true,
        record,
    )?;
    Ok(FunctionReturn::Single(resolved))
}

/// Decodes the closed scalar-or-value resolved type persisted for standard
/// catalogue functions and parameters. `value_type_ids` is required for the
/// value shape so the type must identify one standard value type.
fn decode_standard_resolved_type(
    kind: Option<String>,
    scalar_name: Option<String>,
    value_type: Option<Vec<u8>>,
    value_type_ids: Option<&HashSet<TypeId>>,
    allow_void: bool,
    record: &DurableRecord,
) -> Result<ResolvedType, PostgresKernelError> {
    match kind.as_deref() {
        Some("scalar") => {
            if value_type.is_some() {
                return Err(record.invariant(
                    "standard resolved type columns must form one exact scalar or value tuple",
                ));
            }
            let Some(scalar_name) = scalar_name else {
                return Err(record.invariant(
                    "standard resolved type columns must form one exact scalar or value tuple",
                ));
            };
            let scalar = exact_enum(
                &scalar_name,
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
                "standard resolved scalar type must be one exact supported scalar",
            )?;
            if scalar == StandardScalar::Void && !allow_void {
                return Err(record.invariant(
                    "void is valid only as a SINGLE function return, never as a parameter",
                ));
            }
            Ok(ResolvedType::scalar(scalar))
        }
        Some("value") => {
            if scalar_name.is_some() {
                return Err(record.invariant(
                    "standard resolved type columns must form one exact scalar or value tuple",
                ));
            }
            let Some(bytes) = value_type else {
                return Err(record.invariant(
                    "standard resolved type columns must form one exact scalar or value tuple",
                ));
            };
            let id = TypeId::from_bytes(identity_bytes(
                bytes,
                record,
                "standard resolved value type identity must be 16 bytes",
            )?);
            if value_type_ids.is_none_or(|value_type_ids| !value_type_ids.contains(&id)) {
                return Err(record.invariant(
                    "standard resolved value type must identify one standard catalogue value type",
                ));
            }
            Ok(ResolvedType::value(id))
        }
        _ => Err(record.invariant("standard resolved type kind must be scalar or value")),
    }
}

pub(super) async fn load_standard_parameters(
    transaction: &Transaction<'_>,
    standard: StandardLibraryRevisionId,
    value_type_ids: &HashSet<TypeId>,
) -> Result<BTreeMap<FunctionId, Vec<RecoveredStandardParameter>>, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.standard_catalogue_function_parameters";
    let rows = transaction
        .query(
            "SELECT standard_library_revision_id, function_id, parameter_id, name, ordinal,
                    type_kind, scalar_type, value_type_id,
                    source_unit_id, source_start, source_end
             FROM _orna_kernel.standard_catalogue_function_parameters
             WHERE standard_library_revision_id = $1
             ORDER BY function_id, ordinal, parameter_id",
            &[&standard.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let mut parameters = BTreeMap::<FunctionId, Vec<RecoveredStandardParameter>>::new();
    for (index, row) in rows.iter().enumerate() {
        let parameter = decode_standard_parameter(row, index, standard, RELATION, value_type_ids)?;
        parameters
            .entry(parameter.function)
            .or_default()
            .push(parameter);
    }
    Ok(parameters)
}

fn decode_standard_parameter(
    row: &Row,
    index: usize,
    expected_standard: StandardLibraryRevisionId,
    relation: &'static str,
    value_type_ids: &HashSet<TypeId>,
) -> Result<RecoveredStandardParameter, PostgresKernelError> {
    let row_record = DurableRecord::new(relation, format!("row={index}"));
    require_standard_library_revision(row, &row_record, expected_standard, "parameter")?;
    let function = FunctionId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "function_id",
            "standard parameter owner identity must be 16 bytes",
        )?,
        &row_record,
        "standard parameter owner identity must be 16 bytes",
    )?);
    let id = ParameterId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "parameter_id",
            "standard parameter identity must be 16 bytes",
        )?,
        &row_record,
        "standard parameter identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(
        relation,
        format!(
            "function={} parameter={}",
            function.canonical(),
            id.canonical()
        ),
    );
    let name: String = record.column(
        row,
        "name",
        "standard parameter name must be PostgreSQL text",
    )?;
    if name.is_empty() {
        return Err(record.invariant("standard parameter name must not be empty"));
    }
    let ordinal = u32_from_i64(
        record.column(row, "ordinal", "standard parameter ordinal must fit u32")?,
        &record,
        "standard parameter ordinal must fit u32",
    )?;
    let kind: Option<String> =
        record.column(row, "type_kind", "standard parameter type kind must decode")?;
    let scalar: Option<String> = record.column(
        row,
        "scalar_type",
        "standard parameter scalar type must decode",
    )?;
    let value_type: Option<Vec<u8>> = record.column(
        row,
        "value_type_id",
        "standard parameter value type identity must be null or exact bytes",
    )?;
    let resolved = decode_standard_resolved_type(
        kind,
        scalar,
        value_type,
        Some(value_type_ids),
        false,
        &record,
    )?;
    let origin = decode_origin(
        row,
        &record,
        DefinitionIdentity::Parameter {
            owner: function,
            parameter: id,
        },
    )?;
    Ok(RecoveredStandardParameter {
        function,
        definition: ParameterDefinition::new(id, name, ordinal, resolved, None),
        origin,
    })
}
