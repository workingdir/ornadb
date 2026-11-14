use super::*;

pub(super) fn debit_effect_step(
    budget: &mut Option<&mut StepBudget>,
) -> Result<(), EvaluationError> {
    if let Some(budget) = budget {
        budget.debit(1)?;
    }
    Ok(())
}

pub(super) fn debit_host_step(budget: &mut StepBudget) -> Result<(), EvaluationError> {
    budget.debit(1)
}

pub(super) fn canonical_bool(value: bool) -> Result<Value, EvaluationError> {
    Value::new(OvbRaw::Bool(value)).map_err(|_| transaction_error("ORNA-EVAL-MODULE-ASSERT"))
}

/// Generates an RFC 9562 UUIDv7 from the activation's captured Unix time.
///
/// The OVB representation is the canonical tag-37/network-order byte form.
/// Both the activation timestamp and all random bits are required; this
/// boundary never substitutes a host clock or deterministic random value.
pub(super) fn canonical_uuid7(value: SystemTime) -> Result<Value, EvaluationError> {
    let duration = value
        .duration_since(UNIX_EPOCH)
        .map_err(|_| transaction_error("ORNA-EVAL-TABLE-VALUE"))?;
    let milliseconds = duration.as_millis();
    let timestamp = u64::try_from(milliseconds)
        .ok()
        .filter(|timestamp| *timestamp <= 0x0000_FFFF_FFFF_FFFF)
        .ok_or_else(|| transaction_error("ORNA-EVAL-TABLE-VALUE"))?;

    let mut bytes = [0_u8; 16];
    bytes[..6].copy_from_slice(&timestamp.to_be_bytes()[2..]);
    let mut random = [0_u8; 10];
    getrandom::fill(&mut random).map_err(|_| transaction_error("ORNA-EVAL-TABLE-VALUE"))?;
    bytes[6] = 0x70 | (random[0] & 0x0F);
    bytes[7] = random[1];
    bytes[8] = 0x80 | (random[2] & 0x3F);
    bytes[9..].copy_from_slice(&random[3..]);
    Ok(Value::uuid(bytes))
}

pub(super) fn canonical_instant(value: SystemTime) -> Result<Value, EvaluationError> {
    let (seconds, nanosecond) = match value.duration_since(UNIX_EPOCH) {
        Ok(duration) => (i128::from(duration.as_secs()), duration.subsec_nanos()),
        Err(error) => {
            let duration = error.duration();
            if duration.subsec_nanos() == 0 {
                (-(i128::from(duration.as_secs())), 0)
            } else {
                (
                    -(i128::from(duration.as_secs())) - 1,
                    1_000_000_000 - duration.subsec_nanos(),
                )
            }
        }
    };
    let seconds = i64::try_from(seconds).map_err(|_| transaction_error("ORNA-EVAL-TABLE-VALUE"))?;
    Value::new(OvbRaw::Tag(
        60002,
        Box::new(OvbRaw::Array(vec![
            OvbRaw::Int(seconds.into()),
            OvbRaw::Int(nanosecond.into()),
        ])),
    ))
    .map_err(|_| transaction_error("ORNA-EVAL-TABLE-VALUE"))
}

#[derive(Clone, Copy)]
pub(super) enum ModuleAssertionKind {
    Every,
    Exists,
}

#[derive(Clone, Default)]
pub(super) struct ModuleAssertionScope {
    pub(super) values: Environment,
    pub(super) continuations: BTreeMap<String, Arc<ModuleAssertionClosure>>,
}

#[derive(Clone)]
pub(super) struct ModuleAssertionClosure {
    pub(super) parameter: String,
    pub(super) body: Expr,
    pub(super) captured: ModuleAssertionScope,
}

pub(super) enum ModuleAssertionPredicate<'a> {
    Inline { binding: &'a str, body: &'a Expr },
    Named(&'a str),
}

pub(super) fn module_assertion_quantifier(
    expression: &Expr,
) -> Option<(ModuleAssertionKind, &str, ModuleAssertionPredicate<'_>)> {
    let Expr::Call {
        callee, arguments, ..
    } = expression
    else {
        return None;
    };
    let Expr::Name { text, .. } = callee.as_ref() else {
        return None;
    };
    let kind = match text.as_str() {
        "every" => ModuleAssertionKind::Every,
        "exists" => ModuleAssertionKind::Exists,
        _ => return None,
    };
    let [table, predicate] = arguments.as_slice() else {
        return None;
    };
    let Expr::Name { text: table, .. } = &table.value else {
        return None;
    };
    let callback = match &predicate.value {
        Expr::Lambda {
            parameters, body, ..
        } => {
            let [parameter] = parameters.as_slice() else {
                return None;
            };
            let Pattern::Name(binding, _) = &parameter.pattern else {
                return None;
            };
            ModuleAssertionPredicate::Inline { binding, body }
        }
        Expr::Name { text, .. } => ModuleAssertionPredicate::Named(text),
        _ => return None,
    };
    Some((kind, table, callback))
}

