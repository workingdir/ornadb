use super::*;

pub(super) struct LoweredPlan {
    pub(super) sql: String,
    pub(super) bind_types: Vec<Type>,
    pub(super) binds: Vec<SelectBindValue>,
    pub(super) guards: Vec<VariableGuard>,
    pub(super) variable_payload_limit: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum SelectBindValue {
    Boolean(bool),
    Bytes(Vec<u8>),
    Text(String),
}

impl SelectBindValue {
    pub(super) fn bind_type(&self) -> Type {
        match self {
            Self::Boolean(_) => Type::BOOL,
            Self::Bytes(_) => Type::BYTEA,
            Self::Text(_) => Type::TEXT,
        }
    }

    pub(super) fn as_to_sql(&self) -> &(dyn ToSql + Sync) {
        match self {
            Self::Boolean(value) => value,
            Self::Bytes(value) => value,
            Self::Text(value) => value,
        }
    }
}

pub(super) struct VariableGuard {
    pub(super) column: usize,
    alias: String,
}

pub(super) struct PartialLoweredSelect<'a> {
    lowerer: Lowerer<'a>,
    pub(super) projections: Vec<String>,
    guards: Vec<VariableGuard>,
    pub(super) variable_payload_limit: usize,
}

#[derive(Clone, Copy)]
pub(super) struct RuntimeResultColumns<'a> {
    pub(super) context: &'a CatalogueHashContext,
    pub(super) columns: &'a [ResultColumn],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DuplicatePolicy {
    Preserve,
    Distinct,
}

impl DuplicatePolicy {
    const fn select_sql(self) -> &'static str {
        match self {
            Self::Preserve => "SELECT",
            Self::Distinct => "SELECT DISTINCT",
        }
    }
}

pub(super) fn lower_plan(
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
    context: &CatalogueHashContext,
    plan: &ServerPlan,
    columns: &[ResultColumn],
) -> Result<LoweredPlan, PostgresKernelError> {
    let result_columns = RuntimeResultColumns { context, columns };
    lower_parameter_free_plan(
        catalogue,
        plan.scan.object_type,
        &plan.projections,
        plan.selection.as_ref(),
        &plan.ordering,
        DuplicatePolicy::Preserve,
        result_columns,
    )
}

pub(super) fn lower_distinct_plan(
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
    context: &CatalogueHashContext,
    plan: &DistinctServerPlan,
    columns: &[ResultColumn],
) -> Result<LoweredPlan, PostgresKernelError> {
    let result_columns = RuntimeResultColumns { context, columns };
    lower_parameter_free_plan(
        catalogue,
        plan.scan().object_type,
        plan.projections(),
        plan.selection(),
        &[],
        DuplicatePolicy::Distinct,
        result_columns,
    )
}

fn lower_parameter_free_plan(
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
    scan: TypeId,
    projections: &[Expression],
    selection: Option<&Expression>,
    ordering: &[Ordering],
    duplicate_policy: DuplicatePolicy,
    result_columns: RuntimeResultColumns<'_>,
) -> Result<LoweredPlan, PostgresKernelError> {
    let mut lowered = lower_select_projections(catalogue, result_columns, scan, projections)?;
    let selection = selection
        .map(|expression| lowered.lowerer.expression(expression))
        .transpose()?;
    let mut lowered_ordering = Vec::with_capacity(ordering.len());
    for item in ordering {
        let direction = ordering_sql(item.direction);
        lowered_ordering.push(format!(
            "{} {direction}",
            lowered.lowerer.expression(&item.expression)?
        ));
    }
    let mut suffix = String::new();
    if let Some(selection) = selection {
        suffix.push_str("\nWHERE ");
        suffix.push_str(&selection);
    }
    if !lowered_ordering.is_empty() {
        suffix.push_str("\nORDER BY ");
        suffix.push_str(&lowered_ordering.join(", "));
    }
    let limit = effective_query_limit(projections.len())?;
    suffix.push_str(&format!("\nLIMIT {limit}"));
    finish_lowered_select(lowered, duplicate_policy, &suffix)
}

pub(super) fn lower_identity_selected_plan(
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
    context: &CatalogueHashContext,
    plan: &IdentitySelectedServerPlan,
    columns: &[ResultColumn],
    selector: ObjectId,
) -> Result<LoweredPlan, PostgresKernelError> {
    let scan = plan.scan();
    let result_columns = RuntimeResultColumns { context, columns };
    let mut lowered = lower_select_projections(
        catalogue,
        result_columns,
        scan.object_type,
        plan.projections(),
    )?;
    lowered
        .lowerer
        .binds
        .push(SelectBindValue::Bytes(selector.to_bytes().to_vec()));
    let selector_placeholder = lowered.lowerer.binds.len();
    let suffix = format!("\nWHERE i0.{OBJECT_ID_COLUMN} = ${selector_placeholder}\nLIMIT 2");
    finish_lowered_select(lowered, DuplicatePolicy::Preserve, &suffix)
}

