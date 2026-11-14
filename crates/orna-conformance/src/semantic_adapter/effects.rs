use super::*;

pub(super) struct ListStreamBridge {
    pub(super) source_identity: String,
    pub(super) entry: String,
    pub(super) producer_entry: String,
    pub(super) consumer_principal: String,
    pub(super) consumer_root: String,
    pub(super) consumer_binding: String,
    pub(super) partition: Option<String>,
    pub(super) table: String,
    pub(super) key_fields: Vec<String>,
    pub(super) parameter: String,
    pub(super) insert_row: Expr,
    pub(super) table_assertions: Vec<AdmittedAssertion>,
    pub(super) module_assertions: Vec<AdmittedAssertion>,
    pub(super) assertion_functions: Functions,
    pub(super) payloads: Vec<Vec<u8>>,
}

impl ListStreamBridge {
    pub(super) fn checkpoint_key(&self) -> Result<CheckpointKey, RuntimeError> {
        fn component(value: &str) -> Result<Component, RuntimeError> {
            Component::new(value).map_err(|_| RuntimeError::RecoveryInvalid)
        }
        Ok(CheckpointKey {
            consumer: ConsumerIdentity {
                principal: component(&self.consumer_principal)?,
                root: component(&self.consumer_root)?,
                function: component(&self.entry)?,
                binding: component(&self.consumer_binding)?,
            },
            source_format: component("orna-stream")?,
            source: component(&self.source_identity)?,
            partition_format: component("literal-list")?,
            partition: self.partition.as_deref().map(component).transpose()?,
            position_format: component("orna.list.v1")?,
        })
    }
}

pub(super) struct ListTableHandler {
    pub(super) bridge: ListStreamBridge,
    pub(super) limits: EvaluatorLimits,
    pub(super) next_mutation: u64,
    pub(super) digest: [u8; 32],
    /// The refusal for the item that failed evaluation, reported by the caller
    /// in place of the generic delivery diagnostic.
    pub(super) refusal: Option<Diagnostic>,
    /// The site of the assertion that refused the current item, written by the
    /// item's validator and read when the item fails (ORNA-ASSERT-051).
    pub(super) assertion_refusal: Arc<Mutex<Option<String>>>,
}

pub(super) struct CandidateRelationEffectHandler<'a> {
    pub(super) rows: &'a orna_runtime::RuntimeTableRows,
}

impl EffectHandler for CandidateRelationEffectHandler<'_> {
    fn handle(
        &mut self,
        _callee: &Expr,
        _arguments: &[Value],
    ) -> Result<Option<Value>, EvaluationError> {
        Ok(None)
    }

    fn scan_relation_page(
        &mut self,
        source: &str,
        after: Option<&[u8]>,
        max_rows: usize,
        budget: &mut StepBudget,
    ) -> Result<Option<RelationPage>, EvaluationError> {
        let Some(candidates) = self.rows.get(source) else {
            return Ok(None);
        };
        if max_rows == 0 {
            return Ok(Some(RelationPage {
                rows: Vec::new(),
                next: None,
            }));
        }
        let start = relation_cursor_start(after)?;
        if start > candidates.len() {
            return Err(transaction_error("ORNA-EVAL-TABLE-ASSERT"));
        }
        let end = start.saturating_add(max_rows).min(candidates.len());
        let mut rows = Vec::with_capacity(end - start);
        for (_, encoded) in &candidates[start..end] {
            budget.debit(1)?;
            rows.push(
                Value::decode(encoded).map_err(|_| transaction_error("ORNA-EVAL-TABLE-ASSERT"))?,
            );
        }
        Ok(Some(RelationPage {
            rows,
            next: (end < candidates.len()).then(|| (end as u64).to_be_bytes().to_vec()),
        }))
    }
}

pub(super) fn relation_cursor_start(after: Option<&[u8]>) -> Result<usize, EvaluationError> {
    let Some(after) = after else {
        return Ok(0);
    };
    let cursor: [u8; 8] = after
        .try_into()
        .map_err(|_| transaction_error("ORNA-EVAL-TABLE-ASSERT"))?;
    usize::try_from(u64::from_be_bytes(cursor))
        .map_err(|_| transaction_error("ORNA-EVAL-TABLE-ASSERT"))
}

pub(super) fn table_assertion_function<'a>(
    assertion: &'a Expr,
    functions: &Functions,
) -> Option<&'a str> {
    match assertion {
        Expr::Name { text, .. } if functions.contains_key(text) => Some(text),
        _ => None,
    }
}

pub(super) fn evaluate_function_assertion(
    table: &str,
    assertion: &Expr,
    functions: &Functions,
    limits: EvaluatorLimits,
    budget: &mut StepBudget,
    effects: &mut dyn EffectHandler,
) -> Result<Value, EvaluationError> {
    let span = assertion.span();
    let body = Expr::Call {
        callee: Box::new(assertion.clone()),
        arguments: vec![orna_syntax_ast::Argument {
            name: None,
            value: relation_source_expression(table, span.clone()),
            span: span.clone(),
        }],
        span,
    };
    let wrapper_name = "__orna_table_assertion";
    let mut admitted = functions.clone();
    admitted.insert(
        wrapper_name.into(),
        RetainedFunction {
            parameters: Vec::new(),
            body,
            environment: Environment::new(),
        },
    );

    invoke_named_with_effects_and_budget(
        wrapper_name,
        &admitted,
        &Environment::new(),
        limits,
        effects,
        budget,
    )
}

pub(super) struct ActivationRelationEffectHandler<'a, 'runtime> {
    pub(super) activation: &'a TransactionActivation<'runtime>,
}

impl EffectHandler for ActivationRelationEffectHandler<'_, '_> {
    fn handle(
        &mut self,
        _callee: &Expr,
        _arguments: &[Value],
    ) -> Result<Option<Value>, EvaluationError> {
        Ok(None)
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
        let start = relation_cursor_start(after)?;
        let candidates = self
            .activation
            .candidate_relation(&source.to_owned())
            .map_err(|error| transaction_error(table_error_code(error)))?;
        let mut rows = Vec::new();
        let mut next = None;
        for (index, (_, row)) in candidates.enumerate().skip(start) {
            if rows.len() == max_rows {
                next = Some((index as u64).to_be_bytes().to_vec());
                break;
            }
            budget.debit(1)?;
            rows.push(row);
        }
        Ok(Some(RelationPage { rows, next }))
    }
}
/// Binds a candidate validator to the repository CWD generation observed for
/// the activation capture. The runtime compares this value again at commit, so
/// CWD drift after capture fails closed before any candidate mutation.
pub(super) fn activation_cwd_generation(
    repository: &Repository,
    context: &orna_runtime::RuntimeActivationContext,
) -> Result<orna_repository::CwdGeneration, RuntimeError> {
    let runtime =
        u64::try_from(context.capture().generation()).map_err(|_| RuntimeError::RecoveryInvalid)?;
    repository
        .cwd_generation(orna_repository::RuntimeGeneration::new(runtime))
        .map_err(|_| RuntimeError::InvalidTableMutation)
}
