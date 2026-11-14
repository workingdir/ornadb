use super::*;

/// Why project transaction admission skips a stream root. The CLI routes the
/// skipped stage to the explicit stream seam through
/// [`StageOutcome::is_stream_root_admission_skip`], never by copying this text.
pub(crate) const PROJECT_STREAM_ROOT_ADMISSION_REASON: &str = "project transaction admission does not run stream roots; use the explicit finite-list stream seam";

pub(super) fn stream_handler_diagnostic() -> SafeDiagnostic {
    SafeDiagnostic {
        code: DiagnosticCode::DecodeRejected,
        class: DiagnosticClass::Permanent,
    }
}

pub(super) fn table_assertion_diagnostic() -> SafeDiagnostic {
    SafeDiagnostic {
        code: DiagnosticCode::TableAssertionFalse,
        class: DiagnosticClass::Permanent,
    }
}

/// Names an assertion that refused a commit or a stream item: its owner kind and
/// name, and the source span of its declaration (ORNA-A091-009, ORNA-ASSERT-051).
pub(super) fn assertion_site(assertion: &AdmittedAssertion) -> String {
    let kind = match assertion.owner_kind {
        AssertionOwnerKind::Executable => "executable",
        AssertionOwnerKind::RefinedType => "refined type",
        AssertionOwnerKind::Table => "table",
        AssertionOwnerKind::Module => "module",
    };
    let span = &assertion.declaration_span;
    format!(
        "{kind} {} assertion at {}{}..{}",
        assertion.owner_name,
        span.file().unwrap_or("source"),
        span.start,
        span.end,
    )
}

pub(super) fn table_evaluation_diagnostic() -> SafeDiagnostic {
    SafeDiagnostic {
        code: DiagnosticCode::ExecutionRejected,
        class: DiagnosticClass::Permanent,
    }
}

pub(super) fn table_row_decode_diagnostic() -> SafeDiagnostic {
    SafeDiagnostic {
        code: DiagnosticCode::DecodeRejected,
        class: DiagnosticClass::Permanent,
    }
}

/// True only when a table assertion evaluated to false (ORNA-A091-009). Other
/// validator failures are evaluation or decode errors with their own codes.
pub fn is_table_assertion_false(diagnostic: SafeDiagnostic) -> bool {
    diagnostic.code == DiagnosticCode::TableAssertionFalse
}

pub(super) fn stream_error_diagnostic(error: orna_runtime::StreamStepError) -> Diagnostic {
    Diagnostic::new(
        SafeText::new("ORNA-LIST-STREAM").expect("static code"),
        DiagnosticSeverity::Error,
        SafeText::new(format!("literal list stream failed: {error}"))
            .expect("bounded stream error"),
    )
    .expect("valid diagnostic")
    .redacted()
}

pub(super) fn stream_delivery_failure_diagnostic(failure: &FailureRecord) -> Diagnostic {
    Diagnostic::new(
        SafeText::new("ORNA-LIST-STREAM-DELIVERY").expect("static code"),
        DiagnosticSeverity::Error,
        SafeText::new("literal list stream delivery was retained after activation rollback")
            .expect("static message"),
    )
    .expect("valid diagnostic")
    .with_cause(stream_failure_cause(failure.diagnostic.code))
    .redacted()
}

/// A recorded item refusal names the owning source (ORNA-ASSERT-051) and takes
/// precedence over the generic delivery diagnostic. An assertion refusal is
/// named by its owner kind and name, source span, item position and source
/// identity; the code stays ORNA-LIST-STREAM-DELIVERY.
pub(super) fn stream_failure_diagnostic(
    handler: &mut ListTableHandler,
    failure: &FailureRecord,
) -> Diagnostic {
    if let Some(refusal) = handler.refusal.take() {
        return refusal;
    }
    let site = handler
        .assertion_refusal
        .lock()
        .expect("assertion refusal slot is not poisoned")
        .take();
    match site {
        Some(site) if is_assertion_refusal_code(failure.diagnostic.code) => {
            stream_assertion_refusal_diagnostic(&site, &handler.bridge.source_identity, failure)
        }
        _ => stream_delivery_failure_diagnostic(failure),
    }
}