pub(super) fn lower_unique_text_selected_plan(
    catalogue: &CatalogueSnapshot,
    context: &CatalogueHashContext,
    plan: &UniqueTextSelectedServerPlan,
    columns: &[ResultColumn],
    selector: String,
) -> Result<LoweredPlan, PostgresKernelError> {
    let scan = plan.scan();
    let result_columns = RuntimeResultColumns { context, columns };
    let mut lowered = lower_select_projections(
        catalogue,
        result_columns,
        scan.object_type,
        plan.projections(),
    )?;
    let UniqueTextSelectBindValue::Text { field, .. } = plan.selector();
    lowered.lowerer.binds.push(SelectBindValue::Text(selector));
    let selector_placeholder = lowered.lowerer.binds.len();
    let suffix = format!(
        "\nWHERE i0.{} = ${selector_placeholder}\nLIMIT 2",
        field_name(*field),
    );
    finish_lowered_select(lowered, DuplicatePolicy::Preserve, &suffix)
}

pub(super) fn lower_select_projections<'a>(
    catalogue: &'a orna_core::catalogue::CatalogueSnapshot,
    result_columns: RuntimeResultColumns<'_>,
    scan: TypeId,
    expressions: &[Expression],
) -> Result<PartialLoweredSelect<'a>, PostgresKernelError> {
    let context = result_columns.context;
    let columns = result_columns.columns;
    let mut lowerer = Lowerer {
        catalogue,
        scan,
        joins: BTreeMap::new(),
        join_sql: Vec::new(),
        binds: Vec::new(),
        field_path_steps: 0,
    };
    let variable_payload_limit = variable_payload_limit(catalogue, context, columns)?;
    let mut projections = Vec::with_capacity(expressions.len());
    let mut guard_projections = Vec::new();
    let mut guards = Vec::new();
    for (index, expression) in expressions.iter().enumerate() {
        let expression = lowerer.expression(expression)?;
        if is_variable_type(catalogue, context, columns[index].resolved_type()) {
            let guarded_payload_limit = if matches!(
                resolve_catalogue_runtime_type(catalogue, context, columns[index].resolved_type(),),
                ResolvedRuntimeType::Record(_)
            ) {
                variable_payload_limit
                    .checked_add(ACTIVE_VALUE_ENVELOPE_LENGTH)
                    .ok_or_else(|| {
                        server_error(ServerSelectError::PayloadLimit {
                            maximum: PAYLOAD_LIMIT,
                        })
                    })?
            } else {
                variable_payload_limit
            };
            let alias = format!("g{}", guards.len());
            projections.push(format!(
            "CASE WHEN octet_length({expression}) <= {guarded_payload_limit} THEN {expression} ELSE NULL END AS c{index}"
        ));
            guards.push(VariableGuard {
                column: index,
                alias: alias.clone(),
            });
            guard_projections.push(format!(
            "CASE WHEN {expression} IS NULL OR octet_length({expression}) <= {guarded_payload_limit} THEN TRUE ELSE FALSE END AS {alias}"
        ));
        } else {
            projections.push(format!("{expression} AS c{index}"));
        }
    }
    projections.extend(guard_projections);
    Ok(PartialLoweredSelect {
        lowerer,
        projections,
        guards,
        variable_payload_limit,
    })
}

fn finish_lowered_select(
    lowered: PartialLoweredSelect<'_>,
    duplicate_policy: DuplicatePolicy,
    suffix: &str,
) -> Result<LoweredPlan, PostgresKernelError> {
    let mut sql = format!(
        "{} {}\nFROM {}.{} AS i0",
        duplicate_policy.select_sql(),
        lowered.projections.join(", "),
        DATA_SCHEMA,
        relation_name(lowered.lowerer.scan),
    );
    for join in &lowered.lowerer.join_sql {
        sql.push('\n');
        sql.push_str(join);
    }
    sql.push_str(suffix);
    if sql.len() > SQL_LIMIT {
        return Err(server_error(ServerSelectError::ComplexityLimit {
            category: "generated SQL bytes",
            maximum: SQL_LIMIT,
        }));
    }
    Ok(LoweredPlan {
        sql,
        bind_types: lowered
            .lowerer
            .binds
            .iter()
            .map(SelectBindValue::bind_type)
            .collect(),
        binds: lowered.lowerer.binds,
        guards: lowered.guards,
        variable_payload_limit: lowered.variable_payload_limit,
    })
}

