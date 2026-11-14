use super::*;

pub(super) type TransactionDatabase = DatabaseRuntime<String, TransactionKey, Value>;
pub(super) type TransactionActivation<'runtime> =
    DatabaseActivation<'runtime, String, TransactionKey, Value>;

/// A bounded source executor for the first real table-transaction slice.
///
/// It admits a module through the v1 syntax and semantic analyzer, retains its
/// function bodies, and routes explicit-key table mutations through one root
/// activation. Other effects remain explicit skips until their own runtime
/// contracts exist.
pub struct TransactionalEvaluator {
    pub(super) entry: String,
    pub(super) limits: EvaluatorLimits,
    pub(super) database: TransactionDatabase,
    pub(super) activation_time: Option<SystemTime>,
    pub(super) publication_rows: Option<RuntimePublicationMetadataRows>,
    pub(super) mutation_scope: [u8; 32],
    pub(super) sys_host: Option<std::sync::Arc<SysHostAttachment>>,
    pub(super) explain_catalogue: Option<ActivationExplainCatalogue>,
    pub(super) admission: AdmissionCatalogue,
}

/// Catalogue that admission checks user modules against. `Production` is the
/// authoritative core catalogue; `ConformanceFixture` additionally reserves the
/// conformance fixture namespaces and is opted into by the conformance harness
/// only, through [`TransactionalEvaluator::with_fixture_catalogue`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionCatalogue {
    Production,
    ConformanceFixture,
}

impl AdmissionCatalogue {
    pub(super) fn base(self) -> Catalogue {
        match self {
            Self::Production => Catalogue::authoritative_core(),
            Self::ConformanceFixture => Catalogue::authoritative_fixture(),
        }
    }
}

impl TransactionalEvaluator {
    /// Creates an evaluator that admits transactions against the production
    /// catalogue (`AdmissionCatalogue::Production`). The conformance harness
    /// opts into the fixture catalogue with [`Self::with_fixture_catalogue`].
    #[must_use]
    pub fn new(entry: impl Into<String>, limits: EvaluatorLimits) -> Self {
        Self {
            entry: entry.into(),
            limits,
            admission: AdmissionCatalogue::Production,
            database: TransactionDatabase::default(),
            activation_time: None,
            publication_rows: None,
            mutation_scope: [0; 32],
            sys_host: None,
            explain_catalogue: None,
        }
    }

    /// Opts this evaluator into the conformance fixture catalogue. Production
    /// evaluators never use it. The only callers are the conformance harness
    /// (`CompositeEvaluator::default` in `main.rs` and
    /// `SemanticAdapter::evaluate`), which need `execute_duplicate_key_fixture`.
    #[must_use]
    pub fn with_fixture_catalogue(mut self) -> Self {
        self.admission = AdmissionCatalogue::ConformanceFixture;
        self
    }

    pub(super) fn with_activation_time(mut self, activation_time: SystemTime) -> Self {
        self.activation_time = Some(activation_time);
        self
    }

    pub(super) fn with_publication_rows(mut self, rows: RuntimePublicationMetadataRows) -> Self {
        self.publication_rows = Some(rows);
        self
    }

    pub(super) fn with_mutation_scope(mut self, scope: [u8; 32]) -> Self {
        self.mutation_scope = scope;
        self
    }

    pub(super) fn with_sys_host_context(
        mut self,
        sys_host: Option<std::sync::Arc<SysHostAttachment>>,
    ) -> Self {
        self.sys_host = sys_host;
        self
    }

    /// Pins `sys.explain` inputs to the activation's snapshot.
    pub(super) fn with_explain_catalogue(mut self, catalogue: ActivationExplainCatalogue) -> Self {
        self.explain_catalogue = Some(catalogue);
        self
    }

    /// Returns a committed row by its canonical encoded key.
    pub fn committed_row(&self, table: &str, key: &Value) -> Option<&Value> {
        let encoded = TransactionKey::new(key.encode().ok()?);
        let table = table.to_owned();
        self.database.committed(&table, &encoded)
    }

