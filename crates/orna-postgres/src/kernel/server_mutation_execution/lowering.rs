use super::*;

pub(super) struct LoweredMutation {
    pub(super) sql: String,
    pub(super) bind_types: Vec<Type>,
    pub(super) binds: Vec<BindValue>,
}

struct MutationBindState {
    bind_types: Vec<Type>,
    binds: Vec<BindValue>,
    parameter_placeholders: BTreeMap<ParameterId, usize>,
    record_payload: usize,
}

#[cfg(test)]
pub(super) fn lower_insert_with_context(
    context: &orna_core::revision::CatalogueHashContext,
    plan: &ServerMutationPlan,
    arguments: &BTreeMap<ParameterId, BindValue>,
) -> Result<LoweredMutation, PostgresKernelError> {
    lower_insert_inner(None, context, plan, arguments)
}

pub(super) fn lower_insert_with_active(
    active: &ActiveDatabaseRevision,
    plan: &ServerMutationPlan,
    arguments: &BTreeMap<ParameterId, BindValue>,
) -> Result<LoweredMutation, PostgresKernelError> {
    lower_insert_inner(
        Some(active),
        active.catalogue_hash_context(),
        plan,
        arguments,
    )
}

fn lower_insert_inner(
    active: Option<&ActiveDatabaseRevision>,
    context: &orna_core::revision::CatalogueHashContext,
    plan: &ServerMutationPlan,
    arguments: &BTreeMap<ParameterId, BindValue>,
) -> Result<LoweredMutation, PostgresKernelError> {
    let mut columns = vec![String::from(OBJECT_ID_COLUMN)];
    let mut values = vec![String::from("$1")];
    let mut bind_state = MutationBindState {
        bind_types: vec![Type::BYTEA],
        binds: Vec::new(),
        parameter_placeholders: BTreeMap::new(),
        record_payload: 0,
    };
    for assignment in plan.assignments() {
        columns.push(field_name(assignment.field()));
        values.push(lower_assignment_expression(
            active,
            context,
            assignment.expression(),
            arguments,
            &mut bind_state,
        )?);
    }
    let sql = format!(
        "INSERT INTO {DATA_SCHEMA}.{} ({}) VALUES ({}) RETURNING {OBJECT_ID_COLUMN} AS c0",
        relation_name(plan.target()),
        columns.join(", "),
        values.join(", "),
    );
    if sql.len() > SQL_LIMIT {
        return Err(server_error(ServerInsertError::ComplexityLimit {
            category: "saved function complexity",
            maximum: SQL_LIMIT,
        }));
    }
    Ok(LoweredMutation {
        sql,
        bind_types: bind_state.bind_types,
        binds: bind_state.binds,
    })
}

#[cfg(test)]
pub(super) fn lower_insert(
    plan: &ServerMutationPlan,
    arguments: &BTreeMap<ParameterId, BindValue>,
) -> Result<LoweredMutation, PostgresKernelError> {
    lower_insert_with_context(
        &orna_core::revision::CatalogueHashContext::version_one(),
        plan,
        arguments,
    )
}

fn lower_assignment_expression(
    active: Option<&ActiveDatabaseRevision>,
    context: &orna_core::revision::CatalogueHashContext,
    expression: &server_mutation_plan::MutationExpression,
    arguments: &BTreeMap<ParameterId, BindValue>,
    bind_state: &mut MutationBindState,
) -> Result<String, PostgresKernelError> {
    match expression.kind() {
        MutationExpressionKind::Parameter { parameter, .. } => {
            let value_type = assignment_postgres_type(context, expression)?;
            parameter_placeholder(*parameter, value_type, arguments, bind_state)
        }
        MutationExpressionKind::BooleanLiteral { value } => {
            let value_type = assignment_postgres_type(context, expression)?;
            bind_state.binds.push(BindValue::Boolean(*value));
            bind_state.bind_types.push(value_type);
            Ok(format!("${}", bind_state.bind_types.len()))
        }
        MutationExpressionKind::TypedNull => {
            let value_type = assignment_postgres_type(context, expression)?;
            Ok(format!("CAST(NULL AS {})", value_type.name()))
        }
        MutationExpressionKind::RecordConstructor { fields } => lower_record_constructor(
            active.ok_or_else(|| {
                plan_invariant("record constructor lowering requires an active revision")
            })?,
            expression,
            fields,
            arguments,
            bind_state,
        ),
        _ => Err(plan_invariant(
            "unknown future mutation expression kinds are unsupported",
        )),
    }
}

