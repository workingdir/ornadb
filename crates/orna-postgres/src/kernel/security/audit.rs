use super::*;

pub(super) fn encode_invocation_audit_outcome(outcome: SecurityAuditOutcome) -> &'static str {
    match outcome {
        SecurityAuditOutcome::Allowed => "allowed",
        SecurityAuditOutcome::Denied => "denied",
    }
}

pub(super) fn decode_invocation_audit_outcome(
    outcome: String,
    record: &str,
) -> Result<SecurityAuditOutcome, PostgresKernelError> {
    match outcome.as_str() {
        "allowed" => Ok(SecurityAuditOutcome::Allowed),
        "denied" => Ok(SecurityAuditOutcome::Denied),
        _ => Err(invocation_audit_invariant(
            record,
            "invocation outcome must be allowed or denied",
        )),
    }
}

pub(super) async fn require_invocation_audit_relation_columns(
    transaction: &Transaction<'_>,
) -> Result<(), PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT attribute.attname
         FROM pg_catalog.pg_attribute AS attribute
         JOIN pg_catalog.pg_class AS class ON class.oid = attribute.attrelid
         JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = class.relnamespace
         WHERE namespace.nspname = '_orna_kernel'
           AND class.relname = 'invocation_audit_events'
           AND attribute.attnum > 0
           AND NOT attribute.attisdropped
         ORDER BY attribute.attnum",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let names = rows
        .iter()
        .map(|row| invocation_audit_column(row, "relation", "attname"))
        .collect::<Result<Vec<String>, _>>()?;
    let expected = [
        "sequence",
        "event_id",
        "recorded_at",
        "invocation_id",
        "outcome",
        "session_principal_id",
        "effective_principal_id",
        "authorising_principal_id",
        "function_id",
        "source_revision_id",
        "catalogue_revision_id",
        "security_audit_event_id",
    ];
    if names != expected {
        return Err(invocation_audit_invariant(
            "relation",
            "invocation audit relation has unsupported disclosure-bearing columns",
        ));
    }
    Ok(())
}

pub(super) async fn require_security_audit_relation_columns(
    transaction: &Transaction<'_>,
) -> Result<(), PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT attribute.attname
         FROM pg_catalog.pg_attribute AS attribute
         JOIN pg_catalog.pg_class AS class ON class.oid = attribute.attrelid
         JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = class.relnamespace
         WHERE namespace.nspname = '_orna_kernel'
           AND class.relname = 'security_audit_events'
           AND attribute.attnum > 0
           AND NOT attribute.attisdropped
         ORDER BY attribute.attnum",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let names = rows
        .iter()
        .map(|row| audit_column(row, "relation", "attname"))
        .collect::<Result<Vec<String>, _>>()?;
    let expected = [
        "sequence",
        "event_id",
        "recorded_at",
        "event_kind",
        "outcome",
        "session_principal_id",
        "effective_principal_id",
        "authorising_principal_id",
        "function_id",
        "source_revision_id",
        "catalogue_revision_id",
        "denial_reason",
    ];
    if names != expected {
        return Err(audit_invariant(
            "relation",
            "security audit relation has unsupported disclosure-bearing columns",
        ));
    }
    Ok(())
}

