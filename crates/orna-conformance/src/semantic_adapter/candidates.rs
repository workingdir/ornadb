use super::*;

pub(super) struct TransactionalTableCandidateValidator {
    pub(super) cwd_generation: Option<orna_repository::CwdGeneration>,
    pub(super) table_assertions: TableAssertions,
    pub(super) module_assertions: ModuleAssertions,
    pub(super) candidate_table_fields: TableFields,
    pub(super) candidate_key_fields: TableFields,
    pub(super) functions: Functions,
    pub(super) limits: EvaluatorLimits,
    pub(super) table_object_ids: Arc<BTreeMap<String, orna_runtime::TableObjectId>>,
    pub(super) tables: Vec<String>,
    /// Tables whose primary keys admission could not lower (bead ornadb-gq4g).
    /// A check that reads one of them is refused, never skipped.
    pub(super) unsupported: BTreeSet<String>,
    /// Owner and span of the module assertion the last `validate_changed` found
    /// false; the commit gate reads it to name the refusal (ORNA-A091-009).
    pub(super) refused_module_assertion: Option<String>,
}

/// Reports whether any committed module declares a table-level or module-level
/// assertion. Projects without assertions have nothing for the assertion
/// validator to check.
pub fn project_declares_assertions(loaded: &orna_project::LoadedProject) -> bool {
    loaded.modules().iter().any(|module| {
        parse_module(&module.source)
            .value
            .items
            .iter()
            .any(|item| match &item.declaration {
                Declaration::Assertion { .. } => true,
                Declaration::Table { members, .. } => members
                    .iter()
                    .any(|member| matches!(member, orna_syntax_ast::TableMember::Assertion { .. })),
                _ => false,
            })
    })
}

/// Builds a transaction validator from every committed project module. This
/// keeps mounted row saves on the same cross-module assertion and effect
/// admission path as project-backed transactions.
pub fn transactional_loaded_project_candidate_validator(
    loaded: &orna_project::LoadedProject,
    identities: &orna_project::ObjectIdentityManifest,
    cwd_generation: Option<orna_repository::CwdGeneration>,
    table_object_ids: Arc<BTreeMap<String, orna_runtime::TableObjectId>>,
    limits: EvaluatorLimits,
) -> Result<Box<dyn TableActivationCandidateValidator>, StageOutcome<Diagnostic>> {
    transactional_loaded_project_validator(
        loaded,
        identities,
        cwd_generation,
        table_object_ids,
        limits,
        admit_transaction_project,
    )
}

/// Builds the commit gate's validator (bead ornadb-gq4g). A table whose primary
/// key cannot be lowered is admitted without its key, and only the checks that
/// read such a table are refused.
pub fn transactional_loaded_project_commit_validator(
    loaded: &orna_project::LoadedProject,
    identities: &orna_project::ObjectIdentityManifest,
    table_object_ids: Arc<BTreeMap<String, orna_runtime::TableObjectId>>,
    limits: EvaluatorLimits,
) -> Result<Box<dyn TableActivationCandidateValidator>, StageOutcome<Diagnostic>> {
    transactional_loaded_project_validator(
        loaded,
        identities,
        None,
        table_object_ids,
        limits,
        admit_transaction_project_tolerant,
    )
}