/// True for the codes an assertion produces when it refuses or cannot be
/// evaluated. A decode failure of a stored row is not an assertion refusal and
/// keeps the generic diagnostic.
pub(super) fn is_assertion_refusal_code(code: DiagnosticCode) -> bool {
    matches!(
        code,
        DiagnosticCode::TableAssertionFalse | DiagnosticCode::ExecutionRejected
    )
}

pub(super) fn stream_assertion_refusal_diagnostic(
    site: &str,
    source_identity: &str,
    failure: &FailureRecord,
) -> Diagnostic {
    let message = SafeText::new(format!(
        "literal list stream item at position {} was refused by {site} (source {source_identity})",
        failure.identity.0.position.token.as_str(),
    ))
    .unwrap_or_else(|_| {
        SafeText::new("literal list stream item was refused").expect("static message")
    });
    Diagnostic::new(
        SafeText::new("ORNA-LIST-STREAM-DELIVERY").expect("static code"),
        DiagnosticSeverity::Error,
        SafeText::new("literal list stream delivery was retained after activation rollback")
            .expect("static message"),
    )
    .expect("valid diagnostic")
    .with_cause(stream_failure_cause(failure.diagnostic.code))
    .redacted_with_message(message)
}

/// The runtime's typed failure code, carried as a non-catalogue label for the
/// `--debug` detail. The spec does not catalogue these variants, so the label
/// deliberately does not use the `ORNA-` prefix and promotes no code.
pub(super) fn stream_failure_cause(code: DiagnosticCode) -> Diagnostic {
    let label = match code {
        DiagnosticCode::ProviderUnavailable => "stream-failure:provider-unavailable",
        DiagnosticCode::DecodeRejected => "stream-failure:decode-rejected",
        DiagnosticCode::ExecutionRejected => "stream-failure:execution-rejected",
        DiagnosticCode::Cancelled => "stream-failure:cancelled",
        DiagnosticCode::Internal => "stream-failure:internal",
        DiagnosticCode::TableAssertionFalse => "stream-failure:table-assertion-false",
        DiagnosticCode::StaleBaseline => "stream-failure:stale-baseline",
    };
    Diagnostic::new(
        SafeText::new(label).expect("static label"),
        DiagnosticSeverity::Error,
        SafeText::redacted(),
    )
    .expect("valid diagnostic")
    .redacted()
}

pub(super) fn stream_cancelled_diagnostic() -> Diagnostic {
    Diagnostic::new(
        SafeText::new("ORNA-LIST-STREAM-CANCELLED").expect("static code"),
        DiagnosticSeverity::Error,
        SafeText::new("literal list stream delivery was cancelled").expect("static message"),
    )
    .expect("valid diagnostic")
    .redacted()
}

pub(super) fn unexpected_stream_boundary_diagnostic() -> Diagnostic {
    Diagnostic::new(
        SafeText::new("ORNA-LIST-STREAM-BOUNDARY").expect("static code"),
        DiagnosticSeverity::Error,
        SafeText::new("literal list stream ended at an unexpected runner boundary")
            .expect("static message"),
    )
    .expect("valid diagnostic")
    .redacted()
}

pub(super) fn unexpected_stream_boundary_safe_diagnostic() -> SafeDiagnostic {
    SafeDiagnostic {
        code: DiagnosticCode::ExecutionRejected,
        class: DiagnosticClass::Permanent,
    }
}

pub(super) fn stream_runtime_safe_diagnostic() -> SafeDiagnostic {
    SafeDiagnostic {
        code: DiagnosticCode::Internal,
        class: DiagnosticClass::Transient,
    }
}