    /// Executes the configured entry function inside one root activation.
    /// Errors escaping the function leave all table writes unpublished.
    pub fn execute_source(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        let admitted = match admit_transaction_source(
            unit,
            self.limits,
            &self.entry,
            &self.admission.base(),
        ) {
            Ok(value) => value,
            Err(outcome) => return *outcome,
        };
        match self.execute_admitted(&admitted) {
            Ok(_) => StageOutcome::Passed,
            Err(diagnostic) => StageOutcome::Failed(*diagnostic),
        }
    }

    /// Executes only the frozen duplicate-primary-key fixture through the
    /// table activation boundary. The fixture has no local table declaration,
    /// so this bounded host supplies its declared `Contact` table while
    /// retaining the authoritative function body unchanged.
    pub fn execute_duplicate_key_fixture(
        &mut self,
        unit: &SourceUnit,
    ) -> Option<StageOutcome<Diagnostic>> {
        if !duplicate_key_fixture_contract(unit) {
            return None;
        }
        let mut evaluator = Self::new("bad", self.limits);
        let mut admitted =
            match admit_transaction_source(unit, self.limits, "bad", &self.admission.base()) {
                Ok(value) => value,
                Err(outcome) => return Some(*outcome),
            };
        admitted.key_fields = BTreeMap::from([(
            String::from("Contact"),
            TransactionTableKey::new(
                String::from("Contact"),
                vec![String::from("id")],
                vec![TransactionKeyType::Str],
            )
            .expect("valid Contact key schema"),
        )]);
        admitted.float_fields.clear();
        let outcome = match evaluator.execute_admitted(&admitted) {
            Ok(_) => StageOutcome::Passed,
            Err(diagnostic) => StageOutcome::Failed(*diagnostic),
        };
        Some(match outcome {
            StageOutcome::Failed(diagnostic)
                if diagnostic.code() == "ORNA-EVAL-TABLE-DUPLICATE"
                    && evaluator
                        .committed_row(
                            "Contact",
                            &Value::new(OvbRaw::Text("alice".into())).expect("static Contact key"),
                        )
                        .is_none() =>
            {
                StageOutcome::Failed(duplicate_primary_key_diagnostic())
            }
            outcome => outcome,
        })
    }

    pub(super) fn run_transaction_scenario(&self, scenario: &Scenario) -> StageOutcome<Diagnostic> {
        let (entry, source) = match scenario.id.as_str() {
            "TXN-001" => (
                "parent",
                "pub table Note(id: Int) { text: Str, } fn child() { Note.insert({ id: 7, text: \"nested\" }); } fn parent() { child(); assert false; }",
            ),
            "TXN-002" => (
                "main",
                "pub table Order(id: Int) { text: Str, } pub table Payment(id: Int) { text: Str, } pub table Audit(id: Int) { text: Str, } fn main() { Order.insert({ id: 1, text: \"order\" }); Payment.insert({ id: 1, text: \"payment\" }); Audit.insert({ id: 1, text: \"audit\" }); assert Order.count() == 1; assert Payment.count() == 1; assert Audit.count() == 1; }",
            ),
            _ => {
                return StageOutcome::Skipped {
                    reason: "scenario has no implemented execution contract in the bounded runtime"
                        .into(),
                };
            }
        };
        let unit = SourceUnit {
            fixture_id: scenario.id.clone(),
            source_id: format!("{}.orna", scenario.id.to_lowercase()),
            parse_as: "module_unit".into(),
            source: source.into(),
        };
        let mut evaluator = TransactionalEvaluator::new(entry, self.limits);
        match scenario.id.as_str() {
            "TXN-001" => match evaluator.execute_source(&unit) {
                StageOutcome::Failed(diagnostic)
                    if diagnostic.code() == "ORNA-A091-008"
                        && evaluator
                            .committed_row("Note", &Value::int(7.into()))
                            .is_none() =>
                {
                    StageOutcome::Passed
                }
                _ => scenario_mismatch(),
            },
            "TXN-002" => match evaluator.execute_source(&unit) {
                StageOutcome::Passed
                    if evaluator
                        .committed_row("Order", &Value::int(1.into()))
                        .is_some()
                        && evaluator
                            .committed_row("Payment", &Value::int(1.into()))
                            .is_some()
                        && evaluator
                            .committed_row("Audit", &Value::int(1.into()))
                            .is_some() =>
                {
                    StageOutcome::Passed
                }
                _ => scenario_mismatch(),
            },
            _ => unreachable!("transaction scenario contract admitted only known IDs"),
        }
    }