pub(super) fn transactional_loaded_project_validator(
    loaded: &orna_project::LoadedProject,
    identities: &orna_project::ObjectIdentityManifest,
    cwd_generation: Option<orna_repository::CwdGeneration>,
    table_object_ids: Arc<BTreeMap<String, orna_runtime::TableObjectId>>,
    limits: EvaluatorLimits,
    admit: fn(
        &ProjectUnit,
        EvaluatorLimits,
        &str,
        &Catalogue,
    ) -> Result<AdmittedTransaction, AdmissionFailure>,
) -> Result<Box<dyn TableActivationCandidateValidator>, StageOutcome<Diagnostic>> {
    let mut modules = loaded
        .modules()
        .iter()
        .map(|module| SourceUnit {
            fixture_id: "vfs-mounted-row".into(),
            source_id: format!("mounted/{}", module.logical_path),
            parse_as: "module_unit".into(),
            source: module.source.clone(),
        })
        .collect::<Vec<_>>();
    modules.extend(
        loaded
            .standard_sources()
            .iter()
            .map(|(path, source)| SourceUnit {
                fixture_id: "vfs-mounted-row".into(),
                source_id: path.clone(),
                parse_as: "module_unit".into(),
                source: source.clone(),
            }),
    );

    let project = ProjectUnit {
        fixture_id: "vfs-mounted-row".into(),
        project_id: "mounted".into(),
        environment_id: None,
        modules,
        loose_rows: Vec::new(),
        expectations: ProjectExpectations {
            environment: ProjectEnvironment {
                network: false,
                credentials: false,
                intrinsics: "none".into(),
                stdlib: None,
                initial_tables: "empty".into(),
            },
            steps: Vec::new(),
            negative_cases: Vec::new(),
        },
    };
    let catalogue = loaded
        .production_catalogue()
        .map_err(|_| StageOutcome::Failed(standard_profile_diagnostic()))?;
    let admitted = admit(&project, limits, "", &catalogue).map_err(|failure| *failure)?;
    let candidate_table_fields = candidate_table_fields_in_stable_id_order(
        loaded,
        identities,
        &project,
        &admitted.table_fields,
    )
    .map_err(|reason| StageOutcome::Skipped { reason })?;
    let mut validator = TransactionalTableCandidateValidator::new(
        cwd_generation,
        &admitted.functions,
        &admitted.key_fields,
        &admitted.float_fields,
        &admitted.table_fields,
        &admitted.table_assertions,
        &admitted.module_assertions,
        table_object_ids,
        limits,
    );
    validator.candidate_table_fields = candidate_table_fields;
    // A table whose key admission could not lower (bead ornadb-gq4g) holds rows
    // and assertions but no key; the commit gate refuses only the checks that read it.
    validator.unsupported = admitted
        .table_fields
        .keys()
        .filter(|table| !admitted.key_fields.contains_key(*table))
        .cloned()
        .collect();
    Ok(Box::new(validator))
}

pub(super) fn candidate_table_fields_in_stable_id_order(
    loaded: &orna_project::LoadedProject,
    identities: &orna_project::ObjectIdentityManifest,
    project: &ProjectUnit,
    source_fields: &TableFields,
) -> Result<TableFields, String> {
    let mut ordered_fields = source_fields.clone();
    for unit in &project.modules {
        if is_standard_source(unit) {
            continue;
        }
        let source_path = unit
            .source_id
            .strip_prefix("mounted/")
            .ok_or_else(|| "candidate table source has no committed module path".to_owned())?;
        let identity = loaded
            .identities()
            .iter()
            .find(|identity| identity.logical_path() == source_path)
            .ok_or_else(|| format!("candidate module {source_path} has no committed identity"))?;
        let schema_namespace = identity.namespace().join(".");
        for item in parse_module(&unit.source).value.items {
            let Declaration::Table { name, .. } = item.declaration else {
                continue;
            };
            let table = qualified_table_identity(&Namespace(identity.namespace().to_vec()), &name);
            let fields = ordered_fields
                .get_mut(&table)
                .ok_or_else(|| format!("candidate table {table} has no admitted fields"))?;
            let schema = loaded
                .source_table_schema(identities, &schema_namespace, &name)
                .map_err(|_| format!("candidate table {name} has no stable schema"))?;
            *fields = order_field_names_by_stable_id(fields, schema.field_object_ids())?;
        }
    }
    Ok(ordered_fields)
}