pub(super) fn admit_list_stream_source(
    unit: &SourceUnit,
    limits: EvaluatorLimits,
    entry: &str,
    catalogue: &Catalogue,
) -> Result<ListStreamBridge, AdmissionFailure> {
    let AdmittedTransaction {
        functions,
        key_fields,
        float_fields,
        table_fields,
        mut table_assertions,
        module_assertions,
        ..
    } = admit_transaction_source(unit, limits, entry, catalogue)?;
    if key_fields.len() != 1 {
        return Err(Box::new(StageOutcome::Skipped {
            reason: "literal list stream bridge requires one explicit-key table".into(),
        }));
    }
    let parsed = parse_module(&unit.source);
    let body = parsed
        .value
        .items
        .iter()
        .find_map(|item| match &item.declaration {
            Declaration::Function { signature, body } if signature.name == entry => Some(body),
            _ => None,
        })
        .ok_or_else(|| {
            Box::new(StageOutcome::Skipped {
                reason: "configured list stream entry function is not present".into(),
            })
        })?;
    let (values, source_identity, parameter, table, insert_row) = literal_stream_pipeline(body)
        .ok_or_else(|| Box::new(StageOutcome::Skipped {
            reason: "literal list stream bridge requires Stream.from_list piped to one for_each insert body".into(),
        }))?;
    let (table_name, key_fields) = key_fields.into_iter().next().expect("one table checked");
    applicable_module_assertions(&table_name, &module_assertions)
        .map_err(|reason| Box::new(StageOutcome::Skipped { reason }))?;
    if key_fields.is_empty() {
        return Err(Box::new(StageOutcome::Skipped {
            reason: "literal list stream bridge requires an explicit table key".into(),
        }));
    }
    if table != table_name {
        return Err(Box::new(StageOutcome::Skipped {
            reason: "literal list stream insert must target the declared table".into(),
        }));
    }
    let assertion_functions = lower_relation_bindings(
        &functions,
        &BTreeMap::from([(table_name.clone(), key_fields.clone())]),
        &float_fields,
        &table_fields,
    );
    let table_assertions = table_assertions.remove(&table_name).unwrap_or_default();
    let payloads: Vec<Vec<u8>> = values
        .iter()
        .map(literal_value)
        .collect::<Option<Vec<_>>>()
        .and_then(|values| {
            values
                .into_iter()
                .map(|value| value.encode().ok())
                .collect::<Option<Vec<_>>>()
        })
        .ok_or_else(|| {
            Box::new(StageOutcome::Skipped {
                reason: "literal list stream values must be canonical literals".into(),
            })
        })?;
    Ok(ListStreamBridge {
        source_identity: list_source_identity(&source_identity, &payloads),
        entry: entry.into(),
        producer_entry: entry.into(),
        consumer_principal: "conformance".into(),
        consumer_root: unit.source_id.clone(),
        consumer_binding: "from_list".into(),
        partition: None,
        table: table_name,
        key_fields: key_fields.fields.clone(),
        parameter,
        insert_row: insert_row.clone(),
        table_assertions,
        module_assertions,
        assertion_functions,
        payloads,
    })
}

/// Resolves the admitted root's producer expression to canonical list values.
/// A direct `Stream.from_list` is retained for inline project roots; a bare
/// zero-argument name resolves only within the root's qualified module.
pub(super) fn project_stream_source<'a>(
    functions: &'a Functions,
    root_entry: &str,
    lhs: &'a Expr,
) -> Result<(&'a [Expr], String, String), AdmissionFailure> {
    let Expr::Call {
        callee, arguments, ..
    } = lhs
    else {
        return Err(Box::new(StageOutcome::Skipped {
            reason: "project list stream producer must be a zero-argument sibling call".into(),
        }));
    };
    if qualified_expr_path(callee).as_deref() == Some(["Stream", "from_list"].as_slice()) {
        let (values, source_identity) = literal_list_source(lhs).ok_or_else(|| {
            Box::new(StageOutcome::Skipped {
                reason: "project list stream producer must return literal Stream.from_list".into(),
            })
        })?;
        return Ok((values, source_identity, root_entry.into()));
    }
    let Expr::Name { text: producer, .. } = callee.as_ref() else {
        return Err(Box::new(StageOutcome::Skipped {
            reason: "project list stream producer must be a zero-argument sibling call".into(),
        }));
    };
    if !arguments.is_empty() {
        return Err(Box::new(StageOutcome::Skipped {
            reason: "project list stream producer must have canonical zero-argument bindings"
                .into(),
        }));
    }
    let (namespace, _) = root_entry.rsplit_once('.').ok_or_else(|| {
        Box::new(StageOutcome::Skipped {
            reason: "project stream root must have a module namespace".into(),
        })
    })?;
    let producer_entry = format!("{namespace}.{producer}");
    let producer = functions.get(&producer_entry).ok_or_else(|| {
        Box::new(StageOutcome::Skipped {
            reason: "project list stream producer must resolve in the root module".into(),
        })
    })?;
    if !producer.parameters.is_empty() {
        return Err(Box::new(StageOutcome::Skipped {
            reason: "project list stream producer must have canonical zero-argument bindings"
                .into(),
        }));
    }
    let (values, source_identity) = literal_list_source(&producer.body).ok_or_else(|| {
        Box::new(StageOutcome::Skipped {
            reason: "project list stream producer must return literal Stream.from_list".into(),
        })
    })?;
    Ok((values, source_identity, producer_entry))
}

