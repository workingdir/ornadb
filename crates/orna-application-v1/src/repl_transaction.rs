use super::*;
use orna_project_v1::{admit_format3_row, ProjectLoader};
use orna_repository_v1::{KeyRange, OrpBlobBinding, Repository, RepositoryFormatContext};
use orna_semantic_v1::Type;
use orna_syntax_v1::Expr;
use orna_value_v1::decode_canonical_storage_value;

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
        // The effect handler consumes full row maps, so project the source-bound committed store into that form.
        let captured_rows = committed_table_rows(scope, &tables, &table_ids)?;
        let tables_for_storage = tables.clone();
        let ids_for_storage = table_ids.clone();
        let mutations = SourceMutationEffectHandler::with_table_rows_and_table_object_ids(
            tables,
            captured_rows,
            table_ids,
        )?
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
        let activation = normalize_repl_mutations(scope, &tables_for_storage, &ids_for_storage, activation)?;
        Ok((value, successor, activation))
    }
}

fn normalize_repl_mutations(
    scope: &ReplTransactionScope<'_>,
    tables: &BTreeMap<String, orna_semantic_v1::TableSchema>,
    table_ids: &BTreeMap<String, TableObjectId>,
    activation: StagedActivation,
) -> Result<StagedActivation, ApplicationError> {
    let (value, mutations, snapshot_generation, source_row_schemas, protected_field_pins) =
        activation.into_parts();
    let commit = scope.project.source_commit().ok_or(ApplicationError::InvalidTableMetadata)?;
    let mut field_names_by_table = BTreeMap::<String, Vec<String>>::new();
    let mut normalized = Vec::with_capacity(mutations.len());
    for mutation in mutations {
        let table = mutation.table().to_owned();
        let table_schema = tables.get(&table).ok_or(ApplicationError::InvalidTableMetadata)?;
        if mutation.is_insert() {
            normalized.push(mutation);
            continue;
        }
        let text_primary_key = table_schema.admission.as_ref().is_some_and(|admission| {
            matches!(admission.keys.as_slice(), [(_, Type::Text)])
        });
        let key = normalize_runtime_key(mutation.key(), text_primary_key)?;
        let rekey_to = mutation.rekey_to()
            .map(|key| normalize_runtime_key(key, text_primary_key))
            .transpose()?;
        let mutation = mutation
            .with_reencoded_keys(key, rekey_to)
            .map_err(|_| ApplicationError::InvalidTableMetadata)?;
        let Some(row_value) = mutation.value().map(<[u8]>::to_vec) else {
            normalized.push(mutation);
            continue;
        };
        let object_id = table_ids.get(&table).copied().ok_or(ApplicationError::InvalidTableMetadata)?;
        if !field_names_by_table.contains_key(&table) {
            let schema = ProjectLoader::default()
                .resolve_committed_source_table_schema(scope.repository, commit, object_id)
                .map_err(|_| ApplicationError::InvalidTableMetadata)?;
            let (_, field_names) = stored_field_names(schema.binding().descriptor().raw())?;
            field_names_by_table.insert(table.clone(), field_names);
        }
        let decoded = CanonicalValue::decode(&row_value).map_err(|_| ApplicationError::InvalidTableMetadata)?;
        let OvbRaw::Map(entries) = decoded.raw() else {
            return Err(ApplicationError::InvalidTableMetadata);
        };
        let stored_values = field_names_by_table[&table].iter().map(|name| {
            entries.iter().find_map(|(key, field_value)| match key {
                OvbRaw::Text(field_name) if field_name == name => Some(field_value.clone()),
                _ => None,
            }).ok_or(ApplicationError::InvalidTableMetadata)
        }).collect::<Result<Vec<_>, _>>()?;
        let encoded = CanonicalValue::new(OvbRaw::Array(stored_values))
            .and_then(|row| row.encode())
            .map_err(|_| ApplicationError::InvalidTableMetadata)?;
        normalized.push(mutation.with_reencoded_value(encoded).map_err(|_| ApplicationError::InvalidTableMetadata)?);
    }
    Ok(StagedActivation {
        value,
        mutations: normalized,
        snapshot_generation,
        source_row_schemas,
        protected_field_pins,
    })
}

