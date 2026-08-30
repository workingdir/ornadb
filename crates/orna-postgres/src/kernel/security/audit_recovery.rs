//! Protected audit recovery and validation.

use super::*;

pub(super) async fn load_security_audit_events(
    transaction: &Transaction<'_>,
) -> Result<Vec<SecurityAuditEvent>, PostgresKernelError> {
    require_security_audit_relation_columns(transaction).await?;
    let events = transaction
        .query(
            "SELECT sequence, event_id, recorded_at, event_kind, outcome,
                session_principal_id, effective_principal_id,
                authorising_principal_id, function_id, source_revision_id,
                catalogue_revision_id, denial_reason
         FROM _orna_kernel.security_audit_events
         ORDER BY sequence",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?
        .iter()
        .map(decode_security_audit_event)
        .collect::<Result<Vec<_>, _>>()?;
    for event in &events {
        if let Some(candidate) = event.decision().source_apply_candidate() {
            require_source_apply_audit_target(
                transaction,
                candidate,
                &event.sequence().to_string(),
            )
            .await?;
        }
    }
    Ok(events)
}

pub(super) async fn require_source_apply_audit_target(
    transaction: &Transaction<'_>,
    candidate: RevisionPair,
    record: &str,
) -> Result<(), PostgresKernelError> {
    let source = candidate.source().to_bytes().to_vec();
    let catalogue = candidate.catalogue().to_bytes().to_vec();
    let exists = transaction
        .query_opt(
            "SELECT 1
         FROM _orna_kernel.catalogue_revisions AS catalogue
         JOIN _orna_kernel.source_revisions AS source
           ON source.id = catalogue.source_revision_id
         WHERE catalogue.id = $1
           AND catalogue.source_revision_id = $2",
            &[&catalogue, &source],
        )
        .await
        .map_err(PostgresKernelError::Database)?
        .is_some();
    if !exists {
        return Err(audit_invariant(
            record,
            "source apply audit target pair must exist in protected revisions",
        ));
    }
    Ok(())
}

/// Validates every durable protected invocation decision during normal recovery.
///
/// The caller has already recovered one pinned active revision in the same
/// read-only transaction. This function validates the historical target pair,
/// complete row shape, and exact linked `EXECUTE` evidence without repairing
/// any durable state.
pub(crate) async fn recover_invocation_audit_events(
    transaction: &Transaction<'_>,
    _active: &ActiveDatabaseRevision,
) -> Result<(), PostgresKernelError> {
    require_invocation_audit_relation_columns(transaction).await?;
    let security_events = load_security_audit_events(transaction).await?;
    let rows = transaction
        .query(
            "SELECT sequence, event_id, recorded_at, invocation_id, outcome,
                    session_principal_id, effective_principal_id,
                    authorising_principal_id, function_id, source_revision_id,
                    catalogue_revision_id, security_audit_event_id
             FROM _orna_kernel.invocation_audit_events
             ORDER BY sequence",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    for row in &rows {
        let decision = decode_invocation_audit_decision(row)?;
        let record = decision.invocation.canonical();
        validate_invocation_audit_decision_shape(&decision, &record)?;
        validate_invocation_audit_evidence(&decision, &security_events, &record)?;
        if let Some(target) = decision.target {
            require_invocation_audit_target(transaction, target, &record).await?;
        }
    }
    recover_resource_audit_events(transaction).await?;
    Ok(())
}

pub(super) async fn recover_resource_audit_events(
    transaction: &Transaction<'_>,
) -> Result<(), PostgresKernelError> {
    require_resource_audit_relation_columns(transaction).await?;
    let rows = transaction
        .query(
            "SELECT sequence, event_id, recorded_at, request_id,
                nested_invocation_id, parent_invocation_id, call_site_id,
                target_function_id, source_revision_id, catalogue_revision_id,
                session_principal_id, decision_outcome, terminal_outcome,
                item_count, byte_count
         FROM _orna_kernel.resource_audit_events
         ORDER BY sequence",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let mut request_ids = BTreeSet::new();
    let mut nested_invocation_ids = BTreeSet::new();
    let mut event_ids = BTreeSet::new();
    for row in &rows {
        let sequence: i64 = resource_audit_column(row, "selected row", "sequence")?;
        let record = sequence.to_string();
        if sequence <= 0 {
            return Err(resource_audit_invariant(
                &record,
                "generated resource audit sequence must be positive",
            ));
        }
        let _: SystemTime = resource_audit_column(row, &record, "recorded_at")?;
        let event_identity = resource_audit_id(row, &record, "event_id")?;
        let request_identity = resource_audit_id(row, &record, "request_id")?;
        let nested_identity = resource_audit_optional_id(row, &record, "nested_invocation_id")?;
        let request_id = InvocationId::from_bytes(request_identity);
        let nested = nested_identity.map(InvocationId::from_bytes);
        let parent_invocation_id = resource_audit_id(row, &record, "parent_invocation_id")?;
        let call_site_id = resource_audit_id(row, &record, "call_site_id")?;
        validate_resource_audit_lineage(
            &record,
            request_id.to_bytes(),
            nested.map(InvocationId::to_bytes),
            parent_invocation_id,
            call_site_id,
        )?;
        if !request_ids.insert(request_identity) {
            return Err(resource_audit_invariant(
                &record,
                "resource request identity must be unique during recovery",
            ));
        }
        if let Some(nested_identity) = nested_identity
            && !nested_invocation_ids.insert(nested_identity)
        {
            return Err(resource_audit_invariant(
                &record,
                "resource nested invocation identity must be unique during recovery",
            ));
        }
        let request_id_bytes = request_id.to_bytes().to_vec();
        let reservation = transaction
            .query_opt(
                "SELECT request_id
             FROM _orna_kernel.resource_request_history
             WHERE request_id = $1",
                &[&request_id_bytes],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
        if reservation.is_none() {
            return Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.resource_request_history",
                record: request_id.canonical(),
                rule: "accepted resource producer must retain its reservation",
            });
        }
        let target_function = resource_audit_optional_id(row, &record, "target_function_id")?
            .map(FunctionId::from_bytes);
        let source = resource_audit_optional_id(row, &record, "source_revision_id")?
            .map(SourceRevisionId::from_bytes);
        let catalogue = resource_audit_optional_id(row, &record, "catalogue_revision_id")?
            .map(CatalogueRevisionId::from_bytes);
        if (
            target_function.is_some(),
            source.is_some(),
            catalogue.is_some(),
        ) != (true, true, true)
            && (
                target_function.is_some(),
                source.is_some(),
                catalogue.is_some(),
            ) != (false, false, false)
        {
            return Err(resource_audit_invariant(
                &record,
                "target and pinned revision evidence must be present together",
            ));
        }
        let session_principal =
            PrincipalId::from_bytes(resource_audit_id(row, &record, "session_principal_id")?);
        let decision: String = resource_audit_column(row, &record, "decision_outcome")?;
        let terminal: String = resource_audit_column(row, &record, "terminal_outcome")?;
        if !matches!(decision.as_str(), "allowed" | "denied") {
            return Err(resource_audit_invariant(
                &record,
                "resource decision outcome must be allowed or denied",
            ));
        }
        if !matches!(terminal.as_str(), "completed" | "failed" | "cancelled") {
            return Err(resource_audit_invariant(
                &record,
                "resource terminal outcome must be completed, failed, or cancelled",
            ));
        }
        let item_count: Option<i64> = resource_audit_column(row, &record, "item_count")?;
        let byte_count: Option<i64> = resource_audit_column(row, &record, "byte_count")?;
        if item_count.is_some_and(|count| count < 0) || byte_count.is_some_and(|count| count < 0) {
            return Err(resource_audit_invariant(
                &record,
                "resource audit counts must be non-negative",
            ));
        }
        if terminal != "completed" && (item_count.is_some() || byte_count.is_some()) {
            return Err(resource_audit_invariant(
                &record,
                "only completed resource audits may retain result counts",
            ));
        }
        if terminal == "completed" && decision != "allowed" {
            return Err(resource_audit_invariant(
                &record,
                "completed resource audit requires an allowed decision",
            ));
        }
        if nested.is_none()
            && (decision != "denied" || !matches!(terminal.as_str(), "failed" | "cancelled"))
        {
            return Err(resource_audit_invariant(
                &record,
                "resource audit without nested invocation must be a preaccept denied or cancelled terminal",
            ));
        }
        if let Some(nested) = nested {
            let invocation = transaction
                .query_opt(
                    "SELECT outcome, session_principal_id, function_id,
                        source_revision_id, catalogue_revision_id
                 FROM _orna_kernel.invocation_audit_events
                 WHERE invocation_id = $1",
                    &[&nested.to_bytes().to_vec()],
                )
                .await
                .map_err(PostgresKernelError::Database)?
                .ok_or_else(|| {
                    resource_audit_invariant(
                        &record,
                        "nested resource invocation audit evidence is missing",
                    )
                })?;
            let invocation_outcome: String =
                resource_audit_column(&invocation, &record, "outcome")?;
            let expected_invocation_outcome = decision.as_str();
            if invocation_outcome != expected_invocation_outcome {
                return Err(resource_audit_invariant(
                    &record,
                    "nested invocation outcome does not match resource decision",
                ));
            }
            let invocation_session: Vec<u8> =
                resource_audit_column(&invocation, &record, "session_principal_id")?;
            let resource_target_present =
                target_function.is_some() && source.is_some() && catalogue.is_some();
            if invocation_outcome == "allowed" && !resource_target_present {
                return Err(resource_audit_invariant(
                    &record,
                    "allowed nested invocation requires resource audit target evidence",
                ));
            }
            if invocation_session != session_principal.to_bytes() {
                return Err(resource_audit_invariant(
                    &record,
                    "nested invocation session principal does not match resource audit",
                ));
            }
            if let (Some(function), Some(source), Some(catalogue)) =
                (target_function, source, catalogue)
            {
                let invocation_function: Option<Vec<u8>> =
                    resource_audit_column(&invocation, &record, "function_id")?;
                let invocation_source: Option<Vec<u8>> =
                    resource_audit_column(&invocation, &record, "source_revision_id")?;
                let invocation_catalogue: Option<Vec<u8>> =
                    resource_audit_column(&invocation, &record, "catalogue_revision_id")?;
                if invocation_function.as_deref() != Some(function.to_bytes().as_slice())
                    || invocation_source.as_deref() != Some(source.to_bytes().as_slice())
                    || invocation_catalogue.as_deref() != Some(catalogue.to_bytes().as_slice())
                {
                    return Err(resource_audit_invariant(
                        &record,
                        "nested invocation target does not match resource audit",
                    ));
                }
                require_invocation_audit_target(
                    transaction,
                    InvocationTarget::new(function, RevisionPair::new(source, catalogue)),
                    &record,
                )
                .await?;
            }
        }
        if !event_ids.insert(event_identity) {
            return Err(resource_audit_invariant(
                &record,
                "resource event identity must be unique during recovery",
            ));
        }
    }
    Ok(())
}

pub(super) async fn require_resource_audit_relation_columns(
    transaction: &Transaction<'_>,
) -> Result<(), PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT attribute.attname
         FROM pg_catalog.pg_attribute AS attribute
         JOIN pg_catalog.pg_class AS class ON class.oid = attribute.attrelid
         JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = class.relnamespace
         WHERE namespace.nspname = '_orna_kernel'
           AND class.relname = 'resource_audit_events'
           AND attribute.attnum > 0
           AND NOT attribute.attisdropped
         ORDER BY attribute.attnum",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let names = rows
        .iter()
        .map(|row| resource_audit_column(row, "relation", "attname"))
        .collect::<Result<Vec<String>, _>>()?;
    let expected = [
        "sequence",
        "event_id",
        "recorded_at",
        "request_id",
        "nested_invocation_id",
        "parent_invocation_id",
        "call_site_id",
        "target_function_id",
        "source_revision_id",
        "catalogue_revision_id",
        "session_principal_id",
        "decision_outcome",
        "terminal_outcome",
        "item_count",
        "byte_count",
    ];
    if names != expected {
        return Err(resource_audit_invariant(
            "relation",
            "resource audit relation has unsupported disclosure-bearing columns",
        ));
    }
    Ok(())
}