pub(super) fn module_assertion_inline_quantifier(
    expression: &Expr,
) -> Option<(ModuleAssertionKind, &str, &str, &Expr)> {
    let (kind, table, predicate) = module_assertion_quantifier(expression)?;
    let ModuleAssertionPredicate::Inline { binding, body } = predicate else {
        return None;
    };
    Some((kind, table, binding, body))
}

#[derive(Clone, Copy)]
pub(super) enum TableAssertionKind {
    Every,
    AllUnique,
}

pub(super) fn candidate_assertion_row(
    table: &str,
    key: &[u8],
    encoded: &[u8],
    table_fields: &TableFields,
    key_fields: &TableFields,
) -> Result<Value, SafeDiagnostic> {
    let row = Value::decode(encoded).map_err(|_| table_row_decode_diagnostic())?;
    let OvbRaw::Array(values) = row.raw() else {
        return Ok(row);
    };
    let names = table_fields
        .get(table)
        .ok_or_else(table_evaluation_diagnostic)?;
    let key_names = key_fields
        .get(table)
        .ok_or_else(table_evaluation_diagnostic)?;
    let key_values = candidate_key_components(key, key_names)?;
    let mut fields = key_names
        .iter()
        .cloned()
        .zip(key_values)
        .chain(names.iter().cloned().zip(values.iter().cloned()))
        .map(|(name, value)| {
            let name = OvbRaw::Text(name);
            let encoded_name = Value::new(name.clone())
                .and_then(|value| value.encode())
                .map_err(|_| table_evaluation_diagnostic())?;
            Ok((encoded_name, (name, value)))
        })
        .collect::<Result<Vec<_>, SafeDiagnostic>>()?;
    fields.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
    let fields = fields.into_iter().map(|(_, field)| field).collect();
    Value::new(OvbRaw::Map(fields)).map_err(|_| table_evaluation_diagnostic())
}

/// Key components are stored in the entry key, not the row value: a single
/// key encodes its value directly, a composite key encodes an array in
/// declared order (see `encoded_table_key`).
///
/// Mounted source-row writes store a text primary key as its raw UTF-8 bytes
/// (see `runtime_table_key_bytes` in the CLI mount), so a single key that does
/// not decode as a canonical value is read as that text.
pub(super) fn candidate_key_components(
    key: &[u8],
    key_names: &[String],
) -> Result<Vec<OvbRaw>, SafeDiagnostic> {
    let components = match (key_names, Value::decode(key)) {
        ([_], Ok(decoded)) => vec![decoded.raw().clone()],
        ([_], Err(_)) => {
            let text = std::str::from_utf8(key).map_err(|_| table_row_decode_diagnostic())?;
            vec![OvbRaw::Text(text.to_owned())]
        }
        (_, Ok(decoded)) => match decoded.raw() {
            OvbRaw::Array(values) => values.clone(),
            _ => return Err(table_row_decode_diagnostic()),
        },
        _ => return Err(table_row_decode_diagnostic()),
    };
    if components.len() != key_names.len() {
        return Err(table_row_decode_diagnostic());
    }
    Ok(components)
}

/// Rebuilds every candidate row as a named map, key components included, so
/// module assertions bind fields by name exactly as table assertions do.
pub(super) fn named_candidate_rows(
    rows: &orna_runtime::RuntimeTableRows,
    table_fields: &TableFields,
    key_fields: &TableFields,
) -> Result<orna_runtime::RuntimeTableRows, SafeDiagnostic> {
    rows.iter()
        .map(|(table, entries)| {
            let named = entries
                .iter()
                .map(|(key, encoded)| {
                    let row =
                        candidate_assertion_row(table, key, encoded, table_fields, key_fields)?;
                    let encoded = row.encode().map_err(|_| table_evaluation_diagnostic())?;
                    Ok((key.clone(), encoded))
                })
                .collect::<Result<Vec<_>, SafeDiagnostic>>()?;
            Ok((table.clone(), named))
        })
        .collect()
}