fn normalize_runtime_key(key: &[u8], text_primary_key: bool) -> Result<Vec<u8>, ApplicationError> {
    if !text_primary_key {
        return Ok(key.to_vec());
    }
    let value = CanonicalValue::decode(key).map_err(|_| ApplicationError::InvalidTableMetadata)?;
    let OvbRaw::Text(text) = value.raw() else {
        return Err(ApplicationError::InvalidTableMetadata);
    };
    Ok(text.as_bytes().to_vec())
}

fn committed_table_rows(
    scope: &ReplTransactionScope<'_>,
    tables: &BTreeMap<String, orna_semantic_v1::TableSchema>,
    table_ids: &BTreeMap<String, TableObjectId>,
) -> Result<RuntimeTableRows, ApplicationError> {
    let mut result = BTreeMap::new();
    for (table_name, table_schema) in tables {
        let object_id = table_ids.get(table_name).copied().ok_or(ApplicationError::InvalidTableMetadata)?;
        let commit = scope.project.source_commit().ok_or(ApplicationError::InvalidTableMetadata)?;
        let resolved_schema = ProjectLoader::default()
            .resolve_committed_source_table_schema(scope.repository, commit, object_id)
            .map_err(|_| ApplicationError::InvalidTableMetadata)?;
        let binding = resolved_schema.binding();
        let row_map = scope.format_context.load_row_map(object_id.bytes())
            .map_err(|_| ApplicationError::InvalidTableMetadata)?;
        let graph = scope.format_context.open_native_graph(&row_map)
            .map_err(|_| ApplicationError::InvalidTableMetadata)?;
        let read_scope = graph.open_read_scope().map_err(|_| ApplicationError::InvalidTableMetadata)?;
        let (field_names, _) = stored_field_names(binding.descriptor().raw())?;
        let admission = table_schema.admission.as_ref().ok_or(ApplicationError::InvalidTableMetadata)?;
        let mut committed = BTreeMap::<Vec<u8>, Vec<u8>>::new();
        let mut lower = None;
        loop {
            let range = KeyRange::new(lower.clone(), None, 64).map_err(|_| ApplicationError::InvalidTableMetadata)?;
            let page = graph.range_rows(&range, &read_scope).map_err(|_| ApplicationError::InvalidTableMetadata)?;
            if page.is_empty() { break; }
            let next_lower = page.last().map(|row| row.key().clone());
            for row in page.into_iter().skip(usize::from(lower.is_some())) {
                let decoded = graph.decode_admitted_row_fields(&row, binding, &read_scope)
                    .map_err(|_| ApplicationError::InvalidTableMetadata)?;
                let OvbRaw::Array(stored_values) = decode_canonical_storage_value(decoded.canonical_fields())
                    .map_err(|_| ApplicationError::InvalidTableMetadata)? else {
                    return Err(ApplicationError::InvalidTableMetadata);
                };
                if stored_values.len() != decoded.fields().len() {
                    return Err(ApplicationError::InvalidTableMetadata);
                }
                let canonical_key = row.key().canonical_bytes().map_err(|_| ApplicationError::InvalidTableMetadata)?;
                let key_value = CanonicalValue::decode(&canonical_key).map_err(|_| ApplicationError::InvalidTableMetadata)?;
                let key_bytes = key_value.encode().map_err(|_| ApplicationError::InvalidTableMetadata)?;
                let key_values = match (admission.keys.len(), key_value.raw()) {
                    (1, _) => vec![key_value.raw().clone()],
                    (_, OvbRaw::Array(values)) if values.len() == admission.keys.len() => values.clone(),
                    _ => return Err(ApplicationError::InvalidTableMetadata),
                };
                let mut fields = Vec::with_capacity(admission.keys.len() + stored_values.len());
                for ((name, _), value) in admission.keys.iter().zip(key_values) {
                    fields.push((OvbRaw::Text(name.clone()), value));
                }
                for (field, value) in decoded.fields().iter().zip(stored_values) {
                    let name = field_names.get(field.field_id()).ok_or(ApplicationError::InvalidTableMetadata)?;
                    CanonicalValue::new(value.clone()).map_err(|_| ApplicationError::InvalidTableMetadata)?;
                    fields.push((OvbRaw::Text(name.clone()), value));
                }
                fields.sort_by(|(left, _), (right, _)| match (left, right) {
                    (OvbRaw::Text(left), OvbRaw::Text(right)) => canonical_text_key_order(left, right),
                    _ => std::cmp::Ordering::Equal,
                });
                let row_value = CanonicalValue::new(OvbRaw::Map(fields))
                    .and_then(|value| value.encode())
                    .map_err(|_| ApplicationError::InvalidTableMetadata)?;
                if committed.insert(key_bytes, row_value).is_some() {
                    return Err(ApplicationError::InvalidTableMetadata);
                }
            }
            if next_lower == lower { break; }
            lower = next_lower;
        }
        result.insert(table_name.clone(), committed.into_iter().collect());
    }
    Ok(result)
}