fn assignment_postgres_type(
    context: &orna_core::revision::CatalogueHashContext,
    expression: &server_mutation_plan::MutationExpression,
) -> Result<Type, PostgresKernelError> {
    postgres_type(resolve_runtime_type(context, expression.resolved_type())).ok_or_else(|| {
        plan_invariant("the assignment type cannot be stored by the initial runtime")
    })
}

fn lower_record_constructor(
    active: &ActiveDatabaseRevision,
    expression: &server_mutation_plan::MutationExpression,
    fields: &[server_mutation_plan::RecordFieldExpression],
    arguments: &BTreeMap<ParameterId, BindValue>,
    bind_state: &mut MutationBindState,
) -> Result<String, PostgresKernelError> {
    let record_type = expression
        .resolved_type()
        .named_type()
        .ok_or_else(|| plan_invariant("validated record constructor must have a named type"))?;
    let record_definition = active
        .catalogue()
        .record_value_type_by_id(record_type)
        .ok_or_else(|| plan_invariant("validated record constructor type must be active"))?;
    if fields.len() != record_definition.fields().len() {
        return Err(plan_invariant(
            "validated record constructor field count must remain exact",
        ));
    }
    let values = fields
        .iter()
        .zip(record_definition.fields())
        .map(|(field, declared)| {
            let value = match field.kind() {
                RecordFieldExpressionKind::Parameter { parameter, .. } => arguments
                    .get(parameter)
                    .ok_or_else(|| {
                        plan_invariant(
                            "validated record constructor parameter must have one argument",
                        )
                    })?
                    .to_runtime(),
                RecordFieldExpressionKind::BooleanLiteral { value } => {
                    RuntimeValue::Boolean(*value)
                }
                _ => {
                    return Err(plan_invariant(
                        "unknown future record constructor child kinds are unsupported",
                    ));
                }
            };
            Ok((declared.name().to_owned(), value))
        })
        .collect::<Result<Vec<_>, PostgresKernelError>>()?;
    let record = RecordValue::new(active, record_type, values)
        .map_err(ServerMutationError::RecordValue)
        .map_err(server_error)?;
    let encoded = encode_active_value(active, &RuntimeValue::Record(record))
        .map_err(ServerMutationError::ValueCodec)
        .map_err(server_error)?;
    account_record_bind_payload(&mut bind_state.record_payload, encoded.len())?;
    bind_state.binds.push(BindValue::Bytes(encoded));
    bind_state.bind_types.push(Type::BYTEA);
    Ok(format!("${}", bind_state.bind_types.len()))
}

pub(super) fn account_record_bind_payload(
    total: &mut usize,
    encoded_length: usize,
) -> Result<(), PostgresKernelError> {
    let payload_length = encoded_length
        .checked_sub(ACTIVE_VALUE_ENVELOPE_LENGTH)
        .ok_or_else(|| {
            plan_invariant("canonical record bind must contain one complete ORV3 envelope")
        })?;
    let next = total
        .checked_add(payload_length)
        .ok_or_else(record_bind_payload_limit_error)?;
    if next > VARIABLE_ARGUMENT_PAYLOAD_LIMIT {
        return Err(record_bind_payload_limit_error());
    }
    *total = next;
    Ok(())
}

fn record_bind_payload_limit_error() -> PostgresKernelError {
    server_error(ServerInsertError::ComplexityLimit {
        category: "total size of canonical record payloads",
        maximum: VARIABLE_ARGUMENT_PAYLOAD_LIMIT,
    })
}

fn parameter_placeholder(
    parameter: ParameterId,
    value_type: Type,
    arguments: &BTreeMap<ParameterId, BindValue>,
    bind_state: &mut MutationBindState,
) -> Result<String, PostgresKernelError> {
    if let Some(placeholder) = bind_state.parameter_placeholders.get(&parameter).copied() {
        return Ok(format!("${placeholder}"));
    }
    let value = arguments.get(&parameter).ok_or_else(|| {
        plan_invariant("validated parameter expression must have one runtime argument")
    })?;
    bind_state.binds.push(value.clone());
    bind_state.bind_types.push(value_type);
    let placeholder = bind_state.bind_types.len();
    bind_state
        .parameter_placeholders
        .insert(parameter, placeholder);
    Ok(format!("${placeholder}"))
}