pub(super) fn decode_security_audit_event(
    row: &Row,
) -> Result<SecurityAuditEvent, PostgresKernelError> {
    let sequence: i64 = audit_column(row, "selected row", "sequence")?;
    let record = sequence.to_string();
    if sequence <= 0 {
        return Err(audit_invariant(
            &record,
            "generated security audit sequence must be positive",
        ));
    }
    let id = SecurityAuditEventId::from_bytes(audit_id(row, &record, "event_id")?);
    let recorded_at: SystemTime = audit_column(row, &record, "recorded_at")?;
    let kind: String = audit_column(row, &record, "event_kind")?;
    let outcome: String = audit_column(row, &record, "outcome")?;
    let session_principal =
        audit_optional_id(row, &record, "session_principal_id")?.map(PrincipalId::from_bytes);
    let effective_principal =
        audit_optional_id(row, &record, "effective_principal_id")?.map(PrincipalId::from_bytes);
    let authorising_principal =
        audit_optional_id(row, &record, "authorising_principal_id")?.map(PrincipalId::from_bytes);
    let function = audit_optional_id(row, &record, "function_id")?.map(FunctionId::from_bytes);
    let source_revision =
        audit_optional_id(row, &record, "source_revision_id")?.map(SourceRevisionId::from_bytes);
    let catalogue_revision = audit_optional_id(row, &record, "catalogue_revision_id")?
        .map(CatalogueRevisionId::from_bytes);
    let denial_reason: Option<String> = audit_column(row, &record, "denial_reason")?;

    let decision = match (kind.as_str(), outcome.as_str()) {
        ("authentication", "allowed")
            if effective_principal.is_none()
                && authorising_principal.is_none()
                && function.is_none()
                && source_revision.is_none()
                && catalogue_revision.is_none()
                && denial_reason.is_none() =>
        {
            SecurityAuditDecision::recover_authentication_allowed(require_audit_value(
                session_principal,
                &record,
                "allowed authentication requires a session principal",
            )?)
        }
        ("authentication", "denied")
            if effective_principal.is_none()
                && authorising_principal.is_none()
                && function.is_none()
                && source_revision.is_none()
                && catalogue_revision.is_none() =>
        {
            let reason = decode_authentication_audit_denial(
                require_audit_value(
                    denial_reason,
                    &record,
                    "denied authentication requires a reason",
                )?,
                &record,
            )?;
            SecurityAuditDecision::authentication_denied(session_principal, reason).map_err(
                |_| audit_invariant(&record, "authentication principal and reason must agree"),
            )?
        }
        ("execute", "allowed") if denial_reason.is_none() => {
            let target = audit_target(function, source_revision, catalogue_revision, &record)?;
            SecurityAuditDecision::recover_execute_allowed(
                require_audit_value(
                    session_principal,
                    &record,
                    "allowed EXECUTE requires a session principal",
                )?,
                require_audit_value(
                    effective_principal,
                    &record,
                    "allowed EXECUTE requires an effective principal",
                )?,
                require_audit_value(
                    authorising_principal,
                    &record,
                    "allowed EXECUTE requires an authorising principal",
                )?,
                target,
            )
        }
        ("execute", "denied")
            if effective_principal.is_none() && authorising_principal.is_none() =>
        {
            let target = audit_target(function, source_revision, catalogue_revision, &record)?;
            let reason = decode_execute_audit_denial(
                require_audit_value(denial_reason, &record, "denied EXECUTE requires a reason")?,
                &record,
            )?;
            SecurityAuditDecision::recover_execute_denied(
                require_audit_value(
                    session_principal,
                    &record,
                    "denied EXECUTE requires a session principal",
                )?,
                target,
                reason,
            )
        }
        ("capability", outcome @ ("allowed" | "denied"))
            if effective_principal.is_none()
                && authorising_principal.is_none()
                && function.is_some()
                && source_revision.is_some()
                && catalogue_revision.is_some() =>
        {
            let target = audit_target(function, source_revision, catalogue_revision, &record)?;
            let capability = decode_capability_audit_denial(
                require_audit_value(
                    denial_reason,
                    &record,
                    "capability audit requires a capability name",
                )?,
                &record,
            )?;
            let session_principal = require_audit_value(
                session_principal,
                &record,
                "capability audit requires a session principal",
            )?;
            let decision = match outcome {
                "allowed" => SecurityAuditDecision::recover_capability_allowed(
                    session_principal,
                    target,
                    capability,
                ),
                "denied" => SecurityAuditDecision::recover_capability_denied(
                    session_principal,
                    target,
                    capability,
                ),
                _ => unreachable!("capability outcome is closed by the outer match"),
            };
            decision.map_err(|_| {
                audit_invariant(
                    &record,
                    "capability audit name must be a qualified name with no arguments",
                )
            })?
        }
        ("user_state", "allowed")
            if effective_principal.is_none()
                && authorising_principal.is_none()
                && function.is_some()
                && source_revision.is_none()
                && catalogue_revision.is_none() =>
        {
            let operation_detail = require_audit_value(
                denial_reason,
                &record,
                "USER state audit requires an operation and cell count",
            )?;
            let (operation, cell_count) =
                decode_user_state_audit_detail(&operation_detail, &record)?;
            SecurityAuditDecision::recover_user_state_allowed(
                require_audit_value(
                    session_principal,
                    &record,
                    "USER state audit requires a session principal",
                )?,
                operation,
                require_audit_value(
                    function,
                    &record,
                    "USER state audit requires a root function",
                )?,
                cell_count,
            )
        }
        ("inspect", "allowed")
            if effective_principal.is_none()
                && authorising_principal.is_none()
                && function.is_none()
                && source_revision.is_none()
                && catalogue_revision.is_none() =>
        {
            // The protected columns retain only the closed capture detail in
            // the denial-reason column; the epoch owner is never stored.
            let (requested, scope) = decode_inspect_audit_detail(
                &require_audit_value(
                    denial_reason,
                    &record,
                    "INSPECT audit requires a capture detail",
                )?,
                &record,
            )?;
            SecurityAuditDecision::recover_inspect_allowed(
                require_audit_value(
                    session_principal,
                    &record,
                    "INSPECT audit requires a session principal",
                )?,
                requested,
                scope,
                None,
            )
        }
        ("inspect", "denied")
            if effective_principal.is_none()
                && authorising_principal.is_none()
                && function.is_none()
                && source_revision.is_none()
                && catalogue_revision.is_none() =>
        {
            let reason = decode_inspect_audit_denial(
                require_audit_value(denial_reason, &record, "denied INSPECT requires a reason")?,
                &record,
            )?;
            SecurityAuditDecision::recover_inspect_denied(
                require_audit_value(
                    session_principal,
                    &record,
                    "denied INSPECT requires a session principal",
                )?,
                None,
                reason,
            )
        }
        ("source_apply", "allowed")
            if effective_principal.is_none()
                && authorising_principal.is_none()
                && function.is_none()
                && source_revision.is_some()
                && catalogue_revision.is_some() =>
        {
            decode_source_apply_audit_detail(
                &require_audit_value(
                    denial_reason,
                    &record,
                    "allowed source apply audit requires a committed detail",
                )?,
                &record,
            )?;
            let session_principal = require_audit_value(
                session_principal,
                &record,
                "source apply audit requires a session principal",
            )?;
            if session_principal != CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID {
                return Err(audit_invariant(
                    &record,
                    "source apply audit must use the catalogue-health service principal",
                ));
            }
            SecurityAuditDecision::recover_source_apply_allowed(
                session_principal,
                RevisionPair::new(
                    require_audit_value(
                        source_revision,
                        &record,
                        "source apply audit requires a source revision",
                    )?,
                    require_audit_value(
                        catalogue_revision,
                        &record,
                        "source apply audit requires a catalogue revision",
                    )?,
                ),
            )
        }
        ("security_admin", "allowed")
            if effective_principal.is_none()
                && authorising_principal.is_none()
                && function.is_some()
                && source_revision.is_none()
                && catalogue_revision.is_none() =>
        {
            // The protected columns retain only the closed operation detail
            // and the sealed target identity; argument payloads are never
            // stored.
            let operation = decode_security_admin_audit_detail(
                &require_audit_value(
                    denial_reason,
                    &record,
                    "allowed security-admin audit requires an operation detail",
                )?,
                &record,
            )?;
            let target = require_audit_value(
                function,
                &record,
                "security-admin audit requires the sealed target identity",
            )?;
            require_security_admin_audit_target(target, operation, &record)?;
            SecurityAuditDecision::recover_security_admin_allowed(
                require_audit_value(
                    session_principal,
                    &record,
                    "allowed security-admin audit requires a session principal",
                )?,
                operation,
                target,
            )
        }
        ("security_admin", "denied")
            if effective_principal.is_none()
                && authorising_principal.is_none()
                && function.is_some()
                && source_revision.is_none()
                && catalogue_revision.is_none() =>
        {
            let (operation, reason) = decode_security_admin_audit_denial(
                &require_audit_value(
                    denial_reason,
                    &record,
                    "denied security-admin audit requires a reason",
                )?,
                &record,
            )?;
            let target = require_audit_value(
                function,
                &record,
                "security-admin audit requires the sealed target identity",
            )?;
            require_security_admin_audit_target(target, operation, &record)?;
            SecurityAuditDecision::recover_security_admin_denied(
                require_audit_value(
                    session_principal,
                    &record,
                    "denied security-admin audit requires a session principal",
                )?,
                operation,
                target,
                reason,
            )
        }
        _ => {
            return Err(audit_invariant(
                &record,
                "audit event shape is not recognised",
            ));
        }
    };

    Ok(SecurityAuditEvent::new(id, sequence, recorded_at, decision))
}