pub(super) fn resource_audit_optional_id(
    row: &Row,
    record: &str,
    column: &'static str,
) -> Result<Option<[u8; 16]>, PostgresKernelError> {
    let bytes: Option<Vec<u8>> = resource_audit_column(row, record, column)?;
    bytes
        .map(|bytes| {
            bytes.try_into().map_err(|_| {
                resource_audit_invariant(
                    record,
                    "resource audit identity must be exactly sixteen bytes",
                )
            })
        })
        .transpose()
}

pub(super) fn resource_audit_id(
    row: &Row,
    record: &str,
    column: &'static str,
) -> Result<[u8; 16], PostgresKernelError> {
    let bytes: Vec<u8> = resource_audit_column(row, record, column)?;
    bytes.try_into().map_err(|_| {
        resource_audit_invariant(
            record,
            "resource audit identity must be exactly sixteen bytes",
        )
    })
}

fn resource_audit_column<T: FromSqlOwned>(
    row: &Row,
    record: &str,
    column: &'static str,
) -> Result<T, PostgresKernelError> {
    row.try_get(column)
        .map_err(|source| PostgresKernelError::RowDecode {
            relation: "_orna_kernel.resource_audit_events",
            record: record.to_owned(),
            column,
            rule: "resource audit column must use its exact PostgreSQL type",
            source,
        })
}