/// Admits exactly one project root of the form `Stream.from_list(...) |
/// for_each(...)` or `producer() | for_each(...)`, where `producer` is a
/// zero-argument sibling function returning a literal `Stream.from_list`.
/// The project function map is already namespace-qualified by
/// `admit_transaction_project`, so this does not combine module text.
pub(super) fn admit_project_list_stream(
    _project: &ProjectUnit,
    admitted: &AdmittedTransaction,
    root_entry: &str,
    identity: RuntimeIdentity,
) -> Result<ListStreamBridge, AdmissionFailure> {
    let AdmittedTransaction {
        functions,
        key_fields,
        float_fields,
        table_fields,
        table_assertions,
        module_assertions,
        ..
    } = admitted;
    let root = functions.get(root_entry).ok_or_else(|| {
        Box::new(StageOutcome::Skipped {
            reason: "configured qualified project stream root is not present".into(),
        })
    })?;
    if !root.parameters.is_empty() {
        return Err(Box::new(StageOutcome::Skipped {
            reason: "project list stream root must have canonical zero-argument bindings".into(),
        }));
    }
    let Expr::Block {
        statements,
        tail: None,
        ..
    } = &root.body
    else {
        return Err(Box::new(StageOutcome::Skipped {
            reason: "project list stream root must contain one producer pipeline".into(),
        }));
    };
    let [Statement::Expression { value, .. }] = statements.as_slice() else {
        return Err(Box::new(StageOutcome::Skipped {
            reason: "project list stream root must contain one producer pipeline".into(),
        }));
    };
    let Expr::Binary { lhs, op, rhs, .. } = value else {
        return Err(Box::new(StageOutcome::Skipped {
            reason: "project list stream root must pipe a producer into for_each".into(),
        }));
    };
    if op != "|" {
        return Err(Box::new(StageOutcome::Skipped {
            reason: "project list stream root must pipe a producer into for_each".into(),
        }));
    }
    let (values, source_identity, producer_entry) =
        project_stream_source(functions, root_entry, lhs)?;
    let (parameter, table, insert_row) = list_for_each_body(rhs).ok_or_else(|| {
        Box::new(StageOutcome::Skipped {
            reason: "project list stream root must use one for_each table insert body".into(),
        })
    })?;
    let keys = key_fields.get(&table).ok_or_else(|| {
        Box::new(StageOutcome::Skipped {
            reason: "project list stream insert must target a declared keyed table".into(),
        })
    })?;
    applicable_module_assertions(&table, module_assertions)
        .map_err(|reason| Box::new(StageOutcome::Skipped { reason }))?;
    if keys.is_empty() {
        return Err(Box::new(StageOutcome::Skipped {
            reason: "project list stream insert target must have an explicit table key".into(),
        }));
    }
    let assertion_functions =
        lower_relation_bindings(functions, key_fields, float_fields, table_fields);
    let table_assertions = table_assertions.get(&table).cloned().unwrap_or_default();
    let payloads = canonical_list_payloads(values)?;
    let database = identity
        .database_id
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(ListStreamBridge {
        source_identity: list_source_identity(&source_identity, &payloads),
        entry: root_entry.into(),
        producer_entry,
        consumer_principal: format!("database:{database}"),
        consumer_root: "public-function".into(),
        consumer_binding: "arguments:[]".into(),
        partition: None,
        table,
        key_fields: keys.fields.clone(),
        parameter,
        insert_row: insert_row.clone(),
        table_assertions,
        module_assertions: module_assertions.clone(),
        assertion_functions,
        payloads,
    })
}