pub(super) fn audit_target(
    function: Option<FunctionId>,
    source: Option<SourceRevisionId>,
    catalogue: Option<CatalogueRevisionId>,
    record: &str,
) -> Result<InvocationTarget, PostgresKernelError> {
    Ok(InvocationTarget::new(
        require_audit_value(function, record, "EXECUTE requires a function")?,
        RevisionPair::new(
            require_audit_value(source, record, "EXECUTE requires a source revision")?,
            require_audit_value(catalogue, record, "EXECUTE requires a catalogue revision")?,
        ),
    ))
}

pub(super) fn decode_authentication_audit_denial(
    value: String,
    record: &str,
) -> Result<LocalPeerAuthenticationError, PostgresKernelError> {
    let invalid = |reason| LocalPeerAuthenticationError::InvalidPrincipal(reason);
    match value.as_str() {
        "authentication_unknown_uid" => Ok(LocalPeerAuthenticationError::UnknownUid),
        "authentication_unknown_session_principal" => {
            Ok(invalid(SessionBindingError::UnknownSessionPrincipal))
        }
        "authentication_disabled_session_principal" => {
            Ok(invalid(SessionBindingError::DisabledSessionPrincipal))
        }
        "authentication_role_cannot_authenticate" => {
            Ok(invalid(SessionBindingError::RoleCannotAuthenticate))
        }
        "authentication_duplicate_active_role" => {
            Ok(invalid(SessionBindingError::DuplicateActiveRole))
        }
        "authentication_unknown_active_role" => Ok(invalid(SessionBindingError::UnknownActiveRole)),
        "authentication_disabled_active_role" => {
            Ok(invalid(SessionBindingError::DisabledActiveRole))
        }
        "authentication_active_principal_is_not_role" => {
            Ok(invalid(SessionBindingError::ActivePrincipalIsNotRole))
        }
        "authentication_unreachable_active_role" => {
            Ok(invalid(SessionBindingError::UnreachableActiveRole))
        }
        _ => Err(audit_invariant(
            record,
            "authentication denial reason is unsupported",
        )),
    }
}