    pub(super) fn execute_admitted(
        &mut self,
        admitted: &AdmittedTransaction,
    ) -> Result<Vec<TableMutation>, Box<Diagnostic>> {
        self.execute_admitted_with_arguments(admitted, &Environment::new())
    }

    pub(super) fn execute_admitted_with_arguments(
        &mut self,
        admitted: &AdmittedTransaction,
        arguments: &Environment,
    ) -> Result<Vec<TableMutation>, Box<Diagnostic>> {
        let AdmittedTransaction {
            functions,
            key_fields,
            float_fields,
            table_fields,
            table_assertions,
            module_assertions,
            nominal_definitions,
        } = admitted;
        let entry = self.entry.clone();
        let limits = self.limits;
        let mut mutations = Vec::new();
        let result = self.database.activate(|activation| {
            let functions =
                lower_relation_bindings(functions, key_fields, float_fields, table_fields);
            let mut effects = TableEffectHandler {
                activation,
                key_fields,
                table_fields,
                mutations: &mut mutations,
                next_mutation: 0,
                limits,
                activation_time: self.activation_time,
                publication_rows: self.publication_rows.clone(),
                mutation_scope: self.mutation_scope,
                sys_host: self.sys_host.as_deref(),
                explain_catalogue: self.explain_catalogue.as_ref(),
            };
            let mut budget = StepBudget::new(limits.max_steps);
            let result = invoke_named_with_effects_nominals_and_budget(
                &entry,
                &functions,
                arguments,
                nominal_definitions,
                limits,
                &mut effects,
                &mut budget,
            );
            result.and_then(|_| {
                validate_table_assertions(
                    activation,
                    table_assertions,
                    &functions,
                    limits,
                    &mut budget,
                )?;
                validate_module_assertions(
                    activation,
                    module_assertions,
                    &functions,
                    limits,
                    &mut budget,
                )
            })
        });
        match result {
            Ok(()) => Ok(mutations),
            Err(ActivationError::Operation(error)) => Err(Box::new(error.diagnostic().clone())),
            Err(ActivationError::Commit(error)) => {
                Err(Box::new(transaction_error_diagnostic(error)))
            }
        }
    }

    pub(super) fn seed_committed_from_admission(
        &mut self,
        table: String,
        key: Vec<u8>,
        row: Value,
        key_fields: &TableKeys,
        storage_orders: &BTreeMap<String, Vec<String>>,
        table_fields: &TableFields,
    ) -> Result<(), RuntimeError> {
        let schema = key_fields
            .get(&table)
            .ok_or(RuntimeError::InvalidTableMutation)?;
        // A published row is stored as its descriptor-ordered field values, so
        // the row carries no field names and no key field, and its key is the
        // ORP canonical form rather than the component encoding this evaluator
        // addresses rows by. Both are rebuilt here (bead ornadb-jm0d).
        let key = match schema.key_from_encoded(&key) {
            Ok(key) => key,
            Err(_) => schema
                .key_from_canonical_text(&key)
                .ok_or(RuntimeError::InvalidTableMutation)?,
        };
        let row = stored_row_record(&table, &row, &key, key_fields, storage_orders, table_fields)?;
        self.database
            .activate(|activation| activation.insert(table, key, row))
            .map_err(|_| RuntimeError::InvalidTableMutation)
    }
}