pub(super) fn canonical_list_payloads(values: &[Expr]) -> Result<Vec<Vec<u8>>, AdmissionFailure> {
    values
        .iter()
        .map(literal_value)
        .collect::<Option<Vec<_>>>()
        .and_then(|values| {
            values
                .into_iter()
                .map(|value| value.encode().ok())
                .collect::<Option<Vec<_>>>()
        })
        .ok_or_else(|| {
            Box::new(StageOutcome::Skipped {
                reason: "literal list stream values must be canonical literals".into(),
            })
        })
}

pub(super) fn literal_list_source(body: &Expr) -> Option<(&[Expr], String)> {
    let Expr::Call {
        callee, arguments, ..
    } = body
    else {
        return None;
    };
    if qualified_expr_path(callee).as_deref() != Some(["Stream", "from_list"].as_slice()) {
        return None;
    }
    let [values, identity] = arguments.as_slice() else {
        return None;
    };
    let Expr::List { elements, .. } = &values.value else {
        return None;
    };
    let (
        Some(name),
        Expr::Literal {
            text: source_identity,
            kind: orna_syntax_ast::LiteralKind::String,
            ..
        },
    ) = (&identity.name, &identity.value)
    else {
        return None;
    };
    let source_identity = literal_string(source_identity)?;
    (name == "source_identity" && Component::new(source_identity.clone()).is_ok())
        .then_some((elements.as_slice(), source_identity))
}

pub(super) fn list_for_each_body(rhs: &Expr) -> Option<(String, String, &Expr)> {
    let Expr::Call {
        callee, arguments, ..
    } = rhs
    else {
        return None;
    };
    if qualified_expr_path(callee).as_deref() != Some(["for_each"].as_slice()) {
        return None;
    }
    let [argument] = arguments.as_slice() else {
        return None;
    };
    let Expr::Lambda {
        parameters, body, ..
    } = &argument.value
    else {
        return None;
    };
    let [parameter] = parameters.as_slice() else {
        return None;
    };
    let Pattern::Name(parameter, _) = &parameter.pattern else {
        return None;
    };
    let Expr::Block {
        statements,
        tail: None,
        ..
    } = body.as_ref()
    else {
        return None;
    };
    let [Statement::Expression { value: insert, .. }] = statements.as_slice() else {
        return None;
    };
    let Expr::Call {
        callee, arguments, ..
    } = insert
    else {
        return None;
    };
    let Expr::Field { base, name, .. } = callee.as_ref() else {
        return None;
    };
    let Expr::Name { text: table, .. } = base.as_ref() else {
        return None;
    };
    let [row] = arguments.as_slice() else {
        return None;
    };
    (name == "insert").then_some((parameter.clone(), table.clone(), &row.value))
}

/// Binds a finite list source name to the complete canonical typed list.
///
/// Length prefixes preserve element boundaries, so different canonical value
/// sequences cannot collide by concatenation before the digest is calculated.
pub(super) fn list_source_identity(name: &str, payloads: &[Vec<u8>]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ORNA-LIST-STREAM-IDENTITY\0");
    for payload in payloads {
        digest.update(
            u64::try_from(payload.len())
                .expect("payload length fits u64")
                .to_be_bytes(),
        );
        digest.update(payload);
    }
    let suffix = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{name}:{suffix}")
}