fn stored_field_names(descriptor: &OvbRaw) -> Result<(BTreeMap<[u8; 16], String>, Vec<String>), ApplicationError> {
    let OvbRaw::Map(entries) = descriptor else { return Err(ApplicationError::InvalidTableMetadata); };
    let fields = entries.iter().find_map(|(key, value)| (key == &OvbRaw::Int(BigInt::from(3))).then_some(value))
        .ok_or(ApplicationError::InvalidTableMetadata)?;
    let OvbRaw::Array(fields) = fields else { return Err(ApplicationError::InvalidTableMetadata); };
    let mut by_id = BTreeMap::new();
    let mut in_storage_order = Vec::new();
    for field in fields {
        let OvbRaw::Array(parts) = field else { return Err(ApplicationError::InvalidTableMetadata); };
        let Some(OvbRaw::Tag(37, id)) = parts.first() else { return Err(ApplicationError::InvalidTableMetadata); };
        let OvbRaw::Bytes(id) = id.as_ref() else { return Err(ApplicationError::InvalidTableMetadata); };
        let id: [u8; 16] = id.as_slice().try_into().map_err(|_| ApplicationError::InvalidTableMetadata)?;
        let Some(OvbRaw::Text(name)) = parts.get(1) else { return Err(ApplicationError::InvalidTableMetadata); };
        if matches!(parts.get(3), Some(OvbRaw::Int(role)) if role == &BigInt::from(1)) {
            if by_id.insert(id, name.clone()).is_some() {
                return Err(ApplicationError::InvalidTableMetadata);
            }
            in_storage_order.push(name.clone());
        }
    }
    Ok((by_id, in_storage_order))
}

