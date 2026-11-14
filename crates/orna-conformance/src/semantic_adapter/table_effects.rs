use super::*;

/// System relations whose rows the durable run host serves from runtime
/// publication metadata.
pub(super) const PUBLICATION_RELATIONS: [SysRelation; 2] =
    [SysRelation::Storage, SysRelation::MaintenanceJob];

pub(super) struct TableEffectHandler<'activation, 'runtime> {
    pub(super) activation: &'activation mut TransactionActivation<'runtime>,
    pub(super) key_fields: &'activation TableKeys,
    pub(super) table_fields: &'activation TableFields,
    pub(super) mutations: &'activation mut Vec<TableMutation>,
    pub(super) next_mutation: u64,
    pub(super) limits: EvaluatorLimits,
    pub(super) activation_time: Option<SystemTime>,
    pub(super) publication_rows: Option<RuntimePublicationMetadataRows>,
    pub(super) mutation_scope: [u8; 32],
    pub(super) sys_host: Option<&'activation SysHostAttachment>,
    pub(super) explain_catalogue: Option<&'activation ActivationExplainCatalogue>,
}

impl EffectHandler for TableEffectHandler<'_, '_> {
    fn sys_host_context(&self) -> Option<&SysHostAttachment> {
        self.sys_host
    }

    fn scan_relation_page(
        &mut self,
        source: &str,
        after: Option<&[u8]>,
        max_rows: usize,
        budget: &mut StepBudget,
    ) -> Result<Option<RelationPage>, EvaluationError> {
        if max_rows == 0 {
            return Ok(Some(RelationPage {
                rows: Vec::new(),
                next: None,
            }));
        }
        if let Some(row) = match (source, self.publication_rows.as_ref()) {
            (name, Some(rows)) if name == SysRelation::Storage.name() => Some(&rows.sys_storage),
            (name, Some(rows)) if name == SysRelation::MaintenanceJob.name() => {
                Some(&rows.maintenance_job)
            }
            _ => None,
        } {
            if after.is_some() {
                return Ok(Some(RelationPage {
                    rows: Vec::new(),
                    next: None,
                }));
            }
            budget.debit(1)?;
            return Ok(Some(RelationPage {
                rows: vec![row.clone()],
                next: None,
            }));
        }
        if !self.key_fields.contains_key(source) || !self.table_fields.contains_key(source) {
            return Ok(None);
        }
        let table = source.to_owned();
        let range = match after {
            Some(after) => (
                Bound::Excluded(
                    transaction_cursor_key(
                        after,
                        self.key_fields
                            .get(source)
                            .expect("declared table key checked above"),
                    )
                    .map_err(cursor_error)?,
                ),
                Bound::Unbounded,
            ),
            None => (Bound::Unbounded, Bound::Unbounded),
        };
        let mut candidates = self
            .activation
            .candidate_scan_range(&table, range)
            .map_err(|error| transaction_error(table_error_code(error)))?;
        let mut rows = Vec::with_capacity(max_rows);
        let mut last_key = None;
        for (key, row) in candidates.by_ref().take(max_rows) {
            budget.debit(1)?;
            last_key = Some(key);
            rows.push(row);
        }
        Ok(Some(RelationPage {
            rows,
            next: candidates
                .next()
                .is_some()
                .then(|| {
                    last_key.as_ref().map(|key| {
                        transaction_continuation_cursor(
                            key,
                            self.key_fields
                                .get(source)
                                .expect("declared table key checked above"),
                        )
                    })
                })
                .flatten(),
        }))
    }

    fn handle(
        &mut self,
        callee: &Expr,
        arguments: &[Value],
    ) -> Result<Option<Value>, EvaluationError> {
        let mutation_len = self.mutations.len();
        let savepoint = self
            .activation
            .savepoint()
            .map_err(|error| transaction_error(table_error_code(error)))?;
        let result = self.handle_inner_with_budget(callee, arguments, None);
        if result.is_err() {
            self.activation
                .rollback_to(savepoint)
                .map_err(|error| transaction_error(table_error_code(error)))?;
            self.mutations.truncate(mutation_len);
        }
        result
    }