pub(super) fn project_stream_compat_request(
    identity: RuntimeIdentity,
    initial_digest: [u8; 32],
    root_entry: &str,
    key: &CheckpointKey,
) -> (RequestIdentity, [u8; 32]) {
    pub(super) fn update_text(digest: &mut Sha256, value: &str) {
        digest.update(
            u64::try_from(value.len())
                .expect("compatibility identity text length fits u64")
                .to_be_bytes(),
        );
        digest.update(value.as_bytes());
    }

    pub(super) fn update_material(
        digest: &mut Sha256,
        identity: RuntimeIdentity,
        initial_digest: [u8; 32],
        root_entry: &str,
        key: &CheckpointKey,
    ) {
        digest.update(identity.database_id);
        digest.update(identity.repository_id);
        digest.update(initial_digest);
        update_text(digest, root_entry);
        update_text(digest, key.consumer.canonical().as_str());
        update_text(digest, key.source_format.as_str());
        update_text(digest, key.source.as_str());
        update_text(digest, key.partition_format.as_str());
        if let Some(partition) = &key.partition {
            digest.update([1]);
            update_text(digest, partition.as_str());
        } else {
            digest.update([0]);
        }
        update_text(digest, key.position_format.as_str());
    }

    let mut request_digest = Sha256::new();
    request_digest.update(b"orna-conformance/project-stream-request/v1\0");
    update_material(
        &mut request_digest,
        identity,
        initial_digest,
        root_entry,
        key,
    );
    let request_bytes: [u8; 32] = request_digest.finalize().into();
    let mut request_id = [0; 16];
    request_id.copy_from_slice(&request_bytes[..16]);
    if request_id == [0; 16] {
        request_id[0] = 1;
    }

    let mut fingerprint = Sha256::new();
    fingerprint.update(b"orna-conformance/project-stream-fingerprint/v1\0");
    update_material(&mut fingerprint, identity, initial_digest, root_entry, key);
    (
        RequestIdentity {
            session_id: identity.database_id,
            request_id,
        },
        fingerprint.finalize().into(),
    )
}

pub(super) fn literal_stream_pipeline(
    body: &Expr,
) -> Option<(&[Expr], String, String, String, &Expr)> {
    let Expr::Block {
        statements,
        tail: None,
        ..
    } = body
    else {
        return None;
    };
    let [orna_syntax_ast::Statement::Expression { value, .. }] = statements.as_slice() else {
        return None;
    };
    let Expr::Binary { lhs, op, rhs, .. } = value else {
        return None;
    };
    if op != "|" {
        return None;
    }
    let Expr::Call {
        callee, arguments, ..
    } = lhs.as_ref()
    else {
        return None;
    };
    if qualified_expr_path(callee).as_deref() != Some(["Stream", "from_list"].as_slice()) {
        return None;
    }
    let [values, identity] = arguments.as_slice() else {
        return None;
    };
    let Expr::List { elements, .. } = &values.value else {
        return None;
    };
    let (
        Some(name),
        Expr::Literal {
            text: source_identity,
            kind: orna_syntax_ast::LiteralKind::String,
            ..
        },
    ) = (&identity.name, &identity.value)
    else {
        return None;
    };
    let source_identity = literal_string(source_identity)?;
    if name != "source_identity" || Component::new(source_identity.clone()).is_err() {
        return None;
    }
    let Expr::Call {
        callee, arguments, ..
    } = rhs.as_ref()
    else {
        return None;
    };
    if qualified_expr_path(callee).as_deref() != Some(["for_each"].as_slice()) {
        return None;
    }
    let [argument] = arguments.as_slice() else {
        return None;
    };
    let Expr::Lambda {
        parameters, body, ..
    } = &argument.value
    else {
        return None;
    };
    let [parameter] = parameters.as_slice() else {
        return None;
    };
    let Pattern::Name(parameter, _) = &parameter.pattern else {
        return None;
    };
    let Expr::Block {
        statements,
        tail: None,
        ..
    } = body.as_ref()
    else {
        return None;
    };
    let [orna_syntax_ast::Statement::Expression { value: insert, .. }] = statements.as_slice()
    else {
        return None;
    };
    let Expr::Call {
        callee, arguments, ..
    } = insert
    else {
        return None;
    };
    let Expr::Field { base, name, .. } = callee.as_ref() else {
        return None;
    };
    if name != "insert" {
        return None;
    }
    let Expr::Name { text: table, .. } = base.as_ref() else {
        return None;
    };
    let [row] = arguments.as_slice() else {
        return None;
    };
    Some((
        elements,
        source_identity,
        parameter.clone(),
        table.clone(),
        &row.value,
    ))
}

pub(super) fn qualified_expr_path(expr: &Expr) -> Option<Vec<&str>> {
    match expr {
        Expr::Name { text, .. } => Some(vec![text]),
        Expr::Field { base, name, .. } => {
            let mut path = qualified_expr_path(base)?;
            path.push(name);
            Some(path)
        }
        _ => None,
    }
}

