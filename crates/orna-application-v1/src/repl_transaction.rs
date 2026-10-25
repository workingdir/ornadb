use super::*;
use orna_project_v1::{
    admit_format3_row, blob_placeholder, committed_tables, evaluator_rows, has_text_primary_key,
    restore_stored_blobs, runtime_key_from_evaluator, storage_row_from_record, CommittedTable,
    ProjectLoader,
};
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

/// Failure from one evaluated REPL submission. `external_effects_possible`
/// is true once any external sys binding was reached, so a failure after
/// that point is not known to have left external state unchanged.
pub struct ReplEvaluationFailure {
    pub error: ApplicationError,
    pub external_effects_possible: bool,
}

impl From<ApplicationError> for ReplEvaluationFailure {
    fn from(error: ApplicationError) -> Self {
        Self {
            error,
            external_effects_possible: false,
        }
    }
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
    ) -> Result<(Option<CanonicalValue>, AdmittedReplSession, StagedActivation), ReplEvaluationFailure> {
        let (tables, table_ids, defaults, functions) = admitted_project_tables(self, scope)?;
        // The REPL host serves no publication relation, so its explain
        // catalogue pins the activation's project tables only.
        let explain_catalogue = orna_evaluator_v1::ActivationExplainCatalogue::new(
            scope.runtime_context.capture().snapshot(),
            tables.keys().map(String::as_str),
        )
        .map_err(|_| {
            ApplicationError::Runtime("activation snapshot has no sys.Snapshot key".into())
        })?;
        // The effect handler consumes full row maps, so project the source-bound committed store into that form.
        let (captured_rows, committed) = committed_table_rows(scope, &tables, &table_ids)?;
        let tables_for_storage = tables.clone();
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
            explain_catalogue,
            external_dispatched: false,
        };
        let outcome = session.evaluate_staged_with_effects(staged, &mut effects);
        let external_effects_possible = effects.external_dispatched;
        let failure = move |error: ApplicationError| ReplEvaluationFailure {
            error,
            external_effects_possible,
        };
        let (value, successor) = outcome
            .map_err(|error| failure(ApplicationError::EffectRejected(error.code().to_owned())))?;
        // A bound registry always reports external reach (see `may_reach_external_state`), so this declaration is currently always `ExternalPossible`. Persistence to the ledger is not wired (ornadb-rg5g).
        let declared = repl_declared_effects(effects.bindings);
        let activation = effects
            .mutations
            .into_staged_activation(
                value.clone().unwrap_or_else(CanonicalValue::unit),
                None,
                declared,
            )
            .map_err(|error| failure(error.into()))?;
        let activation = normalize_repl_mutations(&tables_for_storage, &committed, activation)
            .map_err(|error| failure(error.into()))?;
        Ok((value, successor, activation))
    }
}