// Version 1 fixes Orna's unspecified ordering independently of the PostgreSQL
// defaults, so every generated term names its null rule.
pub(super) const fn ordering_sql(direction: SortDirection) -> &'static str {
    match direction {
        SortDirection::Unspecified | SortDirection::Ascending => "ASC NULLS LAST",
        SortDirection::Descending => "DESC NULLS FIRST",
    }
}

pub(super) fn is_variable_type(
    catalogue: &CatalogueSnapshot,
    context: &CatalogueHashContext,
    resolved_type: ResolvedType,
) -> bool {
    matches!(
        resolve_catalogue_runtime_type(catalogue, context, resolved_type),
        ResolvedRuntimeType::CatalogueEnum(_) | ResolvedRuntimeType::Record(_)
    ) || matches!(
        resolve_runtime_type(context, resolved_type).compatibility_scalar(),
        Some(StandardScalar::CharacterLargeObject | StandardScalar::BinaryLargeObject)
    )
}

pub(super) fn variable_payload_limit(
    catalogue: &CatalogueSnapshot,
    context: &CatalogueHashContext,
    columns: &[ResultColumn],
) -> Result<usize, PostgresKernelError> {
    let names = initial_payload_len(columns)?;
    let fixed = columns
        .iter()
        .filter(|column| !is_variable_type(catalogue, context, column.resolved_type()))
        .try_fold(0usize, |total, column| {
            total
                .checked_add(maximum_fixed_payload_len(
                    catalogue,
                    context,
                    column.resolved_type(),
                ))
                .ok_or_else(|| {
                    server_error(ServerSelectError::PayloadLimit {
                        maximum: PAYLOAD_LIMIT,
                    })
                })
        })?;
    let available = PAYLOAD_LIMIT
        .checked_sub(names)
        .and_then(|available| available.checked_sub(fixed))
        .ok_or_else(|| {
            server_error(ServerSelectError::PayloadLimit {
                maximum: PAYLOAD_LIMIT,
            })
        })?;
    let variable_count = columns
        .iter()
        .filter(|column| is_variable_type(catalogue, context, column.resolved_type()))
        .count();
    if variable_count == 0 {
        return Ok(0);
    }
    Ok(available / variable_count)
}

fn maximum_fixed_payload_len(
    catalogue: &CatalogueSnapshot,
    context: &CatalogueHashContext,
    resolved_type: ResolvedType,
) -> usize {
    match resolve_catalogue_runtime_type(catalogue, context, resolved_type) {
        runtime if runtime.compatibility_scalar() == Some(StandardScalar::Boolean) => 1,
        runtime if runtime.compatibility_scalar() == Some(StandardScalar::Integer) => 4,
        runtime
            if matches!(
                runtime.compatibility_scalar(),
                Some(StandardScalar::BigInt | StandardScalar::Float)
            ) =>
        {
            8
        }
        ResolvedRuntimeType::Reference(_) => 16,
        ResolvedRuntimeType::CatalogueEnum(_) | ResolvedRuntimeType::Record(_) => 0,
        ResolvedRuntimeType::LegacyScalar(_)
        | ResolvedRuntimeType::VerifiedValue { .. }
        | ResolvedRuntimeType::Unsupported => 0,
    }
}

pub(super) fn effective_query_limit(projection_count: usize) -> Result<usize, PostgresKernelError> {
    let cell_rows = CELL_LIMIT
        .checked_div(projection_count)
        .ok_or_else(|| plan_invariant("server plan must contain at least one projection"))?;
    let effective = ROW_LIMIT.min(cell_rows);
    effective
        .checked_add(1)
        .ok_or_else(|| plan_invariant("effective server row limit must fit usize"))
}

struct Lowerer<'a> {
    catalogue: &'a orna_core::catalogue::CatalogueSnapshot,
    scan: TypeId,
    joins: BTreeMap<Vec<(TypeId, FieldId)>, String>,
    join_sql: Vec<String>,
    binds: Vec<SelectBindValue>,
    field_path_steps: usize,
}