pub(super) fn encode_authentication_audit_denial(
    reason: LocalPeerAuthenticationError,
) -> &'static str {
    match reason {
        LocalPeerAuthenticationError::UnknownUid => "authentication_unknown_uid",
        LocalPeerAuthenticationError::InvalidPrincipal(
            SessionBindingError::UnknownSessionPrincipal,
        ) => "authentication_unknown_session_principal",
        LocalPeerAuthenticationError::InvalidPrincipal(
            SessionBindingError::DisabledSessionPrincipal,
        ) => "authentication_disabled_session_principal",
        LocalPeerAuthenticationError::InvalidPrincipal(
            SessionBindingError::RoleCannotAuthenticate,
        ) => "authentication_role_cannot_authenticate",
        LocalPeerAuthenticationError::InvalidPrincipal(
            SessionBindingError::DuplicateActiveRole,
        ) => "authentication_duplicate_active_role",
        LocalPeerAuthenticationError::InvalidPrincipal(SessionBindingError::UnknownActiveRole) => {
            "authentication_unknown_active_role"
        }
        LocalPeerAuthenticationError::InvalidPrincipal(SessionBindingError::DisabledActiveRole) => {
            "authentication_disabled_active_role"
        }
        LocalPeerAuthenticationError::InvalidPrincipal(
            SessionBindingError::ActivePrincipalIsNotRole,
        ) => "authentication_active_principal_is_not_role",
        LocalPeerAuthenticationError::InvalidPrincipal(
            SessionBindingError::UnreachableActiveRole,
        ) => "authentication_unreachable_active_role",
    }
}

pub(super) fn decode_execute_audit_denial(
    value: String,
    record: &str,
) -> Result<orna_core::security::ExecuteDenial, PostgresKernelError> {
    use orna_core::security::ExecuteDenial;

    match value.as_str() {
        "execute_invalid_session" => Ok(ExecuteDenial::InvalidSession),
        "execute_unknown_function" => Ok(ExecuteDenial::UnknownFunction),
        "execute_revision_mismatch" => Ok(ExecuteDenial::RevisionMismatch),
        "execute_missing_grant" => Ok(ExecuteDenial::MissingExecuteGrant),
        "execute_unsupported_security_definer" => Ok(ExecuteDenial::UnsupportedSecurityDefiner),
        _ => Err(audit_invariant(
            record,
            "EXECUTE denial reason is unsupported",
        )),
    }
}

pub(super) fn encode_execute_audit_denial(
    reason: orna_core::security::ExecuteDenial,
) -> &'static str {
    use orna_core::security::ExecuteDenial;

    match reason {
        ExecuteDenial::InvalidSession => "execute_invalid_session",
        ExecuteDenial::UnknownFunction => "execute_unknown_function",
        ExecuteDenial::RevisionMismatch => "execute_revision_mismatch",
        ExecuteDenial::MissingExecuteGrant => "execute_missing_grant",
        ExecuteDenial::UnsupportedSecurityDefiner => "execute_unsupported_security_definer",
    }
}

pub(super) fn encode_security_audit_identity_columns(
    decision: &SecurityAuditDecision,
) -> (Option<Vec<u8>>, Option<Vec<u8>>, Option<Vec<u8>>) {
    if let Some(candidate) = decision.source_apply_candidate() {
        return (
            None,
            Some(candidate.source().to_bytes().to_vec()),
            Some(candidate.catalogue().to_bytes().to_vec()),
        );
    }
    match decision.target() {
        Some(target) => (
            Some(target.function().to_bytes().to_vec()),
            Some(target.revision().source().to_bytes().to_vec()),
            Some(target.revision().catalogue().to_bytes().to_vec()),
        ),
        None => (
            decision
                .user_state_root_function()
                .or_else(|| decision.security_admin_target())
                .map(|function| function.to_bytes().to_vec()),
            None,
            None,
        ),
    }
}

pub(super) fn encode_security_audit_kind(kind: SecurityAuditKind) -> &'static str {
    match kind {
        SecurityAuditKind::Authentication => "authentication",
        SecurityAuditKind::Execute => "execute",
        SecurityAuditKind::Capability => "capability",
        SecurityAuditKind::UserState => "user_state",
        SecurityAuditKind::Inspect => "inspect",
        SecurityAuditKind::SecurityAdmin => "security_admin",
        SecurityAuditKind::SourceApply => "source_apply",
    }
}