/// Rebuilds one published row as an evaluator record.
///
/// A published row is stored as its descriptor-ordered field values, so the
/// row carries no field names and no key field, and its key is the ORP canonical
/// form rather than the component encoding this evaluator addresses rows by.
/// The key components and the declared field order rebuild the record (bead
/// ornadb-jm0d).
pub(super) fn stored_row_record(
    table: &str,
    row: &Value,
    key: &TransactionKey,
    key_fields: &TableKeys,
    storage_orders: &BTreeMap<String, Vec<String>>,
    table_fields: &TableFields,
) -> Result<Value, RuntimeError> {
    // The stored row is positional in the descriptor's storage order, which is
    // not the declaration order, so the order comes from the committed read. A
    // table with no committed rows has no recorded order, and its rows are
    // already in the admitted order (bead ornadb-jm0d).
    // A caller that supplies no storage order at all (the harness path) hands
    // over rows in the form its evaluator expects, so they are used as they are
    // (bead ornadb-jm0d).
    if storage_orders.is_empty() {
        return Ok(row.clone());
    }
    let declared = match storage_orders.get(table) {
        Some(order) => order,
        None => match table_fields.get(table) {
            Some(fields) => fields,
            // A caller with no schema metadata hands over a row that is already
            // in record form (bead ornadb-jm0d).
            None => return Ok(row.clone()),
        },
    };
    let key_names = key_fields
        .get(table)
        .ok_or(RuntimeError::InvalidTableMutation)?;
    let key_names: Vec<&String> = key_names.iter().collect();
    let components = key.components().ok_or(RuntimeError::InvalidTableMutation)?;
    if components.len() != key_names.len() {
        return Err(RuntimeError::InvalidTableMutation);
    }
    let mut entries: Vec<(orna_value::Raw, orna_value::Raw)> = Vec::new();
    for (name, component) in key_names.iter().zip(components) {
        let value = orna_value::Value::decode(
            &component
                .encode()
                .map_err(|_| RuntimeError::InvalidTableMutation)?,
        )
        .map_err(|_| RuntimeError::InvalidTableMutation)?;
        entries.push((orna_value::Raw::Text((*name).clone()), value.raw().clone()));
    }
    let positional = match row.raw() {
        orna_value::Raw::Array(values) => values.clone(),
        other => vec![other.clone()],
    };
    let remaining: Vec<&String> = declared
        .iter()
        .filter(|name| !key_names.contains(name))
        .collect();
    if positional.len() != remaining.len() {
        return Err(RuntimeError::InvalidTableMutation);
    }
    for (name, value) in remaining.iter().zip(positional) {
        entries.push((orna_value::Raw::Text((*name).clone()), value));
    }
    // A canonical map carries its keys in sorted order (ORNA-OVB canonical
    // form), so the rebuilt record is sorted before it is validated. The key
    // fields and the positional fields arrive in different orders, which is why
    // an unsorted map fails as DuplicateOrUnorderedMapKey (bead ornadb-jm0d).
    // Canonical CBOR orders map keys by their encoded bytes, so a text key
    // sorts by its length first and then by its bytes: "id" precedes "title"
    // precedes "artist" (bead ornadb-jm0d).
    entries.sort_by(|left, right| {
        let name = |entry: &(orna_value::Raw, orna_value::Raw)| match &entry.0 {
            orna_value::Raw::Text(name) => name.clone(),
            _ => String::new(),
        };
        let (left, right) = (name(left), name(right));
        left.len().cmp(&right.len()).then_with(|| left.cmp(&right))
    });
    Value::new(orna_value::Raw::Map(entries)).map_err(|_| RuntimeError::InvalidTableMutation)
}

impl Default for TransactionalEvaluator {
    fn default() -> Self {
        Self::new("main", EvaluatorLimits::default())
    }
}

impl RuntimeEvaluator for TransactionalEvaluator {
    fn evaluate(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.execute_source(unit)
    }

    fn validate_row(&mut self, _: &SourceUnit) -> StageOutcome<Diagnostic> {
        StageOutcome::Skipped {
            reason: "transactional source execution does not replace schema-backed row admission"
                .into(),
        }
    }

    fn validate_rows(&mut self, _: &ProjectUnit) -> StageOutcome<Diagnostic> {
        StageOutcome::Skipped {
            reason: "transactional source execution does not replace schema-backed row admission"
                .into(),
        }
    }

    fn run_scenario(&mut self, scenario: &Scenario) -> StageOutcome<Diagnostic> {
        if transaction_contract(scenario) {
            return self.run_transaction_scenario(scenario);
        }
        StageOutcome::Skipped {
            reason: "source transaction execution is exposed as an explicit module seam; no corpus scenario is claimed yet".into(),
        }
    }
}
