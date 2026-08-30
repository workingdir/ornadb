use super::*;
pub(crate) async fn append_security_audit_event(
    transaction: &Transaction<'_>,
    decision: SecurityAuditDecision,
) -> Result<SecurityAuditEventId, PostgresKernelError> {
    let event = SecurityAuditEventId::new();
    let event_id = event.to_bytes().to_vec();
    let kind = encode_security_audit_kind(decision.kind());
    let outcome = match decision.outcome() {
        SecurityAuditOutcome::Allowed => "allowed",
        SecurityAuditOutcome::Denied => "denied",
    };
    let session_principal = decision
        .session_principal()
        .map(|principal| principal.to_bytes().to_vec());
    let effective_principal = decision
        .effective_principal()
        .map(|principal| principal.to_bytes().to_vec());
    let authorising_principal = decision
        .authorising_principal()
        .map(|principal| principal.to_bytes().to_vec());
    let (function, source_revision, catalogue_revision) =
        encode_security_audit_identity_columns(&decision);
    let denial_reason = match decision.denial() {
        None => decision
            .user_state_operation()
            .zip(decision.user_state_cell_count())
            .map(|(operation, cell_count)| encode_user_state_audit_detail(operation, cell_count))
            .or_else(|| {
                decision
                    .capability_name()
                    .map(encode_capability_audit_denial)
            })
            .or_else(|| {
                decision
                    .inspect_requested()
                    .zip(decision.inspect_epoch_scope())
                    .map(|(requested, scope)| encode_inspect_audit_detail(requested, scope))
            })
            .or_else(|| {
                decision
                    .security_admin_operation()
                    .map(encode_security_admin_audit_detail)
            })
            .or_else(|| {
                decision
                    .source_apply_candidate()
                    .map(|_| encode_source_apply_audit_detail().to_owned())
            }),
        Some(SecurityAuditDenial::Authentication(reason)) => {
            Some(encode_authentication_audit_denial(reason).to_owned())
        }
        Some(SecurityAuditDenial::Execute(reason)) => {
            Some(encode_execute_audit_denial(reason).to_owned())
        }
        Some(SecurityAuditDenial::Capability { capability }) => {
            Some(encode_capability_audit_denial(&capability))
        }
        Some(SecurityAuditDenial::Inspect(reason)) => {
            Some(encode_inspect_audit_denial(reason).to_owned())
        }
        Some(SecurityAuditDenial::SecurityAdmin(reason)) => {
            encode_security_admin_audit_denied_detail(&decision, reason)
        }
    };
    transaction
        .execute(
            "INSERT INTO _orna_kernel.security_audit_events
                 (event_id, event_kind, outcome, session_principal_id,
                  effective_principal_id, authorising_principal_id, function_id,
                  source_revision_id, catalogue_revision_id, denial_reason)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            &[
                &event_id,
                &kind,
                &outcome,
                &session_principal,
                &effective_principal,
                &authorising_principal,
                &function,
                &source_revision,
                &catalogue_revision,
                &denial_reason,
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    Ok(event)
}

/// Appends one closed invocation decision in the caller's protected transaction.
///
/// PostgreSQL generates the relation sequence and recording time. The caller
/// cannot supply Request, bind, lifecycle, delivery, or diagnostic data.
pub(crate) async fn append_invocation_audit_event(
    transaction: &Transaction<'_>,
    decision: InvocationAuditDecision,
) -> Result<InvocationAuditEventId, PostgresKernelError> {
    let record = decision.invocation.canonical();
    validate_invocation_audit_decision_shape(&decision, &record)?;
    let security_events = load_security_audit_events(transaction).await?;
    validate_invocation_audit_evidence(&decision, &security_events, &record)?;
    if let Some(target) = decision.target {
        require_invocation_audit_target(transaction, target, &record).await?;
    }

    let event_id = InvocationAuditEventId::new();
    let event_id_bytes = event_id.to_bytes().to_vec();
    let invocation_id = decision.invocation.to_bytes().to_vec();
    let outcome = encode_invocation_audit_outcome(decision.outcome);
    let session_principal = decision.session_principal.to_bytes().to_vec();
    let effective_principal = decision
        .effective_principal
        .map(|principal| principal.to_bytes().to_vec());
    let authorising_principal = decision
        .authorising_principal
        .map(|principal| principal.to_bytes().to_vec());
    let (function, source_revision, catalogue_revision) = decision
        .target
        .map(|target| {
            (
                Some(target.function().to_bytes().to_vec()),
                Some(target.revision().source().to_bytes().to_vec()),
                Some(target.revision().catalogue().to_bytes().to_vec()),
            )
        })
        .unwrap_or((None, None, None));
    let security_audit_event = decision
        .security_audit_event
        .map(|event| event.to_bytes().to_vec());
    transaction
        .execute(
            "INSERT INTO _orna_kernel.invocation_audit_events
                 (event_id, invocation_id, outcome, session_principal_id,
                  effective_principal_id, authorising_principal_id, function_id,
                  source_revision_id, catalogue_revision_id, security_audit_event_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            &[
                &event_id_bytes,
                &invocation_id,
                &outcome,
                &session_principal,
                &effective_principal,
                &authorising_principal,
                &function,
                &source_revision,
                &catalogue_revision,
                &security_audit_event,
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    Ok(event_id)
}
pub(super) async fn resource_parent_invocation_is_owned_in_transaction(
    transaction: &Transaction<'_>,
    authenticated_session: &AuthenticatedSession,
    parent_invocation_id: InvocationId,
) -> Result<bool, PostgresKernelError> {
    let parent_invocation_id = parent_invocation_id.to_bytes().to_vec();
    let session_principal = authenticated_session.principal().to_bytes().to_vec();
    transaction
        .query_opt(
            "SELECT 1
             FROM _orna_kernel.invocation_audit_events
             WHERE invocation_id = $1
               AND session_principal_id = $2",
            &[&parent_invocation_id, &session_principal],
        )
        .await
        .map(|row| row.is_some())
        .map_err(PostgresKernelError::Database)
}

pub(super) fn resource_parent_invocation_unavailable(
    request: &ResourceRequest,
) -> PostgresKernelError {
    PostgresKernelError::DurableInvariant {
        relation: "_orna_kernel.invocation_audit_events",
        record: request.request_id.canonical(),
        rule: "resource parent invocation must belong to authenticated session",
    }
}

/// The terminal state retained for one authenticated resource request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceAuditTerminalOutcome {
    /// The target returned its complete bounded result.
    Completed,
    /// The request was denied or execution failed before a result completed.
    Failed,
    /// Cancellation won before a terminal result was committed.
    Cancelled,
}

/// Appends one redacted resource terminal row in the caller's transaction.
///
/// This helper deliberately accepts only identity, decision, terminal, target,
/// and bounded count metadata. Arguments and returned values never cross this
/// boundary. A target is retained only when the caller has recovered an exact
/// active target; unresolved or stale targets must pass None.
pub(crate) async fn append_resource_audit_event(
    transaction: &Transaction<'_>,
    authenticated_session: &AuthenticatedSession,
    request: &ResourceRequest,
    nested_invocation_id: Option<InvocationId>,
    decision: SecurityAuditOutcome,
    terminal: ResourceAuditTerminalOutcome,
    target: Option<InvocationTarget>,
    item_count: Option<u64>,
    byte_count: Option<u64>,
) -> Result<(), PostgresKernelError> {
    validate_resource_lineage(request)?;
    if !resource_parent_invocation_is_owned_in_transaction(
        transaction,
        authenticated_session,
        request.parent_invocation_id,
    )
    .await?
    {
        return Err(resource_parent_invocation_unavailable(request));
    }
    validate_resource_audit_nested_invocation(
        "resource request",
        request.request_id.canonical(),
        nested_invocation_id.map(InvocationId::to_bytes),
    )?;
    if nested_invocation_id.is_none()
        && (decision != SecurityAuditOutcome::Denied
            || !matches!(
                terminal,
                ResourceAuditTerminalOutcome::Failed | ResourceAuditTerminalOutcome::Cancelled
            ))
    {
        return Err(resource_audit_invariant(
            &request.request_id.canonical(),
            "resource audit without nested invocation must be a preaccept denied or cancelled terminal",
        ));
    }
    validate_resource_state_context(request)?;
    let item_count = item_count
        .map(|count| {
            i64::try_from(count).map_err(|_| {
                resource_audit_invariant(
                    &request.request_id.canonical(),
                    "resource item count must fit a signed 64-bit database count",
                )
            })
        })
        .transpose()?;
    let byte_count = byte_count
        .map(|count| {
            i64::try_from(count).map_err(|_| {
                resource_audit_invariant(
                    &request.request_id.canonical(),
                    "resource byte count must fit a signed 64-bit database count",
                )
            })
        })
        .transpose()?;
    let event_id = InvocationAuditEventId::new();
    let event_id_bytes = event_id.to_bytes().to_vec();
    let request_id = request.request_id.to_bytes().to_vec();
    let nested_invocation_id_bytes = nested_invocation_id.map(|id| id.to_bytes().to_vec());
    let parent_invocation_id = request.parent_invocation_id.to_bytes().to_vec();
    let call_site_id = request.call_site_id.to_bytes().to_vec();
    let session_principal = authenticated_session.principal().to_bytes().to_vec();
    let (target_function, source_revision, catalogue_revision) = target
        .map(|target| {
            (
                Some(target.function().to_bytes().to_vec()),
                Some(target.revision().source().to_bytes().to_vec()),
                Some(target.revision().catalogue().to_bytes().to_vec()),
            )
        })
        .unwrap_or((None, None, None));
    let decision = encode_resource_audit_decision(decision);
    let terminal = encode_resource_audit_terminal(terminal);
    transaction
        .execute(
            "INSERT INTO _orna_kernel.resource_audit_events
                 (event_id, request_id, nested_invocation_id, parent_invocation_id,
                  call_site_id, target_function_id, source_revision_id,
                  catalogue_revision_id, session_principal_id, decision_outcome,
                  terminal_outcome, item_count, byte_count)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
            &[
                &event_id_bytes,
                &request_id,
                &nested_invocation_id_bytes,
                &parent_invocation_id,
                &call_site_id,
                &target_function,
                &source_revision,
                &catalogue_revision,
                &session_principal,
                &decision,
                &terminal,
                &item_count,
                &byte_count,
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    Ok(())
}

fn encode_resource_audit_decision(decision: SecurityAuditOutcome) -> &'static str {
    match decision {
        SecurityAuditOutcome::Allowed => "allowed",
        SecurityAuditOutcome::Denied => "denied",
    }
}

fn encode_resource_audit_terminal(terminal: ResourceAuditTerminalOutcome) -> &'static str {
    match terminal {
        ResourceAuditTerminalOutcome::Completed => "completed",
        ResourceAuditTerminalOutcome::Failed => "failed",
        ResourceAuditTerminalOutcome::Cancelled => "cancelled",
    }
}

pub(super) fn validate_resource_lineage(
    request: &ResourceRequest,
) -> Result<(), PostgresKernelError> {
    validate_resource_lineage_identities(
        "resource request",
        request.request_id.canonical(),
        request.request_id.to_bytes(),
        request.parent_invocation_id.to_bytes(),
        request.call_site_id.to_bytes(),
    )
}

pub(super) fn validate_resource_audit_lineage(
    record: &str,
    request_id: [u8; 16],
    nested_invocation_id: Option<[u8; 16]>,
    parent_invocation_id: [u8; 16],
    call_site_id: [u8; 16],
) -> Result<(), PostgresKernelError> {
    validate_resource_lineage_identities(
        "_orna_kernel.resource_audit_events",
        record.to_owned(),
        request_id,
        parent_invocation_id,
        call_site_id,
    )?;
    validate_resource_audit_nested_invocation(
        "_orna_kernel.resource_audit_events",
        record.to_owned(),
        nested_invocation_id,
    )
}

pub(super) fn validate_resource_audit_nested_invocation(
    relation: &'static str,
    record: String,
    nested_invocation_id: Option<[u8; 16]>,
) -> Result<(), PostgresKernelError> {
    if nested_invocation_id.is_some_and(|id| id == [0; 16]) {
        return Err(PostgresKernelError::DurableInvariant {
            relation,
            record,
            rule: "resource nested invocation identity must be non-zero",
        });
    }
    Ok(())
}

fn validate_resource_lineage_identities(
    relation: &'static str,
    record: String,
    request_id: [u8; 16],
    parent_invocation_id: [u8; 16],
    call_site_id: [u8; 16],
) -> Result<(), PostgresKernelError> {
    if request_id == [0; 16] {
        return Err(PostgresKernelError::DurableInvariant {
            relation,
            record,
            rule: "resource request identity must be non-zero",
        });
    }
    if parent_invocation_id == [0; 16] {
        return Err(PostgresKernelError::DurableInvariant {
            relation,
            record,
            rule: "resource parent invocation identity must be non-zero",
        });
    }
    if call_site_id == [0; 16] {
        return Err(PostgresKernelError::DurableInvariant {
            relation,
            record,
            rule: "resource call-site identity must be non-zero",
        });
    }
    Ok(())
}

pub(super) fn validate_resource_state_context(
    request: &ResourceRequest,
) -> Result<(), PostgresKernelError> {
    ClientStateContext::new(
        request.target_function_id,
        request.state_profile.clone(),
        request.function_instance_key.clone(),
    )
    .map(|_| ())
    .map_err(|_| PostgresKernelError::DurableInvariant {
        relation: "resource request",
        record: request.request_id.canonical(),
        rule: "resource state context must contain valid text",
    })
}

pub(super) fn resource_audit_invariant(record: &str, rule: &'static str) -> PostgresKernelError {
    PostgresKernelError::DurableInvariant {
        relation: "_orna_kernel.resource_audit_events",
        record: record.to_owned(),
        rule,
    }
}