pub(super) fn encode_capability_audit_denial(capability: &str) -> String {
    format!("capability:{capability}")
}
pub(super) fn encode_source_apply_audit_detail() -> &'static str {
    "source_apply:committed"
}

pub(super) fn decode_source_apply_audit_detail(
    value: &str,
    record: &str,
) -> Result<(), PostgresKernelError> {
    if value == encode_source_apply_audit_detail() {
        Ok(())
    } else {
        Err(audit_invariant(
            record,
            "source apply audit detail is unsupported",
        ))
    }
}
pub(super) fn encode_user_state_audit_detail(
    operation: UserStateAuditOperation,
    cell_count: u64,
) -> String {
    let operation = match operation {
        UserStateAuditOperation::Load => "load",
        UserStateAuditOperation::Write => "write",
    };
    format!("user_state:{operation}:cells={cell_count}")
}

pub(super) fn decode_user_state_audit_detail(
    value: &str,
    record: &str,
) -> Result<(UserStateAuditOperation, u64), PostgresKernelError> {
    let Some(rest) = value.strip_prefix("user_state:") else {
        return Err(audit_invariant(
            record,
            "USER state audit detail must start with user_state:",
        ));
    };
    let Some((operation, count)) = rest.split_once(":cells=") else {
        return Err(audit_invariant(
            record,
            "USER state audit detail must contain an operation and cell count",
        ));
    };
    let operation = match operation {
        "load" => UserStateAuditOperation::Load,
        "write" => UserStateAuditOperation::Write,
        _ => {
            return Err(audit_invariant(
                record,
                "USER state audit operation must be load or write",
            ));
        }
    };
    let cell_count = count.parse::<u64>().map_err(|_| {
        audit_invariant(
            record,
            "USER state audit cell count must be a canonical unsigned integer",
        )
    })?;
    if encode_user_state_audit_detail(operation, cell_count) != value {
        return Err(audit_invariant(
            record,
            "USER state audit detail is not canonical",
        ));
    }
    Ok((operation, cell_count))
}

pub(super) fn decode_capability_audit_denial(
    value: String,
    record: &str,
) -> Result<String, PostgresKernelError> {
    value
        .strip_prefix("capability:")
        .map(str::to_owned)
        .ok_or_else(|| audit_invariant(record, "capability denial reason is unsupported"))
}

/// Encodes one closed INSPECT denial reason exactly as the pure model names it.
pub(super) fn encode_inspect_audit_denial(reason: InspectDenial) -> &'static str {
    reason.audit_reason()
}

/// Encodes the allowed INSPECT capture detail into the protected `denial_reason`
/// column, mirroring the USER state operation detail pattern: the column
/// carries a closed `inspect:...` detail for allowed rows and a closed
/// `inspect:...` denial reason for denied rows.
pub(super) fn encode_inspect_audit_detail(
    requested: InspectPrivilege,
    scope: InspectEpochScope,
) -> String {
    format!(
        "inspect:requested={}:scope={}",
        encode_inspect_privilege(requested),
        encode_inspect_scope(scope)
    )
}

pub(super) fn encode_inspect_privilege(privilege: InspectPrivilege) -> &'static str {
    match privilege {
        InspectPrivilege::OwnInvocation => "own-invocation",
        InspectPrivilege::SessionInvocations => "session-invocations",
        InspectPrivilege::AnyInvocation => "any-invocation",
        InspectPrivilege::Values => "values",
        InspectPrivilege::Source => "source",
        InspectPrivilege::SecurityDetails => "security-details",
        InspectPrivilege::RuntimeInternals => "runtime-internals",
    }
}

pub(super) fn encode_inspect_scope(scope: InspectEpochScope) -> &'static str {
    match scope {
        InspectEpochScope::Own => "own",
        InspectEpochScope::Session => "session",
        InspectEpochScope::Foreign => "foreign",
    }
}

pub(super) fn decode_inspect_privilege(
    value: &str,
    record: &str,
) -> Result<InspectPrivilege, PostgresKernelError> {
    match value {
        "own-invocation" => Ok(InspectPrivilege::OwnInvocation),
        "session-invocations" => Ok(InspectPrivilege::SessionInvocations),
        "any-invocation" => Ok(InspectPrivilege::AnyInvocation),
        "values" => Ok(InspectPrivilege::Values),
        "source" => Ok(InspectPrivilege::Source),
        "security-details" => Ok(InspectPrivilege::SecurityDetails),
        "runtime-internals" => Ok(InspectPrivilege::RuntimeInternals),
        _ => Err(audit_invariant(
            record,
            "INSPECT requested privilege is unsupported",
        )),
    }
}