/// Projects the activation's evaluator-form mutations onto stored tuples in
/// the runtime keyspace. A written row's Blob references are restored from
/// the stored tuple it was read from: the row an earlier mutation of this
/// activation left at its key, or else its committed row.
fn normalize_repl_mutations(
    tables: &BTreeMap<String, orna_semantic_v1::TableSchema>,
    committed: &BTreeMap<String, CommittedTable>,
    activation: StagedActivation,
) -> Result<StagedActivation, ApplicationError> {
    let (
        value,
        mutations,
        snapshot_generation,
        source_row_schemas,
        protected_field_pins,
        effects,
    ) = activation.into_parts();
    // The stored tuple this activation has left at each key so far; `None`
    // marks a removed row, which then shadows the committed one.
    let mut written = BTreeMap::<(String, Vec<u8>), Option<Vec<u8>>>::new();
    let mut normalized = Vec::with_capacity(mutations.len());
    for mutation in mutations {
        let table = mutation.table().to_owned();
        let table_schema = tables.get(&table).ok_or(ApplicationError::InvalidTableMetadata)?;
        let committed_table = committed
            .get(&table)
            .ok_or(ApplicationError::InvalidTableMetadata)?;
        // A row admitted through `admit_format3_row` (the Blob insert path)
        // already carries its runtime key and stored-field tuple, and its
        // source-row provenance says so. Every other mutation, a plain insert
        // included, still holds the evaluator's row map and is projected to
        // the stored-field tuple here.
        if source_row_schemas
            .iter()
            .any(|source_row| source_row.mutation_id == mutation.id())
        {
            written.insert(
                (table, mutation.key().to_vec()),
                mutation.value().map(<[u8]>::to_vec),
            );
            normalized.push(mutation);
            continue;
        }
        let text_primary_key = has_text_primary_key(table_schema);
        let key = runtime_key_from_evaluator(mutation.key(), text_primary_key)
            .map_err(|_| ApplicationError::InvalidTableMetadata)?;
        let rekey_to = mutation.rekey_to()
            .map(|key| runtime_key_from_evaluator(key, text_primary_key))
            .transpose()
            .map_err(|_| ApplicationError::InvalidTableMetadata)?;
        let mutation = mutation
            .with_reencoded_keys(key, rekey_to)
            .map_err(|_| ApplicationError::InvalidTableMetadata)?;
        let source_key = (table.clone(), mutation.key().to_vec());
        let source = match written.get(&source_key) {
            Some(row) => row.clone(),
            None => committed_table
                .stored_row(mutation.key())
                .map(<[u8]>::to_vec),
        };
        let Some(row_value) = mutation.value().map(<[u8]>::to_vec) else {
            written.insert(source_key, None);
            normalized.push(mutation);
            continue;
        };
        let storage_order = &committed_table.storage_order;
        let encoded = storage_row_from_record(&row_value, storage_order)
            .and_then(|stored| {
                restore_stored_blobs(&stored, storage_order, table_schema, source.as_deref())
            })
            .map_err(|_| ApplicationError::InvalidTableMetadata)?;
        // A rekey moves the stored tuple: its old key is freed for later
        // mutations and its new key carries the restored references.
        if let Some(new_key) = mutation.rekey_to() {
            written.insert((table, new_key.to_vec()), Some(encoded.clone()));
            written.insert(source_key, None);
        } else {
            written.insert(source_key, Some(encoded.clone()));
        }
        normalized.push(mutation.with_reencoded_value(encoded).map_err(|_| ApplicationError::InvalidTableMetadata)?);
    }
    Ok(StagedActivation {
        value,
        mutations: normalized,
        snapshot_generation,
        source_row_schemas,
        protected_field_pins,
        effects,
    })
}

/// Every admitted table at the REPL's source commit: the evaluator rows the
/// effect handler consumes, and the committed stored tuples whose Blob
/// references `normalize_repl_mutations` restores.
fn committed_table_rows(
    scope: &ReplTransactionScope<'_>,
    tables: &BTreeMap<String, orna_semantic_v1::TableSchema>,
    table_ids: &BTreeMap<String, TableObjectId>,
) -> Result<(RuntimeTableRows, BTreeMap<String, CommittedTable>), ApplicationError> {
    let mut rows = BTreeMap::new();
    if tables.is_empty() {
        return Ok((rows, BTreeMap::new()));
    }
    let commit = scope.project.source_commit().ok_or(ApplicationError::InvalidTableMetadata)?;
    let committed = committed_tables(scope.repository, scope.format_context, commit, tables, table_ids)
        .map_err(|_| ApplicationError::InvalidTableMetadata)?;
    for (table_name, table_schema) in tables {
        let table = committed.get(table_name).ok_or(ApplicationError::InvalidTableMetadata)?;
        let table_rows = evaluator_rows(table, table_schema).map_err(|_| ApplicationError::InvalidTableMetadata)?;
        rows.insert(table_name.clone(), table_rows);
    }
    Ok((rows, committed))
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
    let analysis = analyze_with_providers(project.modules(), &authority.catalogue, SystemProviders::Production);
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
    explain_catalogue: orna_evaluator_v1::ActivationExplainCatalogue,
    external_dispatched: bool,
}

impl ReplTransactionEffectHandler<'_, '_> {
    /// Records that a bound sys binding was reached. A call returning
    /// `Ok(None)` declined the operation; any other outcome, including a
    /// failure, may already have changed external state.
    fn record_dispatch<T>(&mut self, result: &Result<Option<T>, EvaluationError>) {
        if !matches!(result, Ok(None)) {
            self.external_dispatched = true;
        }
    }

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
        &mut self,
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
                                self.external_dispatched = true;
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
    fn sys_host_context(&self) -> Option<&orna_evaluator_v1::SysHostAttachment> {
        EffectHandler::sys_host_context(&*self.bindings)
    }