    fn handle_with_budget(
        &mut self,
        callee: &Expr,
        arguments: &[Value],
        budget: &mut StepBudget,
    ) -> Result<Option<Value>, EvaluationError> {
        let mutation_len = self.mutations.len();
        let savepoint = self
            .activation
            .savepoint()
            .map_err(|error| transaction_error(table_error_code(error)))?;
        let result = self.handle_inner_with_budget(callee, arguments, Some(budget));
        if result.is_err() {
            self.activation
                .rollback_to(savepoint)
                .map_err(|error| transaction_error(table_error_code(error)))?;
            self.mutations.truncate(mutation_len);
        }
        result
    }

    fn explain_catalogue(&self) -> Option<&dyn ExplainCatalogue> {
        self.explain_catalogue
            .map(|catalogue| catalogue as &dyn ExplainCatalogue)
    }
}

impl TableEffectHandler<'_, '_> {
    pub(super) fn handle_inner_with_budget(
        &mut self,
        callee: &Expr,
        arguments: &[Value],
        mut budget: Option<&mut StepBudget>,
    ) -> Result<Option<Value>, EvaluationError> {
        if matches!(callee, Expr::Name { text, .. } if text == "uuid7") {
            if !arguments.is_empty() {
                return Err(transaction_error("ORNA-EVAL-TABLE-ARGUMENT"));
            }
            let Some(activation_time) = self.activation_time else {
                return Err(transaction_error("ORNA-EVAL-TABLE-VALUE"));
            };
            return canonical_uuid7(activation_time).map(Some);
        }
        if matches!(callee, Expr::Name { text, .. } if text == "now") {
            if !arguments.is_empty() {
                return Err(transaction_error("ORNA-EVAL-TABLE-ARGUMENT"));
            }
            let Some(activation_time) = self.activation_time else {
                return Ok(None);
            };
            return Ok(Some(canonical_instant(activation_time)?));
        }
        if matches!(
            callee,
            Expr::Field {
                base,
                name,
                ..
            } if matches!(base.as_ref(), Expr::ReplBinding { text, .. } if text == "$__orna_relation")
                && name == "aggregate"
        ) {
            let [table, field, operation, kind] = arguments else {
                return Err(transaction_error("ORNA-EVAL-TABLE-ARGUMENT"));
            };
            let (
                OvbRaw::Text(table),
                OvbRaw::Text(field),
                OvbRaw::Text(operation),
                OvbRaw::Text(kind),
            ) = (table.raw(), field.raw(), operation.raw(), kind.raw())
            else {
                return Err(transaction_error("ORNA-EVAL-TABLE-ARGUMENT"));
            };
            let operation = operation.as_str();
            let kind = kind.as_str();
            if !self.key_fields.contains_key(table)
                || !matches!(operation, "min" | "max" | "sum")
                || !matches!(kind, "integer" | "float" | "decimal")
                || kind == "float" && operation != "sum"
            {
                return Err(transaction_error("ORNA-EVAL-TABLE-ARGUMENT"));
            }
            let mut integer_extreme = None;
            let mut decimal_extreme = None;
            let mut integer_total = BigInt::ZERO;
            let mut float_total = None;
            let mut decimal_total = Value::decimal(BigInt::ZERO, BigInt::ZERO)
                .map_err(|_| transaction_error("ORNA-EVAL-TABLE-ARGUMENT"))?;
            let decimal_addition = parse_expression("left + right").value;
            let mut decimal_budget = StepBudget::new(self.limits.max_steps);
            let decimal_functions = Functions::new();
            let mut candidate_rows: usize = 0;
            for (_, row) in self
                .activation
                .candidate_relation(table)
                .map_err(|error| transaction_error(table_error_code(error)))?
            {
                debit_effect_step(&mut budget)?;
                candidate_rows = candidate_rows
                    .checked_add(1)
                    .ok_or_else(|| transaction_error("ORNA-EVAL-LIMIT"))?;
                self.limits
                    .check_items(candidate_rows)
                    .map_err(|_| transaction_error("ORNA-EVAL-LIMIT"))?;
                let value = record_field(&row, field)
                    .ok_or_else(|| transaction_error("ORNA-EVAL-UNSUPPORTED"))?;
                match value.raw() {
                    OvbRaw::Int(value) if kind == "integer" => {
                        self.limits
                            .check_integer(value)
                            .map_err(|_| transaction_error("ORNA-EVAL-LIMIT"))?;
                        match operation {
                            "sum" => {
                                integer_total += value;
                                self.limits
                                    .check_integer(&integer_total)
                                    .map_err(|_| transaction_error("ORNA-EVAL-LIMIT"))?;
                            }
                            "min" => {
                                if integer_extreme
                                    .as_ref()
                                    .is_none_or(|current: &BigInt| value < current)
                                {
                                    integer_extreme = Some(value.clone());
                                }
                            }
                            "max" => {
                                if integer_extreme
                                    .as_ref()
                                    .is_none_or(|current: &BigInt| value > current)
                                {
                                    integer_extreme = Some(value.clone());
                                }
                            }
                            _ => unreachable!("aggregate operation was checked above"),
                        }
                    }
                    OvbRaw::Float(bits) if kind == "float" => {
                        let canonical = Value::new(OvbRaw::Float(*bits))
                            .map_err(|_| transaction_error("ORNA-EVAL-UNSUPPORTED"))?;
                        let OvbRaw::Float(bits) = canonical.raw() else {
                            unreachable!("Float canonicalization preserves the Float variant")
                        };
                        let value = f64::from_bits(*bits);
                        float_total = Some(float_total.map_or(value, |total: f64| total + value));
                    }
                    OvbRaw::Tag(60000, _) if kind == "decimal" => {
                        if operation != "sum" {
                            let replace = decimal_extreme.as_ref().is_none_or(|current: &Value| {
                                let (OvbRaw::Tag(60000, left), OvbRaw::Tag(60000, right)) =
                                    (value.raw(), current.raw())
                                else {
                                    unreachable!("validated Decimal extrema retain Decimal tags");
                                };
                                let ordering = decimal_key_cmp(left, right);
                                (operation == "min" && ordering == Ordering::Less)
                                    || (operation == "max" && ordering == Ordering::Greater)
                            });
                            if replace {
                                decimal_extreme = Some(value.clone());
                            }
                            continue;
                        }
                        let mut environment = Environment::new();
                        environment.insert("left".into(), decimal_total.clone());
                        environment.insert("right".into(), value);
                        decimal_total = match budget.as_deref_mut() {
                            Some(shared_budget) => evaluate_with_functions_and_budget(
                                &decimal_addition,
                                &environment,
                                &decimal_functions,
                                self.limits,
                                shared_budget,
                            ),
                            None => evaluate_with_functions_and_budget(
                                &decimal_addition,
                                &environment,
                                &decimal_functions,
                                self.limits,
                                &mut decimal_budget,
                            ),
                        }?;
                    }
                    _ => return Err(transaction_error("ORNA-EVAL-UNSUPPORTED")),
                }
            }
            return match operation {
                "sum" if kind == "float" => Ok(Some(Value::float_bits(
                    float_total.map_or(0.0f64.to_bits(), f64::to_bits),
                ))),
                "sum" if kind == "decimal" => Ok(Some(decimal_total)),
                "sum" => Ok(Some(Value::int(integer_total))),
                "min" | "max" => {
                    let extreme = match kind {
                        "integer" => integer_extreme.map(Value::int),
                        "decimal" => decimal_extreme,
                        _ => None,
                    };
                    match extreme {
                        Some(value) => Value::option(Some(value))
                            .map(Some)
                            .map_err(|_| transaction_error("ORNA-EVAL-TABLE-ARGUMENT")),
                        None => {
                            Ok(Some(Value::new(OvbRaw::Null).map_err(|_| {
                                transaction_error("ORNA-EVAL-TABLE-ARGUMENT")
                            })?))
                        }
                    }
                }
                _ => unreachable!("aggregate operation was checked above"),
            };
        }
        if matches!(
            callee,
            Expr::Field {
                base,
                name,
                ..
            } if matches!(base.as_ref(), Expr::ReplBinding { text, .. } if text == "$__orna_relation")
                && name == "filtered_first"
        ) {
            let [table, field, expected] = arguments else {
                return Err(transaction_error("ORNA-EVAL-TABLE-ARGUMENT"));
            };
            let (OvbRaw::Text(table), OvbRaw::Text(field)) = (table.raw(), field.raw()) else {
                return Err(transaction_error("ORNA-EVAL-TABLE-ARGUMENT"));
            };
            if !self.key_fields.contains_key(table)
                || !self
                    .table_fields
                    .get(table)
                    .is_some_and(|fields| fields.contains(field))
                || !matches!(expected.raw(), OvbRaw::Tag(60000, _))
            {
                return Err(transaction_error("ORNA-EVAL-TABLE-ARGUMENT"));
            }
            let mut first = None;
            for (_, row) in self
                .activation
                .candidate_relation(table)
                .map_err(|error| transaction_error(table_error_code(error)))?
            {
                debit_effect_step(&mut budget)?;
                let value = record_field(&row, field)
                    .ok_or_else(|| transaction_error("ORNA-EVAL-UNSUPPORTED"))?;
                let matches = match (value.raw(), expected.raw()) {
                    (OvbRaw::Tag(60000, left), OvbRaw::Tag(60000, right)) => {
                        decimal_key_cmp(left, right) == Ordering::Equal
                    }
                    _ => false,
                };
                if matches {
                    first = Some(row);
                    break;
                }
            }
            return Value::option(first)
                .map(Some)
                .map_err(|_| transaction_error("ORNA-EVAL-TABLE-ARGUMENT"));
        }
        if matches!(
            callee,
            Expr::Field {
                base,
                name,
                ..
            } if matches!(base.as_ref(), Expr::ReplBinding { text, .. } if text == "$__orna_relation")
                && name == "filtered_count"
        ) {
            let [table, field, expected] = arguments else {
                return Err(transaction_error("ORNA-EVAL-TABLE-ARGUMENT"));
            };
            let (OvbRaw::Text(table), OvbRaw::Text(field)) = (table.raw(), field.raw()) else {
                return Err(transaction_error("ORNA-EVAL-TABLE-ARGUMENT"));
            };
            if !self.key_fields.contains_key(table) {
                return Err(transaction_error("ORNA-EVAL-TABLE-ARGUMENT"));
            }
            let mut count = 0usize;
            for (_, row) in self
                .activation
                .candidate_relation(table)
                .map_err(|error| transaction_error(table_error_code(error)))?
            {
                debit_effect_step(&mut budget)?;
                if record_field(&row, field)
                    .is_some_and(|value| relation_values_equal(&value, expected))
                {
                    count = count
                        .checked_add(1)
                        .ok_or_else(|| transaction_error("ORNA-EVAL-LIMIT"))?;
                }
            }
            return Ok(Some(Value::int(BigInt::from(count))));
        }
        if matches!(
            callee,
            Expr::Field {
                base,
                name,
                ..
            } if matches!(base.as_ref(), Expr::ReplBinding { text, .. } if text == "$__orna_relation")
                && name == "filtered_one"
        ) {
            let [table, field, expected] = arguments else {
                return Err(transaction_error("ORNA-EVAL-TABLE-ARGUMENT"));
            };
            let (OvbRaw::Text(table), OvbRaw::Text(field)) = (table.raw(), field.raw()) else {
                return Err(transaction_error("ORNA-EVAL-TABLE-ARGUMENT"));
            };
            if !self.key_fields.contains_key(table) {
                return Err(transaction_error("ORNA-EVAL-TABLE-ARGUMENT"));
            }
            let mut found = None;
            for (_, row) in self
                .activation
                .candidate_relation(table)
                .map_err(|error| transaction_error(table_error_code(error)))?
            {
                debit_effect_step(&mut budget)?;
                if !record_field(&row, field)
                    .is_some_and(|value| relation_values_equal(&value, expected))
                {
                    continue;
                }
                if found.is_some() {
                    return Err(transaction_error("ORNA-EVAL-RELATION-ONE-MULTIPLE"));
                }
                found = Some(row);
            }
            return found
                .map(Some)
                .ok_or_else(|| transaction_error("ORNA-EVAL-RELATION-ONE-ZERO"));
        }
        let Expr::Field { base, name, .. } = callee else {
            return Ok(None);
        };
        let Expr::Name { text: table, .. } = base.as_ref() else {
            return Ok(None);
        };
        // Qualified module calls (for example `inventory.add()`) arrive at
        // the effect boundary before the retained evaluator resolves them.
        // Only declared table names are effects; every other qualified call
        // remains available to the admitted function namespace.
        let Some(key_fields) = self.key_fields.get(table) else {
            return Ok(None);
        };
        match name.as_str() {
            "count" => {
                if !arguments.is_empty() {
                    return Err(transaction_error("ORNA-EVAL-TABLE-ARGUMENT"));
                }
                let mut count = 0usize;
                for _ in self
                    .activation
                    .candidate_relation(table)
                    .map_err(|error| transaction_error(table_error_code(error)))?
                {
                    debit_effect_step(&mut budget)?;
                    count = count
                        .checked_add(1)
                        .ok_or_else(|| transaction_error("ORNA-EVAL-LIMIT"))?;
                }
                Ok(Some(Value::int(BigInt::from(count))))
            }
            "first" => {
                if !arguments.is_empty() {
                    return Err(transaction_error("ORNA-EVAL-TABLE-ARGUMENT"));
                }
                let first = self
                    .activation
                    .candidate_relation(table)
                    .map_err(|error| transaction_error(table_error_code(error)))?
                    .next()
                    .map(|(_, row)| debit_effect_step(&mut budget).map(|()| row))
                    .transpose()?;
                let mut option = vec![OvbRaw::Int(BigInt::from(u8::from(first.is_some())))];
                if let Some(row) = first {
                    option.push(row.raw().clone());
                }
                Value::new(OvbRaw::Tag(60013, Box::new(OvbRaw::Array(option))))
                    .map(Some)
                    .map_err(|_| transaction_error("ORNA-EVAL-TABLE-ARGUMENT"))
            }
            "lookup" => {
                if arguments.len() != key_fields.len() {
                    return Err(transaction_error("ORNA-EVAL-TABLE-ARGUMENT"));
                }
                let key = key_fields
                    .key_from_values(arguments)
                    .map_err(cursor_error)?;
                self.activation
                    .read(table, &key)
                    .map_err(|error| transaction_error(table_error_code(error)))?
                    .cloned()
                    .map(Some)
                    .ok_or_else(|| transaction_error("ORNA-EVAL-TABLE-MISSING"))
            }
            "insert" => {
                let [row] = arguments else {
                    return Err(transaction_error("ORNA-EVAL-TABLE-ARGUMENT"));
                };
                let key = key_fields.key_from_row(row).map_err(cursor_error)?;
                self.activation
                    .insert(table.clone(), key.clone(), row.clone())
                    .map_err(|error| transaction_error(table_error_code(error)))?;
                self.record(table, key, Some(row.clone()), true)?;
                Ok(Some(row.clone()))
            }
            "upsert" => {
                let [row] = arguments else {
                    return Err(transaction_error("ORNA-EVAL-TABLE-ARGUMENT"));
                };
                let key = key_fields.key_from_row(row).map_err(cursor_error)?;
                if let Some(existing) = self
                    .activation
                    .read(table, &key)
                    .map_err(|error| transaction_error(table_error_code(error)))?
                    .cloned()
                {
                    let row = merge_upsert_row(&existing, row, key_fields)?;
                    let key = key.clone();
                    self.activation
                        .update(table.clone(), key.clone(), row.clone())
                        .map_err(|error| transaction_error(table_error_code(error)))?;
                    self.record(table, key, Some(row.clone()), false)?;
                    Ok(Some(row))
                } else {
                    self.activation
                        .insert(table.clone(), key.clone(), row.clone())
                        .map_err(|error| transaction_error(table_error_code(error)))?;
                    self.record(table, key, Some(row.clone()), true)?;
                    Ok(Some(row.clone()))
                }
            }
            "update" => {
                let [key, patch] = arguments else {
                    return Err(transaction_error("ORNA-EVAL-TABLE-ARGUMENT"));
                };
                let key = key_fields
                    .key_from_encoded(&encoded_key(key).map_err(cursor_error)?)
                    .map_err(cursor_error)?;
                let existing = self
                    .activation
                    .read(table, &key)
                    .map_err(|error| transaction_error(table_error_code(error)))?
                    .cloned()
                    .ok_or_else(|| transaction_error("ORNA-EVAL-TABLE-MISSING"))?;
                let row = merge_row(&existing, patch, key_fields)?;
                self.activation
                    .update(table.clone(), key.clone(), row.clone())
                    .map_err(|error| transaction_error(table_error_code(error)))?;
                self.record(table, key, Some(row.clone()), false)?;
                Ok(Some(row))
            }
            "delete" => {
                let [key] = arguments else {
                    return Err(transaction_error("ORNA-EVAL-TABLE-ARGUMENT"));
                };
                let key = key_fields
                    .key_from_encoded(&encoded_key(key).map_err(cursor_error)?)
                    .map_err(cursor_error)?;
                self.activation
                    .delete(table.clone(), key.clone())
                    .map_err(|error| transaction_error(table_error_code(error)))?;
                self.record(table, key, None, false)?;
                Ok(Some(Value::unit()))
            }
            "rekey" => {
                let [old_key, new_key] = arguments else {
                    return Err(transaction_error("ORNA-EVAL-TABLE-ARGUMENT"));
                };
                let old_key = key_fields
                    .key_from_encoded(&encoded_key(old_key).map_err(cursor_error)?)
                    .map_err(cursor_error)?;
                let new_key = key_fields
                    .key_from_encoded(&encoded_key(new_key).map_err(cursor_error)?)
                    .map_err(cursor_error)?;
                let existing = self
                    .activation
                    .read(table, &old_key)
                    .map_err(|error| transaction_error(table_error_code(error)))?
                    .cloned()
                    .ok_or_else(|| transaction_error("ORNA-EVAL-TABLE-MISSING"))?;
                if old_key == new_key {
                    return Err(transaction_error("ORNA-EVAL-TABLE-DUPLICATE"));
                }
                let new_components = key_fields
                    .values_from_encoded(new_key.as_ref())
                    .map_err(cursor_error)?;
                if key_fields.is_empty() {
                    return Err(transaction_error("ORNA-EVAL-TABLE-KEY"));
                }
                let row = replace_record_fields(&existing, key_fields, &new_components)?;
                self.activation
                    .delete(table.clone(), old_key.clone())
                    .map_err(|error| transaction_error(table_error_code(error)))?;
                self.record(table, old_key, None, false)?;
                self.activation
                    .insert(table.clone(), new_key.clone(), row.clone())
                    .map_err(|error| transaction_error(table_error_code(error)))?;
                self.record(table, new_key, Some(row.clone()), false)?;
                Ok(Some(row))
            }
            _ => Ok(None),
        }
    }