impl TransactionalTableCandidateValidator {
    /// The inputs are one table candidate: its working directory generation,
    /// the retained functions and the table schema it validates against.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        cwd_generation: Option<orna_repository::CwdGeneration>,
        functions: &Functions,
        key_fields: &TableKeys,
        float_fields: &TableFloatFields,
        table_fields: &TableFields,
        table_assertions: &TableAssertions,
        module_assertions: &ModuleAssertions,
        table_object_ids: Arc<BTreeMap<String, orna_runtime::TableObjectId>>,
        limits: EvaluatorLimits,
    ) -> Self {
        let mut tables = key_fields.keys().cloned().collect::<BTreeSet<_>>();
        for assertions in table_assertions.values() {
            for assertion in assertions {
                tables.extend(concrete_dependency_tables(&assertion.dependencies));
            }
        }
        for assertion in module_assertions {
            tables.extend(concrete_dependency_tables(&assertion.dependencies));
        }
        Self {
            cwd_generation,
            table_object_ids,
            table_assertions: table_assertions.clone(),
            module_assertions: module_assertions.clone(),
            candidate_table_fields: table_fields.clone(),
            candidate_key_fields: key_fields
                .iter()
                .map(|(table, key)| (table.clone(), key.fields.clone()))
                .collect(),
            functions: lower_relation_bindings(functions, key_fields, float_fields, table_fields),
            limits,
            tables: tables.into_iter().collect(),
            unsupported: BTreeSet::new(),
            refused_module_assertion: None,
        }
    }
}

impl TableActivationCandidateValidator for TransactionalTableCandidateValidator {
    fn tables(&self) -> &[String] {
        &self.tables
    }
    fn table_object_ids(&self) -> BTreeMap<String, orna_runtime::TableObjectId> {
        self.table_object_ids.as_ref().clone()
    }

    fn cwd_generation(&self) -> Option<&orna_repository::CwdGeneration> {
        self.cwd_generation.as_ref()
    }

    fn validate(&mut self, rows: &orna_runtime::RuntimeTableRows) -> Result<(), SafeDiagnostic> {
        let all: BTreeSet<String> = self.tables.iter().cloned().collect();
        self.validate_changed(rows, &all)
    }

    fn failed_module_assertion(&self) -> Option<String> {
        self.refused_module_assertion.clone()
    }