pub(super) fn decode_invocation_audit_decision(
    row: &Row,
) -> Result<InvocationAuditDecision, PostgresKernelError> {
    let sequence: i64 = invocation_audit_column(row, "selected row", "sequence")?;
    let record = sequence.to_string();
    if sequence <= 0 {
        return Err(invocation_audit_invariant(
            &record,
            "generated invocation audit sequence must be positive",
        ));
    }
    let _: SystemTime = invocation_audit_column(row, &record, "recorded_at")?;
    let _ = InvocationAuditEventId::from_bytes(invocation_audit_id(row, &record, "event_id")?);
    let invocation = InvocationId::from_bytes(invocation_audit_id(row, &record, "invocation_id")?);
    let outcome = decode_invocation_audit_outcome(
        invocation_audit_column(row, &record, "outcome")?,
        &record,
    )?;
    let session_principal =
        PrincipalId::from_bytes(invocation_audit_id(row, &record, "session_principal_id")?);
    let effective_principal = invocation_audit_optional_id(row, &record, "effective_principal_id")?
        .map(PrincipalId::from_bytes);
    let authorising_principal =
        invocation_audit_optional_id(row, &record, "authorising_principal_id")?
            .map(PrincipalId::from_bytes);
    let function =
        invocation_audit_optional_id(row, &record, "function_id")?.map(FunctionId::from_bytes);
    let source_revision = invocation_audit_optional_id(row, &record, "source_revision_id")?
        .map(SourceRevisionId::from_bytes);
    let catalogue_revision = invocation_audit_optional_id(row, &record, "catalogue_revision_id")?
        .map(CatalogueRevisionId::from_bytes);
    let security_audit_event =
        invocation_audit_optional_id(row, &record, "security_audit_event_id")?
            .map(SecurityAuditEventId::from_bytes);
    let target = match (function, source_revision, catalogue_revision) {
        (Some(function), Some(source), Some(catalogue)) => Some(InvocationTarget::new(
            function,
            RevisionPair::new(source, catalogue),
        )),
        (None, None, None) => None,
        _ => {
            return Err(invocation_audit_invariant(
                &record,
                "target and pinned revision evidence must be present together",
            ));
        }
    };
    Ok(InvocationAuditDecision {
        invocation,
        outcome,
        session_principal,
        effective_principal,
        authorising_principal,
        target,
        security_audit_event,
    })
}