pub(super) fn decode_inspect_scope(
    value: &str,
    record: &str,
) -> Result<InspectEpochScope, PostgresKernelError> {
    match value {
        "own" => Ok(InspectEpochScope::Own),
        "session" => Ok(InspectEpochScope::Session),
        "foreign" => Ok(InspectEpochScope::Foreign),
        _ => Err(audit_invariant(
            record,
            "INSPECT epoch scope is unsupported",
        )),
    }
}

pub(super) fn decode_inspect_audit_detail(
    value: &str,
    record: &str,
) -> Result<(InspectPrivilege, InspectEpochScope), PostgresKernelError> {
    let Some(rest) = value.strip_prefix("inspect:") else {
        return Err(audit_invariant(
            record,
            "INSPECT audit detail must start with inspect:",
        ));
    };
    let Some((requested, scope)) = rest.split_once(":scope=") else {
        return Err(audit_invariant(
            record,
            "INSPECT audit detail must carry a requested privilege and scope",
        ));
    };
    let Some(requested) = requested.strip_prefix("requested=") else {
        return Err(audit_invariant(
            record,
            "INSPECT audit detail must carry a requested privilege",
        ));
    };
    let requested = decode_inspect_privilege(requested, record)?;
    let scope = decode_inspect_scope(scope, record)?;
    if encode_inspect_audit_detail(requested, scope) != value {
        return Err(audit_invariant(
            record,
            "INSPECT audit detail is not canonical",
        ));
    }
    Ok((requested, scope))
}

pub(super) fn decode_inspect_audit_denial(
    value: String,
    record: &str,
) -> Result<InspectDenial, PostgresKernelError> {
    match value.as_str() {
        "inspect:missing-privilege" => Ok(InspectDenial::MissingPrivilege),
        "inspect:missing-epoch" => Ok(InspectDenial::MissingEpoch),
        "inspect:observer-suppressed" => Ok(InspectDenial::ObserverSuppressed),
        _ => Err(audit_invariant(
            record,
            "INSPECT denial reason is unsupported",
        )),
    }
}

/// Encodes one closed security-admin operation kind exactly as the pure
/// model names it.
pub(super) fn encode_security_admin_audit_operation(
    operation: SecurityAdminAuditOperation,
) -> &'static str {
    match operation {
        SecurityAdminAuditOperation::CreatePrincipal => "create_principal",
        SecurityAdminAuditOperation::DisablePrincipal => "disable_principal",
        SecurityAdminAuditOperation::CreateRole => "create_role",
        SecurityAdminAuditOperation::GrantRole => "grant_role",
        SecurityAdminAuditOperation::RevokeRole => "revoke_role",
        SecurityAdminAuditOperation::GrantPrivilege => "grant_privilege",
        SecurityAdminAuditOperation::RevokePrivilege => "revoke_privilege",
    }
}

/// Encodes the allowed security-admin capture detail into the protected
/// `denial_reason` column, mirroring the INSPECT and USER state detail
/// patterns: the column carries a closed `security_admin:<operation>`
/// detail for allowed rows.
pub(super) fn encode_security_admin_audit_detail(operation: SecurityAdminAuditOperation) -> String {
    format!(
        "security_admin:{}",
        encode_security_admin_audit_operation(operation)
    )
}

/// Encodes the denied security-admin capture detail: the closed operation
/// and the closed `missing-privilege` reason tail, so a denied row
/// round-trips both the operation and the denial without ever recording an
/// argument payload.
pub(super) fn encode_security_admin_audit_denied_detail(
    decision: &SecurityAuditDecision,
    reason: PrivilegeDenial,
) -> Option<String> {
    let operation = decision.security_admin_operation()?;
    Some(encode_security_admin_audit_denied_detail_value(
        operation, reason,
    ))
}

pub(super) fn decode_security_admin_audit_operation(
    value: &str,
    record: &str,
) -> Result<SecurityAdminAuditOperation, PostgresKernelError> {
    match value {
        "create_principal" => Ok(SecurityAdminAuditOperation::CreatePrincipal),
        "disable_principal" => Ok(SecurityAdminAuditOperation::DisablePrincipal),
        "create_role" => Ok(SecurityAdminAuditOperation::CreateRole),
        "grant_role" => Ok(SecurityAdminAuditOperation::GrantRole),
        "revoke_role" => Ok(SecurityAdminAuditOperation::RevokeRole),
        "grant_privilege" => Ok(SecurityAdminAuditOperation::GrantPrivilege),
        "revoke_privilege" => Ok(SecurityAdminAuditOperation::RevokePrivilege),
        _ => Err(audit_invariant(
            record,
            "security-admin audit operation is unsupported",
        )),
    }
}