fn canonical_text_key_order(left: &str, right: &str) -> std::cmp::Ordering {
    fn length_class(length: usize) -> u8 {
        match length {
            0..=23 => 0,
            24..=255 => 1,
            256..=65_535 => 2,
            65_536..=4_294_967_295 => 3,
            _ => 4,
        }
    }

    length_class(left.len())
        .cmp(&length_class(right.len()))
        .then_with(|| left.len().cmp(&right.len()))
        .then_with(|| left.as_bytes().cmp(right.as_bytes()))
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
        self.handle_table_effect_with_blob_bindings(callee, arguments, BTreeMap::new())
    }

    fn handle_table_effect_with_blob_bindings(
        &mut self,
        callee: &Expr,
        arguments: &[CanonicalValue],
        mut prebound_blob_bindings: BTreeMap<String, OrpBlobBinding>,
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
                if !contains_blob_value(&schema, input_row) && prebound_blob_bindings.is_empty() {
                    return self.mutations.handle(callee, arguments);
                }
                let row = self
                    .mutations
                    .complete_insert_row(&table, input_row, false)?;
                let object_id = self
                    .scope
                    .runtime_context
                    .table_object_ids()
                    .get(&table)
                    .copied()
                    .ok_or_else(|| {
                        SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-UNADMITTED")
                    })?;
                let key = self.mutations.key_from_row(&schema, &row)?;
                let mutation_id = self.mutation_id();
                let commit = self
                    .scope
                    .project
                    .source_commit()
                    .ok_or_else(|| SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW"))?;
                let committed_schema = ProjectLoader::default()
                    .resolve_committed_source_table_schema(
                        self.scope.repository,
                        commit,
                        object_id,
                    )
                    .map_err(|_| SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW"))?;
                let value_context = self
                    .scope
                    .format_context
                    .open_value_context(object_id.bytes())
                    .map_err(|_| SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW"))?;
                let (admission_row, blob_bindings) = self
                    .row_fields_for_admission(&schema, &row, &mut prebound_blob_bindings)?;
                let admitted = admit_format3_row(
                    self.scope.project,
                    &key,
                    &admission_row,
                    &committed_schema,
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

    fn row_fields_for_admission(
        &self,
        schema: &TableSchema,
        row: &CanonicalValue,
        prebound_blob_bindings: &mut BTreeMap<String, OrpBlobBinding>,
    ) -> Result<(OvbRaw, Vec<(String, OrpBlobBinding)>), EvaluationError> {
        let OvbRaw::Map(entries) = row.raw() else {
            return Err(SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW"));
        };
        let mut values = BTreeMap::new();
        for (key, value) in entries {
            let OvbRaw::Text(name) = key else {
                return Err(SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW"));
            };
            if values.insert(name.as_str(), value).is_some() {
                return Err(SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW"));
            }
        }
        let admission = schema
            .admission
            .as_ref()
            .ok_or_else(|| SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-UNADMITTED"))?;
        let key_fields = admission.keys.iter().map(|(name, _)| name.as_str()).collect::<BTreeSet<_>>();
        if values.keys().any(|name| !schema.fields.contains_key(*name)) {
            return Err(SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW"));
        }

        let mut stored_fields = Vec::with_capacity(values.len());
        let mut bindings = Vec::new();
        for (name, ty) in &schema.fields {
            if admission.computed.contains(name) {
                continue;
            }
            let raw = values
                .get(name.as_str())
                .ok_or_else(|| SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW"))?;
            if key_fields.contains(name.as_str()) {
                stored_fields.push((OvbRaw::Text(name.clone()), (**raw).clone()));
                continue;
            }
            if let Some(blob) = blob_value(ty, raw) {
                match blob {
                    Some(blob) => {
                        let binding = match prebound_blob_bindings.remove(name) {
                            Some(binding) => binding,
                            None => {
                                let value_context = ContextValue::new(ValueFormat::Ovb2, blob.clone())
                                    .map_err(|_| SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW"))?;
                                self.bindings.accept_captured_blob_for_row(&value_context)?
                            }
                        };
                        bindings.push((name.clone(), binding));
                    }
                    None => stored_fields.push((OvbRaw::Text(name.clone()), OvbRaw::Null)),
                }
            } else {
                stored_fields.push((OvbRaw::Text(name.clone()), (**raw).clone()));
            }
        }
        if !prebound_blob_bindings.is_empty() {
            return Err(SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW"));
        }
        Ok((OvbRaw::Map(stored_fields), bindings))
    }
}

impl EffectHandler for ReplTransactionEffectHandler<'_, '_> {
    fn handle(
        &mut self,
        callee: &Expr,
        arguments: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        if let Some(value) = EffectHandler::handle(self.bindings, callee, arguments)? {
            return Ok(Some(value));
        }
        self.handle_table_effect(callee, arguments)
    }

    fn handle_with_budget(
        &mut self,
        callee: &Expr,
        arguments: &[CanonicalValue],
        budget: &mut StepBudget,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        if let Some(value) = EffectHandler::handle_with_budget(
            self.bindings,
            callee,
            arguments,
            budget,
        )?
        {
            return Ok(Some(value));
        }
        self.handle_table_effect(callee, arguments)
    }

    fn handle_registered_with_budget(
        &mut self,
        operation: &str,
        callee: &Expr,
        arguments: &[CanonicalValue],
        budget: &mut StepBudget,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        if let Some(value) = EffectHandler::handle_registered_with_budget(
            self.bindings,
            operation,
            callee,
            arguments,
            budget,
        )?
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
        if let Some(value) = EffectHandler::handle_registered_with_cancellation_and_budget(
            self.bindings,
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

    fn is_ovb2_binding(&self) -> bool {
        true
    }

    fn ovb2_binding(&mut self) -> Option<&mut dyn Ovb2EffectHandler> {
        Some(self)
    }
}

impl ReplTransactionEffectHandler<'_, '_> {
    fn canonical_arguments(
        arguments: &[ContextValue],
    ) -> Result<Vec<CanonicalValue>, EvaluationError> {
        arguments
            .iter()
            .map(|value| {
                if value.format() != ValueFormat::Ovb2 || value.context().is_some() {
                    return Err(SourceMutationEffectHandler::effect_error("ORNA-EVAL-VALUE"));
                }
                CanonicalValue::new(value.raw().clone())
                    .map_err(|_| SourceMutationEffectHandler::effect_error("ORNA-EVAL-VALUE"))
            })
            .collect()
    }

    fn contextualize_canonical_result(
        value: Option<CanonicalValue>,
    ) -> Result<Option<ContextValue>, EvaluationError> {
        value
            .map(|value| {
                ContextValue::new(ValueFormat::Ovb2, value.raw().clone())
                    .map_err(|_| SourceMutationEffectHandler::effect_error("ORNA-EVAL-VALUE"))
            })
            .transpose()
    }

    fn handle_contextual_blob_insert(
        &mut self,
        callee: &Expr,
        arguments: &[ContextValue],
    ) -> Result<Option<ContextValue>, EvaluationError> {
        let Expr::Field { base, name, .. } = callee else {
            return Ok(None);
        };
        if name != "insert" {
            return Ok(None);
        }
        let Some(path) = expression_name_path(base) else {
            return Ok(None);
        };
        let Some(table) = self.mutations.admitted_table_name(&path).map(str::to_owned) else {
            return Ok(None);
        };
        let [input_row] = arguments else {
            return Ok(None);
        };
        let schema = self.mutations.table(&table)?.clone();
        if !contains_blob_raw(&schema, input_row.raw()) {
            return Ok(None);
        }
        let OvbRaw::Map(input_fields) = input_row.raw() else {
            return Err(SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW"));
        };
        let mut sanitized_row = input_row.raw().clone();
        let OvbRaw::Map(sanitized_fields) = &mut sanitized_row else {
            return Err(SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW"));
        };
        let mut blob_bindings = BTreeMap::new();
        for (field, ty) in &schema.fields {
            let Some((_, original)) = input_fields
                .iter()
                .find(|(key, _)| matches!(key, OvbRaw::Text(name) if name == field))
            else {
                continue;
            };
            let Some(Some(blob)) = blob_value(ty, original) else {
                continue;
            };
            if !matches!(
                blob,
                OvbRaw::Tag(
                    orna_value_v1::OVB2_BLOB_TAG
                        | orna_value_v1::ROV3_BLOB_TAG
                        | orna_value_v1::SOV3_BLOB_TAG,
                    _
                )
            ) {
                return Err(SourceMutationEffectHandler::effect_error("ORNA-EVAL-VALUE"));
            }
            let value = in_profile_of(input_row, blob.clone())?;
            let binding = self.bindings.accept_captured_blob_for_row(&value)
                .map_err(|_| SourceMutationEffectHandler::effect_error("ORNA-EVAL-VALUE"))?;
            blob_bindings.insert(field.clone(), binding);
            let Some((_, sanitized)) = sanitized_fields
                .iter_mut()
                .find(|(key, _)| matches!(key, OvbRaw::Text(name) if name == field))
            else {
                return Err(SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW"));
            };
            *sanitized = blob_placeholder(ty, original)?;
        }
        if blob_bindings.is_empty() {
            return Ok(None);
        }
        let canonical_row = CanonicalValue::new(sanitized_row)
            .map_err(|_| SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW"))?;
        let result = self.handle_table_effect_with_blob_bindings(
            callee,
            &[canonical_row],
            blob_bindings,
        )?;
        let Some(result) = result else {
            return Ok(None);
        };
        let raw = restore_contextual_blob_values(&schema, result.raw(), input_row.raw())?;
        in_profile_of(input_row, raw).map(Some)
    }

    fn handle_contextual_effect(
        &mut self,
        callee: &Expr,
        arguments: &[ContextValue],
    ) -> Result<Option<ContextValue>, EvaluationError> {
        if expression_name_path(callee).as_deref() == Some("sys.blob.capture_file") {
            return Ovb2EffectHandler::handle(self.bindings, callee, arguments);
        }
        if let Some(value) = self.handle_contextual_blob_insert(callee, arguments)? {
            return Ok(Some(value));
        }
        if arguments
            .iter()
            .any(|argument| contains_contextual_blob_value(argument.raw()))
        {
            return Ok(None);
        }
        let canonical_arguments = Self::canonical_arguments(arguments)?;
        let value = EffectHandler::handle(self, callee, &canonical_arguments)?;
        Self::contextualize_canonical_result(value)
    }

    fn handle_contextual_effect_with_budget(
        &mut self,
        callee: &Expr,
        arguments: &[ContextValue],
        budget: &mut StepBudget,
    ) -> Result<Option<ContextValue>, EvaluationError> {
        if expression_name_path(callee).as_deref() == Some("sys.blob.capture_file") {
            return Ovb2EffectHandler::handle_with_budget(
                self.bindings,
                callee,
                arguments,
                budget,
            );
        }
        if let Some(value) = self.handle_contextual_blob_insert(callee, arguments)? {
            return Ok(Some(value));
        }
        if arguments
            .iter()
            .any(|argument| contains_contextual_blob_value(argument.raw()))
        {
            return Ok(None);
        }
        let canonical_arguments = Self::canonical_arguments(arguments)?;
        let value = EffectHandler::handle_with_budget(
            self,
            callee,
            &canonical_arguments,
            budget,
        )?;
        Self::contextualize_canonical_result(value)
    }

    fn handle_contextual_registered_effect(
        &mut self,
        operation: &str,
        callee: &Expr,
        arguments: &[ContextValue],
        budget: &mut StepBudget,
        cancellation: Option<&orna_evaluator_v1::CancellationToken>,
    ) -> Result<Option<ContextValue>, EvaluationError> {
        if operation == "sys.blob.capture_file" {
            return Ovb2EffectHandler::handle_registered_with_cancellation_and_budget(
                self.bindings,
                operation,
                callee,
                arguments,
                budget,
                cancellation,
            );
        }

        if let Some(value) = self.handle_contextual_blob_insert(callee, arguments)? {
            return Ok(Some(value));
        }
        let canonical_arguments = Self::canonical_arguments(arguments)?;
        let value = EffectHandler::handle_registered_with_cancellation_and_budget(
            self,
            operation,
            callee,
            &canonical_arguments,
            budget,
            cancellation,
        )?;
        Self::contextualize_canonical_result(value)
    }
}

impl Ovb2EffectHandler for ReplTransactionEffectHandler<'_, '_> {
    fn handle(
        &mut self,
        callee: &Expr,
        arguments: &[ContextValue],
    ) -> Result<Option<ContextValue>, EvaluationError> {
        self.handle_contextual_effect(callee, arguments)
    }

    fn handle_with_budget(
        &mut self,
        callee: &Expr,
        arguments: &[ContextValue],
        budget: &mut StepBudget,
    ) -> Result<Option<ContextValue>, EvaluationError> {
        self.handle_contextual_effect_with_budget(callee, arguments, budget)
    }

    fn handle_registered_with_budget(
        &mut self,
        operation: &str,
        callee: &Expr,
        arguments: &[ContextValue],
        budget: &mut StepBudget,
    ) -> Result<Option<ContextValue>, EvaluationError> {
        self.handle_contextual_registered_effect(operation, callee, arguments, budget, None)
    }

    fn handle_registered_with_cancellation_and_budget(
        &mut self,
        operation: &str,
        callee: &Expr,
        arguments: &[ContextValue],
        budget: &mut StepBudget,
        cancellation: Option<&orna_evaluator_v1::CancellationToken>,
    ) -> Result<Option<ContextValue>, EvaluationError> {
        self.handle_contextual_registered_effect(
            operation,
            callee,
            arguments,
            budget,
            cancellation,
        )
    }

    /// A lazily captured Blob arrives as a ROV-3 value in its capture context.
    fn accepts_context_values(&self) -> bool {
        true
    }
}

/// Rebuilds `raw` in the format and context of `profile`, so a lazily captured
/// Blob keeps the context that can resolve it.
fn in_profile_of(profile: &ContextValue, raw: OvbRaw) -> Result<ContextValue, EvaluationError> {
    match profile.context() {
        Some(context) => ContextValue::new_in_context(context, profile.format(), raw),
        None => ContextValue::new(profile.format(), raw),
    }
    .map_err(|_| SourceMutationEffectHandler::effect_error("ORNA-EVAL-VALUE"))
}

fn contains_contextual_blob_value(value: &OvbRaw) -> bool {
    match value {
        OvbRaw::Tag(
            orna_value_v1::OVB2_BLOB_TAG
            | orna_value_v1::ROV3_BLOB_TAG
            | orna_value_v1::SOV3_BLOB_TAG,
            _,
        ) => true,
        OvbRaw::Tag(_, inner) => contains_contextual_blob_value(inner),
        OvbRaw::Array(values) => values.iter().any(contains_contextual_blob_value),
        OvbRaw::Map(values) => values.iter().any(|(key, value)| {
            contains_contextual_blob_value(key) || contains_contextual_blob_value(value)
        }),
        _ => false,
    }
}

fn contains_blob_value(schema: &TableSchema, row: &CanonicalValue) -> bool {
    contains_blob_raw(schema, row.raw())
}

fn contains_blob_raw(schema: &TableSchema, row: &OvbRaw) -> bool {
    let OvbRaw::Map(values) = row else {
        return false;
    };
    schema.fields.iter().any(|(name, ty)| {
        values.iter().any(|(key, value)| {
            matches!(key, OvbRaw::Text(field) if field == name)
                && blob_value(ty, value).is_some_and(|value| value.is_some())
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

fn blob_placeholder(ty: &Type, _original: &OvbRaw) -> Result<OvbRaw, EvaluationError> {
    let placeholder = OvbRaw::Bytes(Vec::new());
    if matches!(ty, Type::Optional(_)) {
        return Ok(OvbRaw::Tag(
            60_013,
            Box::new(OvbRaw::Array(vec![
                OvbRaw::Int(BigInt::from(1)),
                placeholder,
            ])),
        ));
    }
    Ok(placeholder)
}

fn restore_contextual_blob_values(
    schema: &TableSchema,
    output: &OvbRaw,
    input: &OvbRaw,
) -> Result<OvbRaw, EvaluationError> {
    let (OvbRaw::Map(output_fields), OvbRaw::Map(input_fields)) = (output, input) else {
        return Err(SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW"));
    };
    let mut restored = output_fields.clone();
    for (field, ty) in &schema.fields {
        let Some((_, original)) = input_fields
            .iter()
            .find(|(key, _)| matches!(key, OvbRaw::Text(name) if name == field))
        else {
            continue;
        };
        if !blob_value(ty, original).is_some_and(|value| value.is_some()) {
            continue;
        }
        let Some((_, value)) = restored
            .iter_mut()
            .find(|(key, _)| matches!(key, OvbRaw::Text(name) if name == field))
        else {
            return Err(SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW"));
        };
        *value = original.clone();
    }
    Ok(OvbRaw::Map(restored))
}

