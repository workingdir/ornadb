//! Standard-library catalogue member recovery.

use super::*;

struct RecoveredStandardSchema {
    definition: SchemaDefinition,
    origin: DefinitionOrigin,
}

struct RecoveredStandardValueType {
    schema: SchemaId,
    definition: ValueTypeDefinition,
    origin: DefinitionOrigin,
}

struct RecoveredStandardEnumType {
    schema: SchemaId,
    definition: EnumTypeDefinition,
    origin: DefinitionOrigin,
}

struct RecoveredStandardTypeBinding {
    binding: TypeBinding,
    origin: DefinitionOrigin,
}

struct RecoveredStandardFunction {
    schema: SchemaId,
    id: FunctionId,
    name: QualifiedSemanticName,
    domain: FunctionDomain,
    security: FunctionSecurity,
    transaction: Option<FunctionTransaction>,
    volatility: FunctionVolatility,
    return_type: FunctionReturn,
    current_revision: FunctionRevisionId,
    origin: DefinitionOrigin,
}

#[derive(Clone)]
struct RecoveredStandardParameter {
    function: FunctionId,
    definition: ParameterDefinition,
    origin: DefinitionOrigin,
}

pub(super) async fn load_standard_catalogue(
    transaction: &Transaction<'_>,
    header: &RecoveredStandardHeader,
) -> Result<(CatalogueSnapshot, Vec<DefinitionOrigin>), PostgresKernelError> {
    let schemas = load_standard_schemas(transaction, header.revision).await?;
    let value_types = load_standard_value_types(transaction, header.revision).await?;
    let value_type_ids = value_types
        .iter()
        .map(|value_type| value_type.definition.id())
        .collect::<HashSet<_>>();
    let enum_types = load_standard_enum_types(transaction, header.revision).await?;
    let bindings = load_standard_type_bindings(transaction, header.revision).await?;
    let functions = load_standard_functions(transaction, header.revision, &value_type_ids).await?;
    let parameters =
        load_standard_parameters(transaction, header.revision, &value_type_ids).await?;

    let schema_names = schemas
        .iter()
        .map(|schema| (schema.definition.id(), schema.definition.name().clone()))
        .collect::<BTreeMap<_, _>>();
    let mut origins = Vec::with_capacity(
        schemas.len()
            + value_types.len()
            + enum_types.len()
            + bindings.len()
            + functions.len()
            + parameters.len(),
    );
    let schemas = schemas
        .into_iter()
        .map(|schema| {
            origins.push(schema.origin);
            schema.definition
        })
        .collect::<Vec<_>>();
    let mut definitions = Vec::with_capacity(value_types.len());
    for value_type in value_types {
        let record = DurableRecord::new(
            "_orna_kernel.standard_catalogue_value_types",
            value_type.definition.id().canonical(),
        );
        require_standard_definition_schema(
            &record,
            &schema_names,
            value_type.schema,
            value_type.definition.name(),
            "standard value type schema identity must identify a recovered schema",
            "standard value type qualified name must contain a schema namespace",
            "standard value type schema identity must equal the schema named by its namespace",
        )?;
        origins.push(value_type.origin);
        definitions.push(value_type.definition);
    }
    let mut enum_definitions = Vec::with_capacity(enum_types.len());
    for enum_type in enum_types {
        let record = DurableRecord::new(
            "_orna_kernel.standard_catalogue_enum_types",
            enum_type.definition.id().canonical(),
        );
        require_standard_definition_schema(
            &record,
            &schema_names,
            enum_type.schema,
            enum_type.definition.name(),
            "standard enum schema identity must identify a recovered schema",
            "standard enum qualified name must contain a schema namespace",
            "standard enum schema identity must equal the schema named by its namespace",
        )?;
        origins.push(enum_type.origin);
        enum_definitions.push(enum_type.definition);
    }
    let bindings = bindings
        .into_iter()
        .map(|binding| {
            origins.push(binding.origin);
            binding.binding
        })
        .collect::<Vec<_>>();
    let function_definitions = functions
        .into_iter()
        .map(|function| {
            let record = DurableRecord::new(
                "_orna_kernel.standard_catalogue_functions",
                function.id.canonical(),
            );
            require_standard_definition_schema(
                &record,
                &schema_names,
                function.schema,
                &function.name,
                "standard function schema identity must identify a recovered schema",
                "standard function qualified name must contain a schema namespace",
                "standard function schema identity must equal the schema named by its namespace",
            )?;
            let recovered_parameters = parameters.get(&function.id).cloned().unwrap_or_default();
            let definition = FunctionDefinition::new(
                function.id,
                function.name,
                function.domain,
                recovered_parameters
                    .iter()
                    .map(|parameter| parameter.definition.clone())
                    .collect(),
                function.return_type,
                function.current_revision,
                function.security,
                function.transaction,
                function.volatility,
            );
            origins.push(function.origin);
            origins.extend(
                recovered_parameters
                    .into_iter()
                    .map(|parameter| parameter.origin),
            );
            Ok(definition)
        })
        .collect::<Result<Vec<_>, PostgresKernelError>>()?;
    let catalogue = CatalogueSnapshot::new_with_functions_and_enum_types(
        header.catalogue,
        schemas,
        Vec::new(),
        definitions,
        enum_definitions,
        bindings,
        function_definitions,
    )
    .map_err(PostgresKernelError::CatalogueSnapshot)?;
    Ok((catalogue, origins))
}

#[allow(clippy::too_many_arguments)]
fn require_standard_definition_schema(
    record: &DurableRecord,
    schema_names: &BTreeMap<SchemaId, QualifiedSemanticName>,
    schema: SchemaId,
    name: &QualifiedSemanticName,
    missing_schema_rule: &'static str,
    missing_namespace_rule: &'static str,
    mismatch_rule: &'static str,
) -> Result<(), PostgresKernelError> {
    let schema_name = schema_names
        .get(&schema)
        .ok_or_else(|| record.invariant(missing_schema_rule))?;
    let name_parts = name.parts();
    let namespace = name_parts
        .get(..name_parts.len().saturating_sub(1))
        .filter(|parts| !parts.is_empty())
        .ok_or_else(|| record.invariant(missing_namespace_rule))?;
    if namespace != schema_name.parts() {
        return Err(record.invariant(mismatch_rule));
    }
    Ok(())
}

async fn load_standard_functions(
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

async fn load_standard_parameters(
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

async fn load_standard_schemas(
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

async fn load_standard_value_types(
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

pub(in super::super) fn recovered_standard_value_definition(
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

async fn load_standard_enum_types(
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

async fn load_standard_type_bindings(
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

pub(in super::super) fn decode_standard_binding_target(
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
