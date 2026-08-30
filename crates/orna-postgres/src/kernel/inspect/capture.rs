//! Protected transaction that captures one immutable INSPECT epoch.

use super::payload::encode_epoch_payload;
use super::projection::{build_inspect_epoch, durable_trace_kind};
use super::*;

/// Captures one inspection epoch and its trace rows in the caller's
/// protected transaction.
///
/// The sealed dispatch runs this inside its own transaction so the snapshot
/// and trace rows share the invocation-audit evidence of the same commit.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn capture_inspect_snapshot_in_transaction(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    authenticated_session: &AuthenticatedSession,
    invocation: InvocationId,
    options: InspectSnapshotOptions,
    owner: PrincipalId,
    root_target: FunctionId,
    outcome: InspectOutcomeKind,
    events: &InvocationEventBatch,
    client_offer: Option<&InvocationClientOffer>,
    observer_invocation: Option<InvocationId>,
    loaded_user_state_cells: Option<&[UserStateCell]>,
    output_requirement: Option<&InvocationOutputRequirement>,
) -> Result<InspectEpochId, PostgresKernelError> {
    // Capture the protected decision evidence before writing the immutable
    // epoch. The linked EXECUTE row admits the invocation and the INSPECT
    // row records this capture; both are copied into the epoch payload so a
    // later projection never has to consult mutable audit history.
    let inspect_decision = InspectDecision::Allowed {
        epoch_scope: InspectEpochScope::Own,
        requested: InspectPrivilege::OwnInvocation,
    };
    let inspect_audit = SecurityAuditDecision::inspect_allowed(
        authenticated_session,
        inspect_decision,
        Some(owner),
    )
    .map_err(|_| PostgresKernelError::DurableInvariant {
        relation: "_orna_kernel.security_audit_events",
        record: invocation.canonical(),
        rule: "the capture decision must be an allowed INSPECT decision",
    })?;
    let inspect_audit_id = append_security_audit_event(transaction, inspect_audit).await?;
    let security_decisions =
        capture_security_decisions_in_transaction(transaction, invocation, inspect_audit_id)
            .await?;
    let state_cells = match loaded_user_state_cells {
        Some(cells) => state_cell_rows_from_loaded_user_state(cells)?,
        None => {
            capture_state_cells_in_transaction(transaction, active, registry, owner, root_target)
                .await?
        }
    };
    let epoch = build_inspect_epoch(
        active,
        invocation,
        options,
        owner,
        root_target,
        outcome,
        events,
        client_offer,
        state_cells,
        security_decisions,
        output_requirement,
    )?;
    let epoch_id = epoch.id();
    let recorded_at = epoch.recorded_at();

    let payload = encode_epoch_payload(active, registry, &epoch)?;
    let summary_bytes = encode_constructed_value(active, registry, &RuntimeValue::Bytes(payload))
        .map_err(PostgresKernelError::InspectValueCodec)?;
    let invocation_id = invocation.to_bytes().to_vec();
    let observer_context = epoch.observer_context();
    let observer_root =
        observer_context.map(|context| context.observer_root_invocation_id().to_bytes().to_vec());
    let observer_parent =
        observer_context.map(|context| context.observer_parent_invocation_id().to_bytes().to_vec());
    let observer_purpose = observer_context.map(|context| context.purpose().as_str().to_owned());
    transaction
        .execute(
            INSPECT_SNAPSHOT_INSERT,
            &[
                &epoch_id.to_bytes().to_vec(),
                &invocation_id,
                &recorded_at,
                &epoch.owner().to_bytes().to_vec(),
                &epoch.source_revision_id().to_bytes().to_vec(),
                &epoch.catalogue_revision_id().to_bytes().to_vec(),
                &summary_bytes,
                &observer_root,
                &observer_parent,
                &observer_purpose,
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    for event_record in events.records() {
        let event = event_record.event();
        let Some(kind) = durable_trace_kind(event.body()) else {
            return Err(PostgresKernelError::DurableInvariant {
                relation: INSPECT_TRACE_RELATION,
                record: invocation.canonical(),
                rule: "captured event body must carry a durable stream kind",
            });
        };
        let payload =
            encode_constructed_value(active, registry, &RuntimeValue::InvokeEvent(event.clone()))
                .map_err(PostgresKernelError::InspectValueCodec)?;
        persist_trace_row(
            transaction,
            invocation,
            event.sequence(),
            kind,
            &payload,
            observer_invocation,
            recorded_at,
        )
        .await?;
    }
    let snapshot_sequence = events.records().len() as u64;
    let payload = encode_constructed_value(
        active,
        registry,
        &RuntimeValue::Bytes(epoch_id.to_bytes().to_vec()),
    )
    .map_err(PostgresKernelError::InspectValueCodec)?;
    persist_trace_row(
        transaction,
        invocation,
        snapshot_sequence,
        "inspect_snapshot",
        &payload,
        observer_invocation,
        recorded_at,
    )
    .await?;

    Ok(epoch_id)
}

/// Converts the exact USER cells loaded for a sealed CLIENT evaluation into
/// immutable Inspector rows. Resource retries run after the load transaction
/// commits, so their capture must not consult mutable USER state again.
fn state_cell_rows_from_loaded_user_state(
    cells: &[UserStateCell],
) -> Result<Vec<StateCellRow>, PostgresKernelError> {
    cells
        .iter()
        .map(|cell| {
            if is_sealed_inspect_type_id(cell.value_type())
                || is_sealed_inspect_runtime_value(cell.value())
            {
                return Err(PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.user_state_cells",
                    record: cell.key().to_string(),
                    rule: "USER state cannot expose sealed Inspector values",
                });
            }
            let value = InvokeValue::new(cell.value().clone())
                .map_err(PostgresKernelError::InvocationCarrier)?;
            Ok(StateCellRow::new(
                cell.key().without_principal(),
                cell.value_type(),
                cell.revision(),
                cell.updated_at(),
                Some(value),
            ))
        })
        .collect()
}

/// Captures the exact USER state-cell rows visible in the capture transaction.
///
/// The resulting rows are embedded in the canonical epoch payload; later
/// projections must never consult mutable live state for an existing epoch.
async fn capture_state_cells_in_transaction(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    owner: PrincipalId,
    root_target: FunctionId,
) -> Result<Vec<StateCellRow>, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT principal_id, root_function_id, root_state_profile,
                    function_id, function_instance_key, state_slot_id,
                    value_bytes, value_type_id, revision, updated_at
             FROM _orna_kernel.user_state_cells
             WHERE principal_id = $1
               AND root_function_id = $2
             ORDER BY root_state_profile, function_id,
                      function_instance_key, state_slot_id",
            &[&owner.to_bytes().to_vec(), &root_target.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    rows.iter()
        .map(|row| decode_state_cell_row(row, active, registry))
        .collect()
}

/// Copies the protected EXECUTE evidence linked to an invocation and the
/// protected INSPECT decision for this capture into the immutable epoch.
///
/// The query is deliberately performed in the capture transaction. The
/// returned rows are then encoded into summary_bytes; projections never
/// re-read either audit relation for an existing epoch.
async fn capture_security_decisions_in_transaction(
    transaction: &Transaction<'_>,
    invocation: InvocationId,
    inspect_audit_id: SecurityAuditEventId,
) -> Result<Vec<SecurityDecisionRow>, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT security.event_id, security.event_kind,
                    security.outcome, security.denial_reason,
                    security.session_principal_id,
                    security.effective_principal_id,
                    security.authorising_principal_id,
                    security.function_id
             FROM _orna_kernel.security_audit_events AS security
             LEFT JOIN _orna_kernel.invocation_audit_events AS invocation_audit
               ON invocation_audit.security_audit_event_id = security.event_id
             WHERE invocation_audit.invocation_id = $1
                OR security.event_id = $2
             ORDER BY security.sequence",
            &[
                &invocation.to_bytes().to_vec(),
                &inspect_audit_id.to_bytes().to_vec(),
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    if rows.is_empty() {
        return Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.security_audit_events",
            record: invocation.canonical(),
            rule: "captured invocation must retain linked EXECUTE and INSPECT evidence",
        });
    }
    rows.iter().map(decode_security_decision_row).collect()
}

/// Appends one trace row in the caller's protected transaction.
async fn persist_trace_row(
    transaction: &Transaction<'_>,
    invocation: InvocationId,
    sequence: u64,
    kind: &str,
    payload: &[u8],
    observer_invocation: Option<InvocationId>,
    recorded_at: SystemTime,
) -> Result<(), PostgresKernelError> {
    let sequence = i64::try_from(sequence).map_err(|_| PostgresKernelError::DurableInvariant {
        relation: INSPECT_TRACE_RELATION,
        record: invocation.canonical(),
        rule: "trace sequence must fit PostgreSQL BIGINT",
    })?;
    let observer_bytes = observer_invocation.map(|id| id.to_bytes().to_vec());
    transaction
        .execute(
            "INSERT INTO _orna_kernel.inspect_trace_events
                 (invocation_id, sequence, kind, payload_bytes,
                  observer_invocation_id, recorded_at)
             VALUES ($1, $2, $3, $4, $5, $6)",
            &[
                &invocation.to_bytes().to_vec(),
                &sequence,
                &kind,
                &payload,
                &observer_bytes,
                &recorded_at,
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    Ok(())
}