impl Lowerer<'_> {
    fn expression(&mut self, expression: &Expression) -> Result<String, PostgresKernelError> {
        match &expression.kind {
            ExpressionKind::ObjectReference { .. } => Ok(format!("i0.{OBJECT_ID_COLUMN}")),
            ExpressionKind::FieldPath { steps, .. } => self.field_path(steps),
            ExpressionKind::BooleanLiteral { value } => {
                if self.binds.len() == server_plan::MAX_EXPRESSION_NODES as usize {
                    return Err(server_error(ServerSelectError::ComplexityLimit {
                        category: "boolean binds",
                        maximum: server_plan::MAX_EXPRESSION_NODES as usize,
                    }));
                }
                self.binds.push(SelectBindValue::Boolean(*value));
                Ok(format!("${}", self.binds.len()))
            }
            ExpressionKind::Equality { left, right } => Ok(format!(
                "({} = {})",
                self.expression(left)?,
                self.expression(right)?,
            )),
        }
    }

    fn field_path(&mut self, steps: &[FieldStep]) -> Result<String, PostgresKernelError> {
        let mut owner = self.scan;
        let mut alias = String::from("i0");
        let mut prefix = Vec::new();
        let mut nullable = false;
        for (index, step) in steps.iter().enumerate() {
            self.field_path_steps = self.field_path_steps.checked_add(1).ok_or_else(|| {
                server_error(ServerSelectError::ComplexityLimit {
                    category: "field path steps",
                    maximum: FIELD_PATH_STEP_LIMIT,
                })
            })?;
            if self.field_path_steps > FIELD_PATH_STEP_LIMIT {
                return Err(server_error(ServerSelectError::ComplexityLimit {
                    category: "field path steps",
                    maximum: FIELD_PATH_STEP_LIMIT,
                }));
            }
            let field = self
                .catalogue
                .object_type_by_id(owner)
                .and_then(|object| object.field_by_id(step.field))
                .ok_or_else(|| plan_invariant("field path field must exist while lowering"))?;
            if index + 1 == steps.len() {
                return Ok(format!("{alias}.{}", field_name(step.field)));
            }
            let Some(target) = field.resolved_type().reference_target() else {
                return Err(plan_invariant(
                    "non-final lowered field path hop must be a reference",
                ));
            };
            prefix.push((step.owner, step.field));
            nullable |= field.nullable();
            let prefix_alias = if let Some(alias) = self.joins.get(&prefix) {
                alias.clone()
            } else {
                if self.joins.len() == JOIN_LIMIT {
                    return Err(server_error(ServerSelectError::ComplexityLimit {
                        category: "unique joins",
                        maximum: JOIN_LIMIT,
                    }));
                }
                let joined = format!("j{}", self.joins.len());
                let join = if nullable { "LEFT JOIN" } else { "JOIN" };
                self.join_sql.push(format!(
                    "{join} {}.{} AS {joined} ON {alias}.{} = {joined}.{OBJECT_ID_COLUMN}",
                    DATA_SCHEMA,
                    relation_name(target),
                    field_name(step.field),
                ));
                self.joins.insert(prefix.clone(), joined.clone());
                joined
            };
            alias = prefix_alias;
            owner = target;
        }
        Err(plan_invariant(
            "field path must contain at least one field while lowering",
        ))
    }
}

pub(super) fn validate_prepared_columns(
    catalogue: &CatalogueSnapshot,
    context: &CatalogueHashContext,
    statement: &Statement,
    expected: &[ResultColumn],
    guards: &[VariableGuard],
) -> Result<(), PostgresKernelError> {
    if statement.columns().len() != expected.len() + guards.len() {
        return Err(server_error(ServerSelectError::PreparedResult {
            rule: "prepared result column count must equal declared ROWS shape",
        }));
    }
    for (index, (column, expected)) in statement.columns().iter().zip(expected).enumerate() {
        if column.name() != format!("c{index}")
            || *column.type_()
                != expected_postgres_type(catalogue, context, expected.resolved_type())?
        {
            return Err(server_error(ServerSelectError::PreparedResult {
                rule: "prepared result column name and PostgreSQL type must match generated shape",
            }));
        }
    }
    for (column, guard) in statement.columns()[expected.len()..].iter().zip(guards) {
        if column.name() != guard.alias || *column.type_() != Type::BOOL {
            return Err(server_error(ServerSelectError::PreparedResult {
                rule: "prepared variable payload guards must use generated BOOLEAN columns",
            }));
        }
    }
    Ok(())
}

pub(super) fn expected_postgres_type(
    catalogue: &CatalogueSnapshot,
    context: &CatalogueHashContext,
    resolved_type: ResolvedType,
) -> Result<Type, PostgresKernelError> {
    postgres_type(resolve_catalogue_runtime_type(
        catalogue,
        context,
        resolved_type,
    ))
    .ok_or_else(|| {
        server_error(ServerSelectError::PreparedResult {
            rule: "result type is outside the initial runtime subset",
        })
    })
}