    pub(super) fn record(
        &mut self,
        table: &str,
        key: TransactionKey,
        value: Option<Value>,
        insert_only: bool,
    ) -> Result<(), EvaluationError> {
        let key = key.into_encoded();
        let encoded = value
            .as_ref()
            .map(Value::encode)
            .transpose()
            .map_err(|_| transaction_error("ORNA-EVAL-TABLE-ROW"))?;
        let ordinal = self.next_mutation;
        self.next_mutation = self.next_mutation.saturating_add(1);
        let mut digest = Sha256::new();
        digest.update(b"ORNA-SOURCE-MUTATION\0");
        digest.update(self.mutation_scope);
        digest.update(table.as_bytes());
        digest.update([0]);
        digest.update(&key);
        digest.update(ordinal.to_be_bytes());
        if let Some(bytes) = &encoded {
            digest.update([1]);
            digest.update(bytes);
        } else {
            digest.update([0]);
        }
        let id: [u8; 16] = digest.finalize()[..16]
            .try_into()
            .expect("truncated digest has fixed length");
        // An insert is recorded as insert-only so the runtime rejects an
        // occupied key; a replacement is recorded as a generic write, because
        // the runtime refuses an insert over an existing row (bead ornadb-jm0d).
        let mutation = match (insert_only, encoded) {
            (true, Some(value)) => TableMutation::insert(id, table, key, value),
            (_, value) => TableMutation::new(id, table, key, value),
        }
        .map_err(|_| transaction_error("ORNA-EVAL-TABLE-ROW"))?;
        self.mutations.push(mutation);
        Ok(())
    }
}