pub(super) fn decode_security_admin_audit_detail(
    value: &str,
    record: &str,
) -> Result<SecurityAdminAuditOperation, PostgresKernelError> {
    let Some(operation) = value.strip_prefix("security_admin:") else {
        return Err(audit_invariant(
            record,
            "security-admin audit detail must start with security_admin:",
        ));
    };
    if operation.contains(':') {
        return Err(audit_invariant(
            record,
            "allowed security-admin audit detail must carry only the operation",
        ));
    }
    let operation = decode_security_admin_audit_operation(operation, record)?;
    if encode_security_admin_audit_detail(operation) != value {
        return Err(audit_invariant(
            record,
            "security-admin audit detail is not canonical",
        ));
    }
    Ok(operation)
}

pub(super) fn decode_security_admin_audit_denial(
    value: &str,
    record: &str,
) -> Result<(SecurityAdminAuditOperation, PrivilegeDenial), PostgresKernelError> {
    let Some(rest) = value.strip_prefix("security_admin:") else {
        return Err(audit_invariant(
            record,
            "security-admin denial reason must start with security_admin:",
        ));
    };
    let Some((operation, reason)) = rest.split_once(':') else {
        return Err(audit_invariant(
            record,
            "security-admin denial reason must carry an operation and a reason",
        ));
    };
    let operation = decode_security_admin_audit_operation(operation, record)?;
    let reason = match reason {
        "missing-privilege" => PrivilegeDenial::MissingPrivilege {
            requested: PrivilegeClass::SecurityAdmin,
        },
        _ => {
            return Err(audit_invariant(
                record,
                "security-admin denial reason is unsupported",
            ));
        }
    };
    if encode_security_admin_audit_denied_detail_value(operation, reason) != value {
        return Err(audit_invariant(
            record,
            "security-admin denial reason is not canonical",
        ));
    }
    Ok((operation, reason))
}

pub(super) fn require_security_admin_audit_target(
    target: FunctionId,
    operation: SecurityAdminAuditOperation,
    record: &str,
) -> Result<(), PostgresKernelError> {
    let Some(definition) = system_function_by_id(target) else {
        return Err(audit_invariant(
            record,
            "security-admin audit target must be a sealed SecurityAdmin function",
        ));
    };
    if definition.kind() != SystemFunctionKind::SecurityAdmin {
        return Err(audit_invariant(
            record,
            "security-admin audit target must be a sealed SecurityAdmin function",
        ));
    }
    if security_admin_audit_target_for_operation(operation) != target {
        return Err(audit_invariant(
            record,
            "security-admin audit target must match operation",
        ));
    }
    Ok(())
}

pub(super) const fn security_admin_audit_target_for_operation(
    operation: SecurityAdminAuditOperation,
) -> FunctionId {
    match operation {
        SecurityAdminAuditOperation::CreatePrincipal => SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID,
        SecurityAdminAuditOperation::DisablePrincipal => SYS_SECURITY_DISABLE_PRINCIPAL_FUNCTION_ID,
        SecurityAdminAuditOperation::CreateRole => SYS_SECURITY_CREATE_ROLE_FUNCTION_ID,
        SecurityAdminAuditOperation::GrantRole => SYS_SECURITY_GRANT_ROLE_FUNCTION_ID,
        SecurityAdminAuditOperation::RevokeRole => SYS_SECURITY_REVOKE_ROLE_FUNCTION_ID,
        SecurityAdminAuditOperation::GrantPrivilege => SYS_SECURITY_GRANT_PRIVILEGE_FUNCTION_ID,
        SecurityAdminAuditOperation::RevokePrivilege => SYS_SECURITY_REVOKE_PRIVILEGE_FUNCTION_ID,
    }
}

pub(super) fn encode_security_admin_audit_denied_detail_value(
    operation: SecurityAdminAuditOperation,
    reason: PrivilegeDenial,
) -> String {
    format!(
        "security_admin:{}:{}",
        encode_security_admin_audit_operation(operation),
        match reason {
            PrivilegeDenial::MissingPrivilege { .. } => "missing-privilege",
        }
    )
}

fn require_audit_value<T>(
    value: Option<T>,
    record: &str,
    rule: &'static str,
) -> Result<T, PostgresKernelError> {
    value.ok_or_else(|| audit_invariant(record, rule))
}

pub(super) fn require_invocation_audit_value<T>(
    value: Option<T>,
    record: &str,
    rule: &'static str,
) -> Result<T, PostgresKernelError> {
    value.ok_or_else(|| invocation_audit_invariant(record, rule))
}

pub(super) fn invocation_audit_optional_id(
    row: &Row,
    record: &str,
    column: &'static str,
) -> Result<Option<[u8; 16]>, PostgresKernelError> {
    let value: Option<Vec<u8>> = invocation_audit_column(row, record, column)?;
    value
        .map(|bytes| {
            bytes.try_into().map_err(|_| {
                invocation_audit_invariant(
                    record,
                    "invocation audit identity must be exactly sixteen bytes",
                )
            })
        })
        .transpose()
}