pub(super) fn lower_update_with_context(
    context: &orna_core::revision::CatalogueHashContext,
    plan: &ServerMutationPlan,
    arguments: &BTreeMap<ParameterId, BindValue>,
) -> Result<LoweredMutation, PostgresKernelError> {
    let ServerMutationOperation::Update { selector } = plan.operation() else {
        return Err(plan_invariant("UPDATE execution requires an UPDATE plan"));
    };
    let mut assignments = Vec::with_capacity(plan.assignments().len());
    let mut bind_state = MutationBindState {
        bind_types: Vec::new(),
        binds: Vec::new(),
        parameter_placeholders: BTreeMap::new(),
        record_payload: 0,
    };
    for assignment in plan.assignments() {
        let value = lower_assignment_expression(
            None,
            context,
            assignment.expression(),
            arguments,
            &mut bind_state,
        )?;
        assignments.push(format!("{} = {value}", field_name(assignment.field())));
    }
    let selector_placeholder = parameter_placeholder(
        selector.parameter(),
        Type::BYTEA,
        arguments,
        &mut bind_state,
    )?;
    let sql = format!(
        "UPDATE {DATA_SCHEMA}.{} SET {} WHERE {OBJECT_ID_COLUMN} = {selector_placeholder} RETURNING {OBJECT_ID_COLUMN} AS c0",
        relation_name(plan.target()),
        assignments.join(", "),
    );
    if sql.len() > SQL_LIMIT {
        return Err(server_error(ServerInsertError::ComplexityLimit {
            category: "saved function complexity",
            maximum: SQL_LIMIT,
        }));
    }
    Ok(LoweredMutation {
        sql,
        bind_types: bind_state.bind_types,
        binds: bind_state.binds,
    })
}

#[cfg(test)]
pub(super) fn lower_update(
    plan: &ServerMutationPlan,
    arguments: &BTreeMap<ParameterId, BindValue>,
) -> Result<LoweredMutation, PostgresKernelError> {
    lower_update_with_context(
        &orna_core::revision::CatalogueHashContext::version_one(),
        plan,
        arguments,
    )
}

pub(super) fn lower_delete(
    plan: &ServerDeletePlan,
    arguments: &BTreeMap<ParameterId, BindValue>,
) -> Result<LoweredMutation, PostgresKernelError> {
    let selector = plan.selector();
    let value = arguments.get(&selector.parameter()).ok_or_else(|| {
        plan_invariant("validated DELETE selector must have one runtime argument")
    })?;
    let sql = format!(
        "DELETE FROM {DATA_SCHEMA}.{} WHERE {OBJECT_ID_COLUMN} = $1 RETURNING {OBJECT_ID_COLUMN} AS c0",
        relation_name(plan.target()),
    );
    if sql.len() > SQL_LIMIT {
        return Err(server_error(ServerMutationError::ComplexityLimit {
            category: "saved function complexity",
            maximum: SQL_LIMIT,
        }));
    }
    Ok(LoweredMutation {
        sql,
        bind_types: vec![Type::BYTEA],
        binds: vec![value.clone()],
    })
}

pub(super) fn validate_prepared_result(
    statement: &Statement,
    operation: &'static str,
) -> Result<(), PostgresKernelError> {
    let [column] = statement.columns() else {
        return Err(server_error(ServerInsertError::PreparedResult {
            rule: match operation {
                "INSERT" => "prepared INSERT must return exactly one column",
                "UPDATE" => "prepared UPDATE must return exactly one column",
                "DELETE" => "prepared DELETE must return exactly one column",
                _ => "prepared mutation must return exactly one column",
            },
        }));
    };
    if column.name() != "c0" || *column.type_() != Type::BYTEA {
        return Err(server_error(ServerInsertError::PreparedResult {
            rule: match operation {
                "INSERT" => "prepared INSERT must return one BYTEA column named c0",
                "UPDATE" => "prepared UPDATE must return one BYTEA column named c0",
                "DELETE" => "prepared DELETE must return one BYTEA column named c0",
                _ => "prepared mutation must return one BYTEA column named c0",
            },
        }));
    }
    Ok(())
}
