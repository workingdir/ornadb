//! Protected audit relation validation and row decoding.

use super::*;

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

pub(super) fn require_audit_value<T>(
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