pub(super) fn invocation_audit_id(
    row: &Row,
    record: &str,
    column: &'static str,
) -> Result<[u8; 16], PostgresKernelError> {
    let bytes: Vec<u8> = invocation_audit_column(row, record, column)?;
    bytes.try_into().map_err(|_| {
        invocation_audit_invariant(
            record,
            "invocation audit identity must be exactly sixteen bytes",
        )
    })
}

pub(super) fn invocation_audit_column<T: FromSqlOwned>(
    row: &Row,
    record: &str,
    column: &'static str,
) -> Result<T, PostgresKernelError> {
    row.try_get(column)
        .map_err(|source| PostgresKernelError::RowDecode {
            relation: "_orna_kernel.invocation_audit_events",
            record: record.to_owned(),
            column,
            rule: "invocation audit column must use its exact PostgreSQL type",
            source,
        })
}

pub(super) fn invocation_audit_invariant(record: &str, rule: &'static str) -> PostgresKernelError {
    PostgresKernelError::DurableInvariant {
        relation: "_orna_kernel.invocation_audit_events",
        record: record.to_owned(),
        rule,
    }
}

pub(super) fn audit_optional_id(
    row: &Row,
    record: &str,
    column: &'static str,
) -> Result<Option<[u8; 16]>, PostgresKernelError> {
    let value: Option<Vec<u8>> = audit_column(row, record, column)?;
    value
        .map(|bytes| {
            bytes.try_into().map_err(|_| {
                audit_invariant(record, "audit identity must be exactly sixteen bytes")
            })
        })
        .transpose()
}

pub(super) fn audit_id(
    row: &Row,
    record: &str,
    column: &'static str,
) -> Result<[u8; 16], PostgresKernelError> {
    let bytes: Vec<u8> = audit_column(row, record, column)?;
    bytes
        .try_into()
        .map_err(|_| audit_invariant(record, "audit event identity must be exactly sixteen bytes"))
}

fn audit_column<T: FromSqlOwned>(
    row: &Row,
    record: &str,
    column: &'static str,
) -> Result<T, PostgresKernelError> {
    row.try_get(column)
        .map_err(|source| PostgresKernelError::RowDecode {
            relation: "_orna_kernel.security_audit_events",
            record: record.to_owned(),
            column,
            rule: "security audit column must use its exact PostgreSQL type",
            source,
        })
}

pub(super) fn audit_invariant(record: &str, rule: &'static str) -> PostgresKernelError {
    PostgresKernelError::DurableInvariant {
        relation: "_orna_kernel.security_audit_events",
        record: record.to_owned(),
        rule,
    }
}

pub(super) fn exact_id(
    row: &Row,
    column: &'static str,
    rule: &'static str,
) -> Result<[u8; 16], PostgresKernelError> {
    let bytes: Vec<u8> = row.try_get(column).map_err(|source| {
        row_decode(
            "_orna_kernel security snapshot",
            "selected row".to_owned(),
            column,
            source,
        )
    })?;
    bytes
        .try_into()
        .map_err(|_| PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel security snapshot",
            record: "selected row".to_owned(),
            rule,
        })
}

pub(super) fn row_decode(
    relation: &'static str,
    record: String,
    column: &'static str,
    source: tokio_postgres::Error,
) -> PostgresKernelError {
    PostgresKernelError::RowDecode {
        relation,
        record,
        column,
        rule: "security snapshot column must use its exact PostgreSQL type",
        source,
    }
}

pub(crate) fn encode_principal_kind(kind: PrincipalKind) -> &'static str {
    match kind {
        PrincipalKind::User => "user",
        PrincipalKind::Role => "role",
        PrincipalKind::Service => "service",
    }
}

pub(super) fn decode_principal_kind(value: String) -> Result<PrincipalKind, PostgresKernelError> {
    match value.as_str() {
        "user" => Ok(PrincipalKind::User),
        "role" => Ok(PrincipalKind::Role),
        "service" => Ok(PrincipalKind::Service),
        _ => Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.security_principals",
            record: value,
            rule: "principal kind must be user, role, or service",
        }),
    }
}

pub(super) fn encode_principal_status(status: PrincipalStatus) -> &'static str {
    match status {
        PrincipalStatus::Active => "active",
        PrincipalStatus::Disabled => "disabled",
    }
}

pub(super) fn decode_principal_status(
    value: String,
) -> Result<PrincipalStatus, PostgresKernelError> {
    match value.as_str() {
        "active" => Ok(PrincipalStatus::Active),
        "disabled" => Ok(PrincipalStatus::Disabled),
        _ => Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.security_principals",
            record: value,
            rule: "principal status must be active or disabled",
        }),
    }
}