pub(super) fn validate_invocation_audit_decision_shape(
    decision: &InvocationAuditDecision,
    record: &str,
) -> Result<(), PostgresKernelError> {
    if decision.effective_principal.is_some() != decision.authorising_principal.is_some() {
        return Err(invocation_audit_invariant(
            record,
            "effective and authorising principals must be present together",
        ));
    }
    if decision.target.is_some() != decision.security_audit_event.is_some() {
        return Err(invocation_audit_invariant(
            record,
            "target, pinned revision, and security audit evidence must be present together",
        ));
    }
    match (
        decision.outcome,
        decision.target,
        decision.effective_principal,
    ) {
        (SecurityAuditOutcome::Allowed, Some(_), Some(_)) => Ok(()),
        (SecurityAuditOutcome::Allowed, _, _) => Err(invocation_audit_invariant(
            record,
            "allowed invocation decision requires target and principal evidence",
        )),
        (SecurityAuditOutcome::Denied, None, None) => Ok(()),
        (SecurityAuditOutcome::Denied, Some(_), _) => Ok(()),
        (SecurityAuditOutcome::Denied, None, Some(_)) => Err(invocation_audit_invariant(
            record,
            "unresolved denied invocation cannot retain principal evidence",
        )),
    }
}

pub(super) fn validate_invocation_audit_evidence(
    decision: &InvocationAuditDecision,
    security_events: &[SecurityAuditEvent],
    record: &str,
) -> Result<(), PostgresKernelError> {
    let Some(event_id) = decision.security_audit_event else {
        return Ok(());
    };
    let evidence = security_events
        .iter()
        .find(|event| event.id() == event_id)
        .ok_or_else(|| {
            invocation_audit_invariant(record, "linked security audit evidence is missing")
        })?;
    let security = evidence.decision();
    if security.kind() != SecurityAuditKind::Execute
        || security.outcome() != decision.outcome
        || security.session_principal() != Some(decision.session_principal)
        || security.effective_principal() != decision.effective_principal
        || security.authorising_principal() != decision.authorising_principal
        || security.target() != decision.target
    {
        return Err(invocation_audit_invariant(
            record,
            "linked security audit evidence does not match the invocation decision",
        ));
    }
    Ok(())
}