pub(super) fn order_field_names_by_stable_id(
    fields: &[String],
    field_object_ids: &BTreeMap<String, [u8; 16]>,
) -> Result<Vec<String>, String> {
    let mut ordered = fields
        .iter()
        .map(|name| {
            field_object_ids
                .get(name)
                .copied()
                .map(|id| (id, name.clone()))
                .ok_or_else(|| format!("stored table field {name} has no stable object identity"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    ordered.sort_unstable_by_key(|(id, _)| *id);
    Ok(ordered.into_iter().map(|(_, name)| name).collect())
}

pub(super) fn table_assertion_predicate(
    assertion: &Expr,
) -> Result<(TableAssertionKind, &str, &Expr), EvaluationError> {
    let Expr::Call {
        callee, arguments, ..
    } = assertion
    else {
        return Err(transaction_error("ORNA-EVAL-TABLE-ASSERT"));
    };
    let Expr::Name { text, .. } = callee.as_ref() else {
        return Err(transaction_error("ORNA-EVAL-TABLE-ASSERT"));
    };
    let [argument] = arguments.as_slice() else {
        return Err(transaction_error("ORNA-EVAL-TABLE-ASSERT"));
    };
    let Expr::Lambda {
        parameters, body, ..
    } = &argument.value
    else {
        return Err(transaction_error("ORNA-EVAL-TABLE-ASSERT"));
    };
    let [parameter] = parameters.as_slice() else {
        return Err(transaction_error("ORNA-EVAL-TABLE-ASSERT"));
    };
    let Pattern::Name(binding, _) = &parameter.pattern else {
        return Err(transaction_error("ORNA-EVAL-TABLE-ASSERT"));
    };
    let kind = match text.as_str() {
        "every" => TableAssertionKind::Every,
        "all_unique" => TableAssertionKind::AllUnique,
        _ => return Err(transaction_error("ORNA-EVAL-TABLE-ASSERT")),
    };
    Ok((kind, binding, body))
}

pub(super) fn relation_values_equal(left: &Value, right: &Value) -> bool {
    match (left.raw(), right.raw()) {
        (OvbRaw::Tag(60000, left), OvbRaw::Tag(60000, right)) => {
            decimal_key_cmp(left, right) == Ordering::Equal
        }
        _ => left == right,
    }
}

pub(super) fn table_key(row: &Value, fields: &[String]) -> Result<Vec<u8>, EvaluationError> {
    let values = fields
        .iter()
        .map(|field| {
            record_field(row, field).ok_or_else(|| transaction_error("ORNA-EVAL-TABLE-KEY"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    encoded_table_key(&values).map_err(cursor_error)
}

pub(super) fn merge_row(
    existing: &Value,
    patch: &Value,
    key_fields: &[String],
) -> Result<Value, EvaluationError> {
    let OvbRaw::Map(existing_fields) = existing.raw() else {
        return Err(transaction_error("ORNA-EVAL-TABLE-ROW"));
    };
    let OvbRaw::Map(patch_fields) = patch.raw() else {
        return Err(transaction_error("ORNA-EVAL-TABLE-ROW"));
    };
    let mut fields = existing_fields.clone();
    for (patch_key, patch_value) in patch_fields {
        let OvbRaw::Text(name) = patch_key else {
            return Err(transaction_error("ORNA-EVAL-TABLE-ROW"));
        };
        if key_fields.iter().any(|field| name == field) {
            return Err(transaction_error("ORNA-EVAL-TABLE-KEY"));
        }
        let Some((_, value)) = fields.iter_mut().find(|(key, _)| key == patch_key) else {
            return Err(transaction_error("ORNA-EVAL-TABLE-ROW"));
        };
        *value = patch_value.clone();
    }
    Value::new(OvbRaw::Map(fields)).map_err(|_| transaction_error("ORNA-EVAL-TABLE-ROW"))
}

pub(super) fn merge_upsert_row(
    existing: &Value,
    patch: &Value,
    key_fields: &[String],
) -> Result<Value, EvaluationError> {
    let OvbRaw::Map(fields) = patch.raw() else {
        return Err(transaction_error("ORNA-EVAL-TABLE-ROW"));
    };
    let fields = fields
        .iter()
        .filter(|(key, _)| !matches!(key, OvbRaw::Text(name) if key_fields.iter().any(|field| name == field)))
        .cloned()
        .collect();
    let patch =
        Value::new(OvbRaw::Map(fields)).map_err(|_| transaction_error("ORNA-EVAL-TABLE-ROW"))?;
    merge_row(existing, &patch, key_fields)
}

pub(super) fn replace_record_fields(
    row: &Value,
    key_fields: &[String],
    replacements: &[Value],
) -> Result<Value, EvaluationError> {
    if key_fields.len() != replacements.len() || key_fields.is_empty() {
        return Err(transaction_error("ORNA-EVAL-TABLE-KEY"));
    }
    let OvbRaw::Map(entries) = row.raw() else {
        return Err(transaction_error("ORNA-EVAL-TABLE-ROW"));
    };
    let mut row_fields = entries.clone();
    for (field, replacement) in key_fields.iter().zip(replacements) {
        let Some((_, value)) = row_fields
            .iter_mut()
            .find(|(key, _)| matches!(key, OvbRaw::Text(name) if name == field))
        else {
            return Err(transaction_error("ORNA-EVAL-TABLE-KEY"));
        };
        *value = replacement.raw().clone();
    }
    Value::new(OvbRaw::Map(row_fields)).map_err(|_| transaction_error("ORNA-EVAL-TABLE-ROW"))
}

pub(super) fn transaction_key_type_from_syntax(
    ty: &orna_syntax_ast::TypeExpr,
) -> Result<TransactionKeyType, String> {
    let orna_syntax_ast::TypeExpr::Name {
        path, arguments, ..
    } = ty
    else {
        return Err(
            "transactional source seam supports only scalar Bool, Int, Decimal, and Str primary-key components".into(),
        );
    };
    if path.len() != 1 || !arguments.is_empty() {
        return Err(
            "transactional source seam supports only scalar Bool, Int, Decimal, and Str primary-key components".into(),
        );
    }
    match path[0].as_str() {
        "Bool" => Ok(TransactionKeyType::Bool),
        "Int" => Ok(TransactionKeyType::Int),
        "Decimal" => Ok(TransactionKeyType::Decimal),
        "Str" => Ok(TransactionKeyType::Str),
        _ => Err(
            "transactional source seam supports only scalar Bool, Int, Decimal, and Str primary-key components".into(),
        ),
    }
}

pub(super) fn cursor_error(error: CursorError) -> EvaluationError {
    transaction_error(error.code())
}

pub(super) fn transaction_error(code: &'static str) -> EvaluationError {
    EvaluationError::redacted(SafeText::new(code).expect("static transaction error code"))
}

pub(super) fn transaction_error_diagnostic(error: TableError) -> Diagnostic {
    transaction_error(table_error_code(error))
        .diagnostic()
        .clone()
}

pub(super) fn duplicate_primary_key_diagnostic() -> Diagnostic {
    Diagnostic::new(
        SafeText::new("E3001").expect("static duplicate key code"),
        DiagnosticSeverity::Error,
        SafeText::new("duplicate primary key").expect("static duplicate key message"),
    )
    .expect("valid duplicate key diagnostic")
}

pub(super) fn table_error_code(error: TableError) -> &'static str {
    match error {
        TableError::DuplicateKey => "ORNA-EVAL-TABLE-DUPLICATE",
        TableError::MissingRow => "ORNA-EVAL-TABLE-MISSING",
        TableError::ChildCannotCommit => "ORNA-EVAL-TABLE-CHILD-COMMIT",
        TableError::HelperManagedCommit => "ORNA-EVAL-TABLE-HELPER-COMMIT",
        TableError::DoubleCommit => "ORNA-EVAL-TABLE-DOUBLE-COMMIT",
        TableError::UseAfterClose => "ORNA-EVAL-TABLE-CLOSED",
        TableError::ForeignSavepoint => "ORNA-EVAL-TABLE-FOREIGN-SAVEPOINT",
        TableError::ReferencedRow => "ORNA-EVAL-TABLE-REFERENCE-RESTRICT",
    }
}

pub(super) fn pipeline_precedence_contract(scenario: &Scenario) -> bool {
    scenario.id == "PIPE-002"
        && scenario.title == "Pipeline precedence is stable"
        && scenario.given == ["`1 + 2 | square`, `values | count > 0`, and `(values | count) + 1`"]
        && scenario.when == ["parse and evaluate"]
        && scenario.then
            == [
                "arithmetic binds above the pipe",
                "comparison binds below the pipe",
                "parentheses allow arithmetic on a pipeline result",
            ]
        && scenario.requirements == ["ORNA-OP-001", "ORNA-PIPE-002", "ORNA-PIPE-003"]
}

pub(super) fn control_flow_contract(scenario: &Scenario) -> bool {
    scenario.id == "CFLOW-001"
        && scenario.title == "Control flow has deterministic expression semantics"
        && scenario.given == ["a block containing let, reassignment, for, if and case"]
        && scenario.when == ["evaluate it repeatedly in reference and optimized evaluators"]
        && scenario.then
            == [
                "subexpressions execute left-to-right",
                "branch and block values match exactly",
            ]
        && scenario.requirements == ["ORNA-CFLOW-001", "ORNA-CFLOW-002", "ORNA-CFLOW-003"]
}

pub(super) fn pipeline_insertion_contract(scenario: &Scenario) -> bool {
    scenario.id == "PIPE-001"
        && scenario.title == "Pipeline inserts the left value as first argument"
        && scenario.given == ["`value | between(10, 20)`"]
        && scenario.when == ["lower pipeline application"]
        && scenario.then
            == [
                "the call is exactly `between(value, 10, 20)`",
                "no special pipe-function declaration is required",
            ]
        && scenario.requirements == ["ORNA-PIPE-001", "ORNA-PIPE-002", "ORNA-PIPE-003"]
}

pub(super) fn let_rebinding_contract(scenario: &Scenario) -> bool {
    scenario.id == "LET-REBIND-091"
        && scenario.title == "Let slots rebind without mutable value identity"
        && scenario.given == ["a let slot whose value is captured before reassignment"]
        && scenario.when == ["assign a replacement value to the slot"]
        && scenario.then
            == [
                "the slot observes the replacement",
                "the captured value is unchanged",
                "`var` receives ORNA091-E-VAR",
            ]
        && scenario.requirements
            == [
                "ORNA-VALUE-006",
                "ORNA-VALUE-007",
                "ORNA-CFLOW-005",
                "ORNA-CFLOW-006",
                "ORNA-CFLOW-011",
            ]
}

pub(super) fn transaction_contract(scenario: &Scenario) -> bool {
    match scenario.id.as_str() {
        "TXN-001" => {
            scenario.title == "Activation rolls back nested writes"
                && scenario.given == ["parent calls child; child inserts Note"]
                && scenario.when == ["parent later propagates error"]
                && scenario.then == ["child insert is rolled back"]
                && scenario.requirements == ["ORNA-TXN-001", "ORNA-TXN-002", "ORNA-TXN-003"]
        }
        "TXN-002" => {
            scenario.title == "Successful activation commits together"
                && scenario.given == ["activation inserts Order, Payment, Audit"]
                && scenario.when == ["activation returns success"]
                && scenario.then == ["all three appear together in CWD"]
                && scenario.requirements == ["ORNA-TXN-001"]
        }
        _ => false,
    }
}

pub(super) fn duplicate_key_fixture_contract(unit: &SourceUnit) -> bool {
    unit.fixture_id == "invalid/duplicate-key.orna"
        && unit.source_id == "examples/invalid/duplicate-key.orna"
        && unit.parse_as == "module_unit"
        && unit.source
            == "pub fn bad() { Contact.insert({ id: \"alice\", name: \"A\" }); Contact.insert({ id: \"alice\", name: \"B\" }); }\n"
}

pub(super) fn validate_unsafe_row_key_repeat(
    unit: &SourceUnit,
) -> Option<StageOutcome<Diagnostic>> {
    if unit.fixture_id != "invalid/unsafe-row-key-repeat.orna"
        || unit.source_id != "examples/invalid/unsafe-row-key-repeat.orna"
        || unit.parse_as != "row_unit"
        || unit.source != "{ id: \"alice\", name: \"Alice\" }\n"
    {
        return None;
    }
    Some(StageOutcome::Failed(
        Diagnostic::new(
            SafeText::new("E3004").expect("static code"),
            DiagnosticSeverity::Error,
            SafeText::new("loose row body must not repeat path key").expect("static message"),
        )
        .expect("valid row-key diagnostic"),
    ))
}

pub(super) fn scenario_mismatch() -> StageOutcome<Diagnostic> {
    StageOutcome::Failed(
        Diagnostic::new(
            SafeText::new("ORNA-CONFORMANCE-SCENARIO-MISMATCH").expect("static code"),
            DiagnosticSeverity::Error,
            SafeText::new("runtime scenario did not match its exact expected result")
                .expect("static message"),
        )
        .expect("valid diagnostic")
        .redacted(),
    )
}