    fn validate_changed(
        &mut self,
        rows: &orna_runtime::RuntimeTableRows,
        changed_tables: &BTreeSet<String>,
    ) -> Result<(), SafeDiagnostic> {
        self.refused_module_assertion = None;
        let mut budget = StepBudget::new(self.limits.max_steps);
        for (table, assertions) in &self.table_assertions {
            if self.unsupported.contains(table) {
                return Err(table_evaluation_diagnostic());
            }
            for assertion in assertions {
                if table_assertion_function(&assertion.expression, &self.functions).is_some() {
                    let mut effects = CandidateRelationEffectHandler { rows };
                    let value = evaluate_function_assertion(
                        table,
                        &assertion.expression,
                        &self.functions,
                        self.limits,
                        &mut budget,
                        &mut effects,
                    )
                    .map_err(|_| table_evaluation_diagnostic())?;
                    if !matches!(value.raw(), OvbRaw::Bool(true)) {
                        return Err(table_assertion_diagnostic());
                    }
                    continue;
                }
                let (kind, binding, predicate) = table_assertion_predicate(&assertion.expression)
                    .map_err(|_| table_evaluation_diagnostic())?;
                let mut projections = BTreeSet::new();
                let mut candidate_rows = 0usize;
                for (key, encoded) in rows.get(table).ok_or_else(table_evaluation_diagnostic)? {
                    debit_host_step(&mut budget).map_err(|_| table_evaluation_diagnostic())?;
                    candidate_rows = candidate_rows
                        .checked_add(1)
                        .ok_or_else(table_evaluation_diagnostic)?;
                    self.limits
                        .check_items(candidate_rows)
                        .map_err(|_| table_evaluation_diagnostic())?;
                    let row = candidate_assertion_row(
                        table,
                        key,
                        encoded,
                        &self.candidate_table_fields,
                        &self.candidate_key_fields,
                    )?;
                    let environment = Environment::from([(binding.to_owned(), row)]);
                    let value = evaluate_with_functions_and_budget(
                        predicate,
                        &environment,
                        &self.functions,
                        self.limits,
                        &mut budget,
                    )
                    .map_err(|_| table_evaluation_diagnostic())?;
                    match kind {
                        TableAssertionKind::Every if matches!(value.raw(), OvbRaw::Bool(true)) => {}
                        TableAssertionKind::Every => {
                            return Err(table_assertion_diagnostic());
                        }
                        TableAssertionKind::AllUnique => {
                            let projection =
                                value.encode().map_err(|_| table_evaluation_diagnostic())?;
                            if !projections.insert(projection) {
                                return Err(table_assertion_diagnostic());
                            }
                        }
                    }
                }
            }
        }
        // ORNA-ASSERT-026: a module assertion is evaluated only when a validating
        // boundary changed a table in its transitive dependency set.
        let applicable = self
            .module_assertions
            .iter()
            .filter(|assertion| {
                assertion_dependencies_apply(&assertion.dependencies, changed_tables)
            })
            .cloned()
            .collect::<Vec<_>>();
        if !applicable.is_empty() {
            // A module assertion that reads an unsupported table cannot be
            // evaluated, so it is refused rather than skipped (bead ornadb-gq4g).
            if applicable.iter().any(|assertion| {
                assertion
                    .dependencies
                    .iter()
                    .any(|table| self.unsupported.contains(table))
            }) {
                return Err(table_evaluation_diagnostic());
            }
            // Module assertions read whole rows by field name, so candidate rows
            // are named from their key and stable field order, exactly as the
            // table assertions above name them. Unsupported tables are left out.
            let scoped = rows
                .iter()
                .filter(|(table, _)| !self.unsupported.contains(table.as_str()))
                .map(|(table, entries)| (table.clone(), entries.clone()))
                .collect::<orna_runtime::RuntimeTableRows>();
            let named = named_candidate_rows(
                &scoped,
                &self.candidate_table_fields,
                &self.candidate_key_fields,
            )?;
            for assertion in applicable {
                let value = evaluate_module_assertion_rows(
                    &named,
                    &assertion.expression,
                    &Environment::new(),
                    &self.functions,
                    self.limits,
                    &mut budget,
                    0,
                )
                .map_err(|_| table_evaluation_diagnostic())?;
                if !matches!(value.raw(), OvbRaw::Bool(true)) {
                    self.refused_module_assertion = Some(assertion_site(&assertion));
                    return Err(table_assertion_diagnostic());
                }
            }
        }
        Ok(())
    }
}

pub(super) struct ListTableCandidateValidator {
    pub(super) table: String,
    pub(super) assertions: Vec<AdmittedAssertion>,
    pub(super) module_assertions: Vec<AdmittedAssertion>,
    pub(super) functions: Functions,
    pub(super) limits: EvaluatorLimits,
    pub(super) tables: Vec<String>,
    /// Shared with the handler that built this validator; holds the site of the
    /// assertion that refused the item (ORNA-ASSERT-051).
    pub(super) assertion_refusal: Arc<Mutex<Option<String>>>,
}

impl ListTableCandidateValidator {
    pub(super) fn new(
        bridge: &ListStreamBridge,
        limits: EvaluatorLimits,
        assertion_refusal: Arc<Mutex<Option<String>>>,
    ) -> Result<Self, String> {
        let module_assertions =
            applicable_module_assertions(&bridge.table, &bridge.module_assertions)?;
        let mut tables = BTreeSet::from([bridge.table.clone()]);
        for assertion in &module_assertions {
            tables.extend(concrete_dependency_tables(&assertion.dependencies));
        }
        Ok(Self {
            table: bridge.table.clone(),
            assertions: bridge.table_assertions.clone(),
            module_assertions,
            functions: bridge.assertion_functions.clone(),
            limits,
            tables: tables.into_iter().collect(),
            assertion_refusal,
        })
    }