    fn handle(
        &mut self,
        callee: &Expr,
        arguments: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        let result = EffectHandler::handle(self.bindings, callee, arguments);
        self.record_dispatch(&result);
        if let Some(value) = result? {
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
        let result = EffectHandler::handle_with_budget(
            self.bindings,
            callee,
            arguments,
            budget,
        );
        self.record_dispatch(&result);
        if let Some(value) = result? {
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
        let result = EffectHandler::handle_registered_with_budget(
            self.bindings,
            operation,
            callee,
            arguments,
            budget,
        );
        self.record_dispatch(&result);
        if let Some(value) = result? {
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
        let result = EffectHandler::handle_registered_with_cancellation_and_budget(
            self.bindings,
            operation,
            callee,
            arguments,
            budget,
            cancellation,
        );
        self.record_dispatch(&result);
        if let Some(value) = result? {
            return Ok(Some(value));
        }
        self.handle_table_effect(callee, arguments)
    }

    fn admitted_relation_source(&self, name: &str) -> Option<String> {
        EffectHandler::admitted_relation_source(&self.mutations, name)
    }

    fn scan_relation_page(
        &mut self,
        source: &str,
        after: Option<&[u8]>,
        limit: usize,
        budget: &mut StepBudget,
    ) -> Result<Option<RelationPage>, EvaluationError> {
        EffectHandler::scan_relation_page(&mut self.mutations, source, after, limit, budget)
    }

    fn is_ovb2_binding(&self) -> bool {
        true
    }

    fn ovb2_binding(&mut self) -> Option<&mut dyn Ovb2EffectHandler> {
        Some(self)
    }

    fn explain_catalogue(&self) -> Option<&dyn orna_evaluator_v1::ExplainCatalogue> {
        Some(&self.explain_catalogue)
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
            self.external_dispatched = true;
            let binding = self.bindings.accept_captured_blob_for_row(&value)
                .map_err(|_| SourceMutationEffectHandler::effect_error("ORNA-EVAL-VALUE"))?;
            blob_bindings.insert(field.clone(), binding);
            let Some((_, sanitized)) = sanitized_fields
                .iter_mut()
                .find(|(key, _)| matches!(key, OvbRaw::Text(name) if name == field))
            else {
                return Err(SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-ROW"));
            };
            *sanitized = blob_placeholder(ty);
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
            let result = Ovb2EffectHandler::handle(self.bindings, callee, arguments);
            self.record_dispatch(&result);
            return result;
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
            let result = Ovb2EffectHandler::handle_with_budget(
                self.bindings,
                callee,
                arguments,
                budget,
            );
            self.record_dispatch(&result);
            return result;
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
            let result = Ovb2EffectHandler::handle_registered_with_cancellation_and_budget(
                self.bindings,
                operation,
                callee,
                arguments,
                budget,
                cancellation,
            );
            self.record_dispatch(&result);
            return result;
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
    fn sys_host_context(&self) -> Option<&orna_evaluator_v1::SysHostAttachment> {
        Ovb2EffectHandler::sys_host_context(&*self.bindings)
    }

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

/// The effect set declared for one REPL transaction. Every REPL evaluation runs
/// under a bound registry. `may_reach_external_state` is currently constant `true`
/// (environment reads are always external), so this always yields `ExternalPossible`.
/// The declaration is truthful, but the REPL commit path does not yet persist it to
/// the request ledger (tracked by ornadb-rg5g); this helper only maps the registry
/// answer to the declared effect set.
pub(crate) fn repl_declared_effects(bindings: &SysHostBindingRegistry) -> ActivationEffectReport {
    crate::declared_effects(bindings.may_reach_external_state())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repl_declared_effects_maps_bound_registry_to_external_possible() {
        let bindings = SysHostBindingRegistry::default();
        assert!(matches!(
            repl_declared_effects(&bindings),
            ActivationEffectReport::ExternalPossible
        ));
    }
}

