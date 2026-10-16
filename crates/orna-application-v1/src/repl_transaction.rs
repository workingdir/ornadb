use super::*;
use orna_project_v1::{admit_source_row, ProjectLoader};
use orna_repository_v1::{OrpBlobBinding, Repository, RepositoryFormatContext};
use orna_semantic_v1::Type;
use orna_syntax_v1::Expr;

/// Borrowed repository and runtime authority for one trusted local REPL input.
pub struct ReplTransactionScope<'a> {
    pub project: &'a LoadedProject,
    pub repository: &'a Repository,
    pub format_context: &'a RepositoryFormatContext,
    pub runtime_context: &'a RuntimeActivationContext,
    pub request: RequestIdentity,
}

impl ApplicationAuthority {
    /// Evaluates one effectful REPL submission into an unpublished candidate.
    /// The caller must commit the returned activation before publishing the
    /// returned session successor.
    pub fn evaluate_staged_repl_transaction(
        &self,
        session: &AdmittedReplSession,
        staged: StagedReplActivation,
        scope: &ReplTransactionScope<'_>,
        bindings: &mut SysHostBindingRegistry,
    ) -> Result<(Option<CanonicalValue>, AdmittedReplSession, StagedActivation), ApplicationError> {
        let (tables, table_ids, defaults, functions) = admitted_project_tables(self, scope)?;
        let mutations = SourceMutationEffectHandler::new_with_table_object_ids(tables, table_ids)?
            .with_insert_defaults(defaults, functions, self.limits);
        let mut effects = ReplTransactionEffectHandler {
            bindings,
            mutations,
            scope,
            next_row_ordinal: 0,
        };
        let (value, successor) = session
            .evaluate_staged_with_effects(staged, &mut effects)
            .map_err(|error| ApplicationError::EffectRejected(error.code().to_owned()))?;
        let activation = effects
            .mutations
            .into_staged_activation(value.clone().unwrap_or_else(CanonicalValue::unit), None)?;
        Ok((value, successor, activation))
    }
}