    pub(super) fn clear_refusal(&self) {
        *self
            .assertion_refusal
            .lock()
            .expect("assertion refusal slot is not poisoned") = None;
    }

    /// Records the assertion that refused the item and returns its diagnostic
    /// unchanged, so the failure can name the owner and span (ORNA-ASSERT-051).
    pub(super) fn refuse(
        &self,
        assertion: &AdmittedAssertion,
        diagnostic: SafeDiagnostic,
    ) -> SafeDiagnostic {
        *self
            .assertion_refusal
            .lock()
            .expect("assertion refusal slot is not poisoned") = Some(assertion_site(assertion));
        diagnostic
    }

    pub(super) fn check_table_assertion(
        &self,
        assertion: &AdmittedAssertion,
        rows: &orna_runtime::RuntimeTableRows,
        budget: &mut StepBudget,
    ) -> Result<(), SafeDiagnostic> {
        if table_assertion_function(&assertion.expression, &self.functions).is_some() {
            let mut effects = CandidateRelationEffectHandler { rows };
            let value = evaluate_function_assertion(
                &self.table,
                &assertion.expression,
                &self.functions,
                self.limits,
                budget,
                &mut effects,
            )
            .map_err(|_| table_evaluation_diagnostic())?;
            if !matches!(value.raw(), OvbRaw::Bool(true)) {
                return Err(table_assertion_diagnostic());
            }
            return Ok(());
        }
        let (kind, binding, predicate) = table_assertion_predicate(&assertion.expression)
            .map_err(|_| table_evaluation_diagnostic())?;
        let mut projections = BTreeSet::new();
        let mut candidate_rows = 0usize;
        for (_, encoded) in rows
            .get(&self.table)
            .ok_or_else(table_evaluation_diagnostic)?
        {
            debit_host_step(budget).map_err(|_| table_evaluation_diagnostic())?;
            candidate_rows = candidate_rows
                .checked_add(1)
                .ok_or_else(table_evaluation_diagnostic)?;
            self.limits
                .check_items(candidate_rows)
                .map_err(|_| table_evaluation_diagnostic())?;
            let row = Value::decode(encoded).map_err(|_| table_row_decode_diagnostic())?;
            let environment = Environment::from([(binding.to_owned(), row)]);
            let value = evaluate_with_functions_and_budget(
                predicate,
                &environment,
                &self.functions,
                self.limits,
                budget,
            )
            .map_err(|_| table_evaluation_diagnostic())?;
            match kind {
                TableAssertionKind::Every if matches!(value.raw(), OvbRaw::Bool(true)) => {}
                TableAssertionKind::Every => return Err(table_assertion_diagnostic()),
                TableAssertionKind::AllUnique => {
                    let projection = value.encode().map_err(|_| table_evaluation_diagnostic())?;
                    if !projections.insert(projection) {
                        return Err(table_assertion_diagnostic());
                    }
                }
            }
        }
        Ok(())
    }

    pub(super) fn check_module_assertion(
        &self,
        assertion: &AdmittedAssertion,
        rows: &orna_runtime::RuntimeTableRows,
        budget: &mut StepBudget,
    ) -> Result<(), SafeDiagnostic> {
        let value = evaluate_module_assertion_rows(
            rows,
            &assertion.expression,
            &Environment::new(),
            &self.functions,
            self.limits,
            budget,
            0,
        )
        .map_err(|_| table_evaluation_diagnostic())?;
        if !matches!(value.raw(), OvbRaw::Bool(true)) {
            return Err(table_assertion_diagnostic());
        }
        Ok(())
    }
}

impl StreamTableCandidateValidator for ListTableCandidateValidator {
    fn tables(&self) -> &[String] {
        &self.tables
    }

    fn validate(&mut self, rows: &orna_runtime::RuntimeTableRows) -> Result<(), SafeDiagnostic> {
        let all: BTreeSet<String> = self.tables.iter().cloned().collect();
        self.validate_changed(rows, &all)
    }