pub(super) fn literal_value(expr: &Expr) -> Option<Value> {
    match expr {
        Expr::Literal { text, kind, .. } => match kind {
            orna_syntax_ast::LiteralKind::Integer => text.parse::<BigInt>().ok().map(Value::int),
            orna_syntax_ast::LiteralKind::Decimal => literal_decimal(text),
            orna_syntax_ast::LiteralKind::String => {
                literal_string(text).and_then(|text| Value::new(OvbRaw::Text(text)).ok())
            }
            orna_syntax_ast::LiteralKind::Boolean => Value::new(OvbRaw::Bool(text == "true")).ok(),
            orna_syntax_ast::LiteralKind::Null => Value::new(OvbRaw::Null).ok(),
            _ => None,
        },
        Expr::Tuple { elements, .. } | Expr::List { elements, .. } => elements
            .iter()
            .map(literal_value)
            .collect::<Option<Vec<_>>>()
            .and_then(|values| {
                Value::new(OvbRaw::Array(
                    values
                        .into_iter()
                        .map(|value| value.raw().clone())
                        .collect(),
                ))
                .ok()
            }),
        Expr::Record { fields, .. } | Expr::Nominal { fields, .. } => {
            let mut fields = fields
                .iter()
                .map(|field| {
                    Some((
                        field.name.clone(),
                        literal_value(&field.value)?.raw().clone(),
                    ))
                })
                .collect::<Option<Vec<_>>>()?;
            fields.sort_by_cached_key(|(name, _)| {
                Value::new(OvbRaw::Text(name.clone()))
                    .expect("text field key is a valid value")
                    .encode()
                    .expect("text field key encodes canonically")
            });
            Value::new(OvbRaw::Map(
                fields
                    .into_iter()
                    .map(|(name, value)| (OvbRaw::Text(name), value))
                    .collect(),
            ))
            .ok()
        }
        _ => None,
    }
}

pub(super) fn literal_decimal(text: &str) -> Option<Value> {
    let text = text.replace('_', "");
    let (mantissa, exponent) = match text.find(['e', 'E']) {
        Some(index) => (&text[..index], text[index + 1..].parse::<i64>().ok()?),
        None => (text.as_str(), 0),
    };
    let (whole, fraction) = mantissa.split_once('.')?;
    let coefficient = BigInt::parse_bytes(format!("{whole}{fraction}").as_bytes(), 10)?;
    let exponent = exponent.checked_sub(i64::try_from(fraction.len()).ok()?)?;
    Value::decimal(coefficient, BigInt::from(exponent)).ok()
}

pub(super) fn literal_string(text: &str) -> Option<String> {
    let body = text.strip_prefix('"')?.strip_suffix('"')?;
    (!body.contains('\\')).then(|| body.into())
}

/// Evaluates the admitted `table.insert` row expression for one stream item
/// (ORNA-A12-01). The item parameter is bound to the decoded item value and the
/// row expression is evaluated with the admitted assertion functions, so helper
/// calls and arithmetic in the item expression produce the row.
pub(super) fn evaluate_list_item_row(
    bridge: &ListStreamBridge,
    item: &Value,
    limits: EvaluatorLimits,
) -> Result<Value, EvaluationError> {
    let environment = Environment::from([(bridge.parameter.clone(), item.clone())]);
    let mut budget = StepBudget::new(limits.max_steps);
    evaluate_with_functions_and_budget(
        &bridge.insert_row,
        &environment,
        &bridge.assertion_functions,
        limits,
        &mut budget,
    )
}

/// Refusal for one stream item whose admitted row expression fails to
/// evaluate. The evaluation failure is kept as a cause (ORNA-ASSERT-049) and
/// the root message names the owning source (ORNA-ASSERT-051). The item value
/// is not presented.
pub(super) fn item_evaluation_refusal(
    bridge: &ListStreamBridge,
    error: &EvaluationError,
) -> Diagnostic {
    let message = SafeText::new(format!(
        "literal list stream item was refused by owner {}",
        bridge.source_identity
    ))
    .unwrap_or_else(|_| {
        SafeText::new("literal list stream item was refused").expect("static message")
    });
    Diagnostic::new(
        SafeText::new("ORNA-LIST-STREAM-ITEM").expect("static code"),
        DiagnosticSeverity::Error,
        SafeText::new("literal list stream item was refused").expect("static message"),
    )
    .expect("valid diagnostic")
    .with_cause(error.diagnostic().clone())
    .redacted_with_message(message)
}