fn admitted_project_tables(
    authority: &ApplicationAuthority,
    scope: &ReplTransactionScope<'_>,
) -> Result<(
    BTreeMap<String, TableSchema>,
    BTreeMap<String, TableObjectId>,
    TableInsertDefaults,
    Functions,
), ApplicationError> {
    let project = scope.project;
    let analysis = analyze_with_catalogue(project.modules(), &authority.catalogue);
    if !analysis.is_ok() {
        return Err(ApplicationError::Semantic(
            analysis
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code().to_owned())
                .collect(),
        ));
    }

    let mut tables = BTreeMap::new();
    for module in analysis.modules.values() {
        for (name, schema) in admitted_table_schemas(module) {
            if tables.insert(name, schema).is_some() {
                return Err(ApplicationError::InvalidTableMetadata);
            }
        }
    }
    let table_ids = tables
        .keys()
        .map(|name| {
            scope
                .runtime_context
                .table_object_ids()
                .get(name)
                .copied()
                .map(|id| (name.clone(), id))
                .ok_or(ApplicationError::InvalidTableMetadata)
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;

    let mut defaults = BTreeMap::new();
    let mut functions = Functions::new();
    for module in project.modules() {
        let parsed = parse_module_with_file(&module.source, module.logical_path.clone());
        if !parsed.is_ok() {
            return Err(ApplicationError::InvalidTableMetadata);
        }
        defaults.extend(table_insert_defaults(&parsed.value.items));
        for item in &parsed.value.items {
            if let Declaration::Function { signature, body } = &item.declaration {
                functions.insert(
                    signature.name.clone(),
                    PureFunction {
                        parameters: signature.parameters.clone(),
                        body: body.clone(),
                        environment: Environment::new(),
                    },
                );
            }
        }
    }
    Ok((tables, table_ids, defaults, functions))
}

struct ReplTransactionEffectHandler<'a, 'scope> {
    bindings: &'a mut SysHostBindingRegistry,
    mutations: SourceMutationEffectHandler,
    scope: &'a ReplTransactionScope<'scope>,
    next_row_ordinal: u64,
}

impl ReplTransactionEffectHandler<'_, '_> {
    fn handle_table_effect(
        &mut self,
        callee: &Expr,
        arguments: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        let Expr::Field { base, name, .. } = callee else {
            return self.mutations.handle(callee, arguments);
        };
        let Some(path) = expression_name_path(base) else {
            return self.mutations.handle(callee, arguments);
        };
        let Some(table) = self.mutations.admitted_table_name(&path).map(str::to_owned) else {
            return self.mutations.handle(callee, arguments);
        };
        let schema = self.mutations.table(&table)?.clone();
        match name.as_str() {
            "insert" => {
                let [input_row] = arguments else {
                    return self.mutations.handle(callee, arguments);
                };
                if !contains_blob_value(&schema, input_row) {
                    return self.mutations.handle(callee, arguments);
                }
                let row = self.mutations.complete_insert_row(&table, input_row, false)?;
                let object_id = self
                    .scope
                    .runtime_context
                    .table_object_ids()
                    .get(&table)
                    .copied()
                    .ok_or_else(|| SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-UNADMITTED"))?;
                let key = self.mutations.key_from_row(&schema, &row)?;
                let mutation_id = self.mutation_id();
                let encoded_row = row
                    .encode()
                    .map_err(|_| SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW"))?;
                let provisional = TableMutation::new(
                    mutation_id,
                    table.clone(),
                    key.clone(),
                    Some(encoded_row),
                )
                .map_err(|_| SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW"))?
                .with_table_object_id(object_id);
                let commit = self
                    .scope
                    .project
                    .source_commit()
                    .ok_or_else(|| SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW"))?;
                let projection = ProjectLoader::default()
                    .resolve_committed_loose_row_projection(
                        self.scope.repository,
                        commit,
                        &provisional,
                    )
                    .map_err(|_| SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW"))?;
                let value_context = self
                    .scope
                    .format_context
                    .open_value_context(object_id.bytes())
                    .map_err(|_| SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW"))?;
                let (row_source, blob_bindings) = self.row_source(&schema, &row)?;
                let admitted = admit_source_row(
                    self.scope.project,
                    projection.logical_path(),
                    &row_source,
                    projection.schema(),
                    &value_context,
                    blob_bindings,
                    mutation_id,
                )
                .map_err(|_| SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW"))?;
                let mutation_index = self.mutations.mutations.len();
                self.mutations
                    .record_admitted_source_row(admitted, &value_context)
                    .map_err(|_| SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW"))?;
                let recorded = self.mutations.mutations[mutation_index].clone();
                let mut insert = TableMutation::insert(
                    recorded.id(),
                    recorded.table().to_owned(),
                    recorded.key().to_vec(),
                    recorded
                        .value()
                        .ok_or_else(|| SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW"))?
                        .to_vec(),
                )
                .map_err(|_| SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW"))?
                .with_table_object_id(object_id);
                for pin in self
                    .mutations
                    .protected_field_pins
                    .iter()
                    .filter(|pin| pin.mutation_id() == recorded.id())
                {
                    insert = insert
                        .with_protected_content_pin(pin.pin())
                        .map_err(|_| SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW"))?;
                }
                self.mutations.mutations[mutation_index] = insert;
                self.mutations
                    .overlay
                    .entry(table)
                    .or_default()
                    .insert(key, Some(row.clone()));
                self.next_row_ordinal = self.next_row_ordinal.saturating_add(1);
                Ok(Some(row))
            }
            "upsert" | "update" => {
                if arguments.iter().any(|argument| contains_blob_value(&schema, argument)) {
                    return Err(SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-SNAPSHOT"));
                }
                self.mutations.handle(callee, arguments)
            }
            _ => self.mutations.handle(callee, arguments),
        }
    }

    fn mutation_id(&self) -> [u8; 16] {
        let mut digest = Sha256::new();
        digest.update(b"ORNA-REPL-SOURCE-ROW\0");
        digest.update(self.scope.runtime_context.capture().generation_digest());
        digest.update(self.scope.request.session_id);
        digest.update(self.scope.request.request_id);
        digest.update(self.next_row_ordinal.to_be_bytes());
        digest.finalize()[..16]
            .try_into()
            .expect("SHA-256 prefix has the mutation-id length")
    }

    fn row_source(
        &self,
        schema: &TableSchema,
        row: &CanonicalValue,
    ) -> Result<(String, Vec<(String, OrpBlobBinding)>), EvaluationError> {
        let OvbRaw::Map(values) = row.raw() else {
            return Err(SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW"));
        };
        let values = values
            .iter()
            .filter_map(|(key, value)| match key {
                OvbRaw::Text(name) => Some((name.as_str(), value)),
                _ => None,
            })
            .collect::<BTreeMap<_, _>>();
        let admission = schema
            .admission
            .as_ref()
            .ok_or_else(|| SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-UNADMITTED"))?;
        let key_fields = admission.keys.iter().map(|(name, _)| name.as_str()).collect::<BTreeSet<_>>();
        let mut fields = Vec::new();
        let mut bindings = Vec::new();
        for (name, ty) in &schema.fields {
            if key_fields.contains(name.as_str()) || admission.computed.contains(name) {
                continue;
            }
            let raw = values
                .get(name.as_str())
                .ok_or_else(|| SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW"))?;
            if let Some(blob) = blob_value(ty, raw) {
                match blob {
                    Some(blob) => {
                        let value_context = ContextValue::new(ValueFormat::Ovb2, blob.clone())
                            .map_err(|_| SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW"))?;
                        let binding = self
                            .bindings
                            .accept_captured_blob_for_row(&value_context)?;
                        bindings.push((name.clone(), binding));
                    }
                    None => fields.push(format!("{name}: null")),
                }
            } else {
                fields.push(format!(
                    "{name}: {}",
                    source_literal(ty, raw)?
                ));
            }
        }
        let source = if fields.is_empty() {
            "{}\n".to_owned()
        } else {
            format!("{{ {}, }}\n", fields.join(", "))
        };
        Ok((source, bindings))
    }
}

impl EffectHandler for ReplTransactionEffectHandler<'_, '_> {
    fn handle(
        &mut self,
        callee: &Expr,
        arguments: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        self.handle_table_effect(callee, arguments)
    }

    fn handle_with_budget(
        &mut self,
        callee: &Expr,
        arguments: &[CanonicalValue],
        budget: &mut StepBudget,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        self.mutations.handle_with_budget(callee, arguments, budget)
    }

    fn handle_registered_with_budget(
        &mut self,
        operation: &str,
        callee: &Expr,
        arguments: &[CanonicalValue],
        budget: &mut StepBudget,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        if let Some(value) = self
            .bindings
            .handle_registered_with_budget(operation, callee, arguments, budget)?
        {
            return Ok(Some(value));
        }
        self.handle_table_effect(callee, arguments)
    }

    fn handle_registered_with_cancellation_and_budget(
        &mut self,
        operation: &str,
        callee: &Expr,
        arguments: &[CanonicalValue],
        budget: &mut StepBudget,
        cancellation: Option<&orna_evaluator_v1::CancellationToken>,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        if let Some(value) = self
            .bindings
            .handle_registered_with_cancellation_and_budget(
                operation,
                callee,
                arguments,
                budget,
                cancellation,
            )?
        {
            return Ok(Some(value));
        }
        self.handle_table_effect(callee, arguments)
    }
}

fn contains_blob_value(schema: &TableSchema, row: &CanonicalValue) -> bool {
    let OvbRaw::Map(values) = row.raw() else {
        return false;
    };
    schema.fields.iter().any(|(name, ty)| {
        values.iter().any(|(key, value)| {
            matches!(key, OvbRaw::Text(field) if field == name)
                && blob_value(ty, value).is_some_and(|value| {
                    value.is_some_and(|value| matches!(value, OvbRaw::Tag(60_110 | 60_111 | 60_112, _)))
                })
        })
    })
}

fn blob_value<'a>(ty: &Type, value: &'a OvbRaw) -> Option<Option<&'a OvbRaw>> {
    let is_blob = |ty: &Type| matches!(ty, Type::Named(name) if name == "Blob");
    if let Type::Optional(inner) = ty {
        if !is_blob(inner) {
            return None;
        }
        return Some(match value {
            OvbRaw::Null => None,
            OvbRaw::Tag(60_013, encoded) => match encoded.as_ref() {
                OvbRaw::Array(parts) if matches!(parts.as_slice(), [OvbRaw::Int(value)] if value == &BigInt::from(0)) => None,
                OvbRaw::Array(parts) if matches!(parts.as_slice(), [OvbRaw::Int(value), _] if value == &BigInt::from(1)) => parts.get(1),
                _ => Some(value),
            },
            _ => Some(value),
        });
    }
    is_blob(ty).then_some(Some(value))
}

fn source_literal(ty: &Type, value: &OvbRaw) -> Result<String, EvaluationError> {
    if let Type::Optional(inner) = ty {
        return match value {
            OvbRaw::Null => Ok("null".into()),
            OvbRaw::Tag(60_013, encoded) => match encoded.as_ref() {
                OvbRaw::Array(parts) if matches!(parts.as_slice(), [OvbRaw::Int(value)] if value == &BigInt::from(0)) => Ok("null".into()),
                OvbRaw::Array(parts) if matches!(parts.as_slice(), [OvbRaw::Int(value), _] if value == &BigInt::from(1)) => source_literal(inner, &parts[1]),
                _ => Err(SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW")),
            },
            _ => source_literal(inner, value),
        };
    }
    match value {
        OvbRaw::Null => Ok("null".into()),
        OvbRaw::Bool(value) => Ok(value.to_string()),
        OvbRaw::Int(value) => Ok(value.to_string()),
        OvbRaw::Float(bits) => {
            let value = f64::from_bits(*bits);
            value.is_finite().then(|| format!("{value:?}")).ok_or_else(|| {
                SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW")
            })
        }
        OvbRaw::Text(value) => Ok(format!("{value:?}")),
        OvbRaw::Array(values) => {
            let element_type = match ty {
                Type::List(inner) => Some(inner.as_ref()),
                Type::Tuple(elements) if elements.len() == values.len() => None,
                _ => None,
            };
            let mut rendered = Vec::with_capacity(values.len());
            for (index, value) in values.iter().enumerate() {
                let item_type = element_type.or_else(|| match ty {
                    Type::Tuple(elements) => elements.get(index),
                    _ => None,
                });
                rendered.push(source_literal(item_type.unwrap_or(&Type::Error), value)?);
            }
            Ok(format!("[{}]", rendered.join(", ")))
        }
        OvbRaw::Map(values) => {
            let mut rendered = Vec::with_capacity(values.len());
            for (key, value) in values {
                let OvbRaw::Text(name) = key else {
                    return Err(SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW"));
                };
                let field_type = match ty {
                    Type::Record(fields) => fields.get(name),
                    _ => None,
                };
                rendered.push(format!(
                    "{name}: {}",
                    source_literal(field_type.unwrap_or(&Type::Error), value)?
                ));
            }
            Ok(format!("{{ {}, }}", rendered.join(", ")))
        }
        _ => Err(SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW")),
    }
}