    fn validate_changed(
        &mut self,
        rows: &orna_runtime::RuntimeTableRows,
        changed_tables: &BTreeSet<String>,
    ) -> Result<(), SafeDiagnostic> {
        self.clear_refusal();
        let mut budget = StepBudget::new(self.limits.max_steps);
        for assertion in &self.assertions {
            self.check_table_assertion(assertion, rows, &mut budget)
                .map_err(|diagnostic| self.refuse(assertion, diagnostic))?;
        }
        for assertion in self.module_assertions.iter().filter(|assertion| {
            assertion_dependencies_apply(&assertion.dependencies, changed_tables)
        }) {
            self.check_module_assertion(assertion, rows, &mut budget)
                .map_err(|diagnostic| self.refuse(assertion, diagnostic))?;
        }
        Ok(())
    }
}

impl ListTableHandler {
    pub(super) fn new(bridge: ListStreamBridge, limits: EvaluatorLimits) -> Self {
        Self {
            bridge,
            limits,
            next_mutation: 0,
            digest: [9; 32],
            refusal: None,
            assertion_refusal: Arc::new(Mutex::new(None)),
        }
    }
}

impl StreamHandler for ListTableHandler {
    fn handle(&mut self, item: &StreamItem) -> StreamHandlerResult {
        let value = match Value::decode(&item.payload) {
            Ok(value) => value,
            Err(_) => return StreamHandlerResult::Fail(stream_handler_diagnostic()),
        };
        let row = match evaluate_list_item_row(&self.bridge, &value, self.limits) {
            Ok(row) => row,
            Err(error) => {
                self.refusal = Some(item_evaluation_refusal(&self.bridge, &error));
                return StreamHandlerResult::Fail(stream_handler_diagnostic());
            }
        };
        let key = match table_key(&row, &self.bridge.key_fields) {
            Ok(key) => key,
            Err(_) => return StreamHandlerResult::Fail(stream_handler_diagnostic()),
        };
        let mut hasher = Sha256::new();
        hasher.update(b"ORNA-LIST-STREAM-MUTATION\0");
        hasher.update(item.delivery.canonical().as_bytes());
        hasher.update(self.next_mutation.to_be_bytes());
        let id = hasher.finalize()[..16]
            .try_into()
            .expect("fixed digest length");
        let encoded = match row.encode() {
            Ok(encoded) => encoded,
            Err(_) => return StreamHandlerResult::Fail(stream_handler_diagnostic()),
        };
        let mutation = match TableMutation::new(id, &self.bridge.table, key, Some(encoded)) {
            Ok(mutation) => mutation,
            Err(_) => return StreamHandlerResult::Fail(stream_handler_diagnostic()),
        };
        self.next_mutation = self.next_mutation.saturating_add(1);
        let mut digest = Sha256::new();
        digest.update(b"ORNA-LIST-STREAM-COMMIT\0");
        digest.update(self.digest);
        digest.update(mutation.id());
        self.digest = digest.finalize().into();
        // Constructing the validator is part of handler execution. If it
        // fails after the mutation was staged, preserve that staged batch only
        // in the failure result so the runtime can discard it atomically.
        let validator = match ListTableCandidateValidator::new(
            &self.bridge,
            self.limits,
            Arc::clone(&self.assertion_refusal),
        ) {
            Ok(validator) => Box::new(validator),
            Err(_) => {
                return StreamHandlerResult::FailAfterTable(
                    StreamTableMutationBatch {
                        mutations: vec![mutation],
                        next_digest: self.digest,
                    },
                    stream_handler_diagnostic(),
                );
            }
        };
        StreamHandlerResult::CommitValidatedTable(StreamValidatedTableMutationBatch {
            mutations: vec![mutation],
            next_digest: self.digest,
            // Even a table with no declared assertions crosses the durable
            // boundary through an explicit no-op validator.
            validator,
        })
    }
}