/// Validates one protected invocation-audit target through the durable
/// target-authority relation without writing or repairing any row.
///
/// The audited `RevisionPair` stays the durable standard pin: an `application`
/// authority row must resolve the function and its pinned executable revision
/// in that historical application catalogue, a `standard` authority row must
/// resolve the audited function and its executable revision exactly once in the
/// exact verified standard snapshot pinned by that historical catalogue
/// revision, and a `system` authority row must identify an admitted sealed
/// security identity with the identity repeated as its function revision and no
/// standard-library pin. An absent authority row, mismatched revision pair,
/// wrong standard pin, absent or duplicate standard executable, or a row whose
/// class cannot resolve fails closed.
pub(super) async fn require_invocation_audit_target(
    transaction: &Transaction<'_>,
    target: InvocationTarget,
    record: &str,
) -> Result<(), PostgresKernelError> {
    let function = target.function().to_bytes().to_vec();
    let source = target.revision().source().to_bytes().to_vec();
    let catalogue = target.revision().catalogue().to_bytes().to_vec();
    let row = transaction
        .query_opt(
            "SELECT authority.target_class AS target_class,
                authority.function_revision_id AS pinned_function_revision_id,
                authority.standard_library_revision_id AS pinned_standard_library_revision_id,
                revision.source_revision_id AS catalogue_source_revision_id,
                revision.standard_library_revision_id AS catalogue_standard_library_revision_id,
                function.current_function_revision_id AS catalogue_current_function_revision_id
         FROM _orna_kernel.invocation_target_authorities AS authority
         JOIN _orna_kernel.catalogue_revisions AS revision
           ON revision.id = authority.catalogue_revision_id
         LEFT JOIN _orna_kernel.catalogue_functions AS function
           ON function.catalogue_revision_id = authority.catalogue_revision_id
          AND function.function_id = authority.function_id
         WHERE authority.catalogue_revision_id = $1
           AND authority.function_id = $2",
            &[&catalogue, &function],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let Some(row) = row else {
        return Err(invocation_audit_invariant(
            record,
            "target function and pinned revision must exist together",
        ));
    };
    let relation = "_orna_kernel.invocation_audit_events";
    let catalogue_source: Vec<u8> =
        row.try_get("catalogue_source_revision_id")
            .map_err(|source| {
                row_decode(
                    relation,
                    record.to_owned(),
                    "catalogue_source_revision_id",
                    source,
                )
            })?;
    if catalogue_source != source {
        return Err(invocation_audit_invariant(
            record,
            "target function and pinned revision must exist together",
        ));
    }
    let class: String = row
        .try_get("target_class")
        .map_err(|source| row_decode(relation, record.to_owned(), "target_class", source))?;
    let pinned_revision: Vec<u8> =
        row.try_get("pinned_function_revision_id")
            .map_err(|source| {
                row_decode(
                    relation,
                    record.to_owned(),
                    "pinned_function_revision_id",
                    source,
                )
            })?;
    match class.as_str() {
        "system" => {
            let catalogue_current: Option<Vec<u8>> = row
                .try_get("catalogue_current_function_revision_id")
                .map_err(|source| {
                    row_decode(
                        relation,
                        record.to_owned(),
                        "catalogue_current_function_revision_id",
                        source,
                    )
                })?;
            let admitted =
                system_function_by_id(target.function()).is_some_and(is_admitted_security_identity);
            let standard_revision: Option<Vec<u8>> = row
                .try_get("pinned_standard_library_revision_id")
                .map_err(|source| {
                    row_decode(
                        relation,
                        record.to_owned(),
                        "pinned_standard_library_revision_id",
                        source,
                    )
                })?;
            if catalogue_current.is_some()
                || !admitted
                || pinned_revision != function
                || standard_revision.is_some()
            {
                return Err(invocation_audit_invariant(
                    record,
                    "target function and pinned revision must exist together",
                ));
            }
        }
        "application" => {
            let current: Option<Vec<u8>> = row
                .try_get("catalogue_current_function_revision_id")
                .map_err(|source| {
                    row_decode(
                        relation,
                        record.to_owned(),
                        "catalogue_current_function_revision_id",
                        source,
                    )
                })?;
            if current.as_deref() != Some(pinned_revision.as_slice()) {
                return Err(invocation_audit_invariant(
                    record,
                    "target function and pinned revision must exist together",
                ));
            }
        }
        "standard" => {
            let pinned_standard: Option<Vec<u8>> = row
                .try_get("pinned_standard_library_revision_id")
                .map_err(|source| {
                    row_decode(
                        relation,
                        record.to_owned(),
                        "pinned_standard_library_revision_id",
                        source,
                    )
                })?;
            let catalogue_standard: Option<Vec<u8>> = row
                .try_get("catalogue_standard_library_revision_id")
                .map_err(|source| {
                    row_decode(
                        relation,
                        record.to_owned(),
                        "catalogue_standard_library_revision_id",
                        source,
                    )
                })?;
            if pinned_standard.as_deref() != catalogue_standard.as_deref() {
                return Err(invocation_audit_invariant(
                    record,
                    "target function and pinned revision must exist together",
                ));
            }
            let bytes = catalogue_standard.ok_or_else(|| {
                invocation_audit_invariant(
                    record,
                    "target function and pinned revision must exist together",
                )
            })?;
            let standard_revision =
                StandardLibraryRevisionId::from_bytes(bytes.try_into().map_err(|_| {
                    invocation_audit_invariant(
                        record,
                        "target function and pinned revision must exist together",
                    )
                })?);
            let standard = load_verified_standard_library(transaction, standard_revision)
                .await
                .map_err(|_| {
                    invocation_audit_invariant(
                        record,
                        "target function and pinned revision must exist together",
                    )
                })?;
            let mut matches = standard
                .executables()
                .iter()
                .filter(|executable| executable.function() == target.function())
                .map(|executable| executable.revision().id().to_bytes().to_vec())
                .collect::<Vec<_>>();
            matches.sort_unstable();
            matches.dedup();
            if matches.len() != 1 || matches[0] != pinned_revision {
                return Err(invocation_audit_invariant(
                    record,
                    "target function and pinned revision must exist together",
                ));
            }
        }
        _ => {
            return Err(invocation_audit_invariant(
                record,
                "target function and pinned revision must exist together",
            ));
        }
    }
    Ok(())
}
