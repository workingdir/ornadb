//! Durable INSPECT relation recovery and row decoding.

use super::payload::{decode_epoch_payload, encode_epoch_payload};
use super::projection::model_payload_for;
use super::*;

/// Validates the inspection relations during normal recovery.
///
/// The caller has already recovered one pinned active revision in the same
/// read-only transaction. Every snapshot payload must decode through the
/// canonical envelope and round-trip exactly; every trace row must satisfy
/// the identity, sequence, and closed-kind invariants and agree with its
/// canonical event payload when the kind is model-expressible. No durable
/// state is repaired.
pub(crate) async fn recover_inspect_relations(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
) -> Result<(), PostgresKernelError> {
    require_inspect_relation_columns(transaction).await?;

    let rows = transaction
        .query(
            "SELECT epoch_id, invocation_id, recorded_at,
                    owner_principal_id, source_revision_id,
                    catalogue_revision_id, summary_bytes,
                            observer_root_invocation_id, observer_parent_invocation_id,
                            observer_purpose
             FROM _orna_kernel.inspect_snapshots
             ORDER BY epoch_id",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let trace_rows = transaction
        .query(
            "SELECT invocation_id, sequence, kind, payload_bytes,
                    observer_invocation_id, recorded_at
             FROM _orna_kernel.inspect_trace_events
             ORDER BY invocation_id, sequence",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    // A fresh database before the first standard upgrade legitimately has no
    // verified standard snapshot and no inspection relations. The registry is
    // only needed to decode stored payloads, so recovery proceeds without it
    // when both relations are empty; any stored row still requires the
    // accepted standard (fail closed, never silently skipped).
    if rows.is_empty() && trace_rows.is_empty() {
        return Ok(());
    }
    let mut contexts = HashMap::new();
    let mut invocation_pairs = HashMap::new();
    for row in &rows {
        let pair = inspect_snapshot_pair(row)?;
        if !contexts.keys().any(|candidate| *candidate == pair) {
            let context = inspect_codec_context(transaction, active, pair).await?;
            contexts.insert(pair, context);
        }
        let (historical, registry) = contexts
            .get(&pair)
            .expect("inspection codec context was inserted above");
        let epoch = decode_inspect_snapshot_row(row, historical, registry)?;
        remember_inspection_pair(&mut invocation_pairs, epoch.invocation_id(), pair)?;
    }

    for record_row in &trace_rows {
        let record = row_invocation_record(record_row)?;
        if !INSPECT_TRACE_KINDS.contains(&record.kind.as_str()) {
            return Err(PostgresKernelError::DurableInvariant {
                relation: INSPECT_TRACE_RELATION,
                record: record.invocation.canonical(),
                rule: "trace kind is outside the closed durable stream set",
            });
        }
        if record.payload_bytes.is_empty() {
            return Err(PostgresKernelError::DurableInvariant {
                relation: INSPECT_TRACE_RELATION,
                record: record.invocation.canonical(),
                rule: "trace payload must not be empty",
            });
        }
        if record.payload_bytes.len() > MAX_INSPECT_EVIDENCE_BYTES {
            return Err(PostgresKernelError::DurableInvariant {
                relation: INSPECT_TRACE_RELATION,
                record: record.invocation.canonical(),
                rule: "trace payload must fit the durable evidence bound",
            });
        }
        if !matches!(
            record.kind.as_str(),
            "started" | "value_batch" | "completed"
        ) {
            continue;
        }
        let Some(pair) = invocation_pairs.get(&record.invocation) else {
            return Err(PostgresKernelError::DurableInvariant {
                relation: INSPECT_TRACE_RELATION,
                record: record.invocation.canonical(),
                rule: "trace payload must have a pinned inspection snapshot context",
            });
        };
        let (historical, registry) = contexts
            .get(pair)
            .expect("trace context must come from its inspection snapshot");
        let RuntimeValue::InvokeEvent(event) =
            decode_constructed_value(historical, registry, &record.payload_bytes)
                .map_err(PostgresKernelError::InspectValueCodec)?
        else {
            return Err(PostgresKernelError::DurableInvariant {
                relation: INSPECT_TRACE_RELATION,
                record: record.invocation.canonical(),
                rule: "trace payload must decode as one invocation event",
            });
        };
        if event.invocation_id() != record.invocation || event.sequence() != record.sequence {
            return Err(PostgresKernelError::DurableInvariant {
                relation: INSPECT_TRACE_RELATION,
                record: record.invocation.canonical(),
                rule: "trace row must agree with its canonical event payload",
            });
        }
        if model_payload_for(&record.kind, event.body()).is_none() {
            return Err(PostgresKernelError::DurableInvariant {
                relation: INSPECT_TRACE_RELATION,
                record: record.invocation.canonical(),
                rule: "trace kind must agree with its event payload kind",
            });
        }
    }
    Ok(())
}

/// The durable identity and kind facts of one trace row.
pub(super) struct InvocationTraceRecord {
    pub(super) invocation: InvocationId,
    pub(super) sequence: u64,
    pub(super) kind: String,
    pub(super) payload_bytes: Vec<u8>,
    pub(super) observer_invocation: Option<InvocationId>,
    pub(super) recorded_at: SystemTime,
}

pub(super) fn row_invocation_record(
    row: &Row,
) -> Result<InvocationTraceRecord, PostgresKernelError> {
    let invocation = InvocationId::from_bytes(inspect_id(
        INSPECT_TRACE_RELATION,
        row,
        "invocation_id",
        "invocation_id",
    )?);
    let record = invocation.canonical();
    let sequence: i64 = inspect_column(INSPECT_TRACE_RELATION, row, &record, "sequence")?;
    let sequence = u64::try_from(sequence).map_err(|_| PostgresKernelError::DurableInvariant {
        relation: INSPECT_TRACE_RELATION,
        record: record.clone(),
        rule: "trace sequence must be a non-negative unsigned integer",
    })?;
    let kind: String = inspect_column(INSPECT_TRACE_RELATION, row, &record, "kind")?;
    let payload_bytes: Vec<u8> =
        inspect_column(INSPECT_TRACE_RELATION, row, &record, "payload_bytes")?;
    if payload_bytes.is_empty() || payload_bytes.len() > MAX_INSPECT_EVIDENCE_BYTES {
        return Err(PostgresKernelError::DurableInvariant {
            relation: INSPECT_TRACE_RELATION,
            record: record.clone(),
            rule: "trace payload must be non-empty and fit the durable evidence bound",
        });
    }
    let observer_invocation = inspect_optional_id(
        INSPECT_TRACE_RELATION,
        row,
        &record,
        "observer_invocation_id",
    )?
    .map(InvocationId::from_bytes);
    let recorded_at: SystemTime =
        inspect_column(INSPECT_TRACE_RELATION, row, &record, "recorded_at")?;
    Ok(InvocationTraceRecord {
        invocation,
        sequence,
        kind,
        payload_bytes,
        observer_invocation,
        recorded_at,
    })
}

pub(super) fn decode_inspect_snapshot_row(
    row: &Row,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
) -> Result<InspectSnapshotEpoch, PostgresKernelError> {
    let epoch_id = InspectEpochId::from_bytes(inspect_id(
        INSPECT_SNAPSHOT_RELATION,
        row,
        "epoch_id",
        "epoch_id",
    )?);
    let record = epoch_id.canonical();
    let invocation_id = InvocationId::from_bytes(inspect_id(
        INSPECT_SNAPSHOT_RELATION,
        row,
        &record,
        "invocation_id",
    )?);
    let owner = PrincipalId::from_bytes(inspect_id(
        INSPECT_SNAPSHOT_RELATION,
        row,
        &record,
        "owner_principal_id",
    )?);
    let source_revision_id = SourceRevisionId::from_bytes(inspect_id(
        INSPECT_SNAPSHOT_RELATION,
        row,
        &record,
        "source_revision_id",
    )?);
    let catalogue_revision_id = CatalogueRevisionId::from_bytes(inspect_id(
        INSPECT_SNAPSHOT_RELATION,
        row,
        &record,
        "catalogue_revision_id",
    )?);
    let recorded_at: SystemTime =
        inspect_column(INSPECT_SNAPSHOT_RELATION, row, &record, "recorded_at")?;
    let summary_bytes: Vec<u8> =
        inspect_column(INSPECT_SNAPSHOT_RELATION, row, &record, "summary_bytes")?;
    if summary_bytes.is_empty() || summary_bytes.len() > MAX_INSPECT_EVIDENCE_BYTES {
        return Err(PostgresKernelError::DurableInvariant {
            relation: INSPECT_SNAPSHOT_RELATION,
            record: record.clone(),
            rule: "inspection summary must be non-empty and fit the durable evidence bound",
        });
    }
    let observer_root = inspect_optional_id(
        INSPECT_SNAPSHOT_RELATION,
        row,
        &record,
        "observer_root_invocation_id",
    )?;
    let observer_parent = inspect_optional_id(
        INSPECT_SNAPSHOT_RELATION,
        row,
        &record,
        "observer_parent_invocation_id",
    )?;
    let observer_purpose: Option<String> =
        inspect_column(INSPECT_SNAPSHOT_RELATION, row, &record, "observer_purpose")?;
    let column_observer_context = match (observer_root, observer_parent, observer_purpose) {
        (None, None, None) => None,
        (Some(root), Some(parent), Some(purpose)) => {
            if purpose != InspectObserverPurpose::Inspect.as_str() {
                return Err(PostgresKernelError::DurableInvariant {
                    relation: INSPECT_SNAPSHOT_RELATION,
                    record: record.clone(),
                    rule: "observer purpose must be the closed inspect purpose",
                });
            }
            let root = InvocationId::from_bytes(root);
            let parent = InvocationId::from_bytes(parent);
            if root.to_bytes() == [0; 16] || parent.to_bytes() == [0; 16] {
                return Err(PostgresKernelError::DurableInvariant {
                    relation: INSPECT_SNAPSHOT_RELATION,
                    record: record.clone(),
                    rule: "observer context identities must be non-zero",
                });
            }
            Some(InspectObserverContext::new(root, parent).map_err(|_| {
                PostgresKernelError::DurableInvariant {
                    relation: INSPECT_SNAPSHOT_RELATION,
                    record: record.clone(),
                    rule: "observer context identities must be valid",
                }
            })?)
        }
        _ => {
            return Err(PostgresKernelError::DurableInvariant {
                relation: INSPECT_SNAPSHOT_RELATION,
                record: record.clone(),
                rule: "observer context columns must be all NULL or all populated",
            });
        }
    };
    let RuntimeValue::Bytes(payload) = decode_constructed_value(active, registry, &summary_bytes)
        .map_err(PostgresKernelError::InspectValueCodec)?
    else {
        return Err(PostgresKernelError::DurableInvariant {
            relation: INSPECT_SNAPSHOT_RELATION,
            record: record.clone(),
            rule: "the epoch payload must decode as one BYTES value",
        });
    };
    let epoch = decode_epoch_payload(active, registry, &payload, &record)?;
    if epoch.id() != epoch_id
        || epoch.invocation_id() != invocation_id
        || epoch.source_revision_id() != source_revision_id
        || epoch.catalogue_revision_id() != catalogue_revision_id
        || epoch.owner() != owner
        || epoch.observer_context() != column_observer_context
    {
        return Err(PostgresKernelError::DurableInvariant {
            relation: INSPECT_SNAPSHOT_RELATION,
            record: record.clone(),
            rule: "the snapshot row must agree with its canonical epoch payload",
        });
    }
    // PostgreSQL retains timestamptz at microsecond precision, whereas the
    // canonical payload retains the complete capture time.  Bind the stored
    // ordering key to the immutable payload at the precision PostgreSQL can
    // represent.  `find_latest_inspect_epoch` orders on this column, so
    // accepting a different value would let a durable timestamp tamper pick
    // a different epoch without changing the captured evidence.
    if recorded_at != postgres_timestamp_precision(epoch.recorded_at()) {
        return Err(PostgresKernelError::DurableInvariant {
            relation: INSPECT_SNAPSHOT_RELATION,
            record: record.clone(),
            rule: "inspection snapshot timestamp must agree with its canonical epoch payload",
        });
    }
    let reencoded = encode_epoch_payload(active, registry, &epoch)?;
    if reencoded != payload {
        return Err(PostgresKernelError::DurableInvariant {
            relation: INSPECT_SNAPSHOT_RELATION,
            record,
            rule: "the epoch payload is not canonical",
        });
    }
    Ok(epoch)
}

/// Returns the exact value PostgreSQL can retain for a `timestamptz` written
/// from the canonical epoch timestamp.
///
/// The protocol codec represents pre-epoch times as the epoch, and the
/// PostgreSQL driver stores only whole microseconds. Keeping this conversion
/// beside recovery makes that lossy storage boundary explicit and testable.
fn postgres_timestamp_precision(time: SystemTime) -> SystemTime {
    let duration = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let microseconds = duration.as_micros();
    let seconds = u64::try_from(microseconds / 1_000_000)
        .expect("a SystemTime duration has a u64 seconds component");
    let nanoseconds = u32::try_from((microseconds % 1_000_000) * 1_000)
        .expect("a sub-second microsecond count always fits in u32 nanoseconds");
    SystemTime::UNIX_EPOCH + Duration::new(seconds, nanoseconds)
}

fn inspect_snapshot_pair(row: &Row) -> Result<RevisionPair, PostgresKernelError> {
    let source = SourceRevisionId::from_bytes(inspect_id(
        INSPECT_SNAPSHOT_RELATION,
        row,
        "inspection snapshot context",
        "source_revision_id",
    )?);
    let catalogue = CatalogueRevisionId::from_bytes(inspect_id(
        INSPECT_SNAPSHOT_RELATION,
        row,
        "inspection snapshot context",
        "catalogue_revision_id",
    )?);
    Ok(RevisionPair::new(source, catalogue))
}

pub(super) fn decode_security_decision_row(
    row: &Row,
) -> Result<SecurityDecisionRow, PostgresKernelError> {
    let event_id = SecurityAuditEventId::from_bytes(inspect_id(
        "_orna_kernel.security_audit_events",
        row,
        "event_id",
        "event_id",
    )?);
    let record = event_id.canonical();
    let kind: String = inspect_column(
        "_orna_kernel.security_audit_events",
        row,
        &record,
        "event_kind",
    )?;
    let kind = match kind.as_str() {
        "execute" => InspectSecurityDecisionKind::Execute,
        "capability" => InspectSecurityDecisionKind::Capability,
        "user_state" => InspectSecurityDecisionKind::UserState,
        "inspect" => InspectSecurityDecisionKind::Inspect,
        _ => {
            return Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.security_audit_events",
                record,
                rule: "security decision kind is outside the closed set",
            });
        }
    };
    let outcome: String = inspect_column(
        "_orna_kernel.security_audit_events",
        row,
        &event_id.canonical(),
        "outcome",
    )?;
    let outcome = match outcome.as_str() {
        "allowed" => InspectSecurityDecisionOutcome::Allowed,
        "denied" => InspectSecurityDecisionOutcome::Denied,
        _ => {
            return Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.security_audit_events",
                record: event_id.canonical(),
                rule: "security decision outcome must be allowed or denied",
            });
        }
    };
    let mut principals = Vec::new();
    for principal in [
        inspect_optional_id(
            "_orna_kernel.security_audit_events",
            row,
            &event_id.canonical(),
            "session_principal_id",
        )?
        .map(PrincipalId::from_bytes),
        inspect_optional_id(
            "_orna_kernel.security_audit_events",
            row,
            &event_id.canonical(),
            "effective_principal_id",
        )?
        .map(PrincipalId::from_bytes),
        inspect_optional_id(
            "_orna_kernel.security_audit_events",
            row,
            &event_id.canonical(),
            "authorising_principal_id",
        )?
        .map(PrincipalId::from_bytes),
    ]
    .into_iter()
    .flatten()
    {
        if !principals.contains(&principal) {
            principals.push(principal);
        }
    }
    let target = inspect_optional_id(
        "_orna_kernel.security_audit_events",
        row,
        &event_id.canonical(),
        "function_id",
    )?
    .map(FunctionId::from_bytes);
    let denial_reason: Option<String> = inspect_column(
        "_orna_kernel.security_audit_events",
        row,
        &event_id.canonical(),
        "denial_reason",
    )?;
    let denial_reason = if outcome == InspectSecurityDecisionOutcome::Denied {
        denial_reason
    } else {
        None
    };
    SecurityDecisionRow::new(
        kind,
        outcome,
        principals,
        target,
        denial_reason,
        vec![event_id],
    )
    .map_err(PostgresKernelError::Inspect)
}

pub(super) fn decode_state_cell_row(
    row: &Row,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
) -> Result<StateCellRow, PostgresKernelError> {
    let key = decode_state_cell_key(row)?;
    let value_bytes: Vec<u8> = inspect_column(
        "_orna_kernel.user_state_cells",
        row,
        "selected row",
        "value_bytes",
    )?;
    let value = decode_constructed_value(active, registry, &value_bytes)
        .map_err(PostgresKernelError::InspectValueCodec)?;
    let value_type = TypeId::from_bytes(inspect_id(
        "_orna_kernel.user_state_cells",
        row,
        "selected row",
        "value_type_id",
    )?);
    if is_sealed_inspect_type_id(value_type) || is_sealed_inspect_runtime_value(&value) {
        return Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.user_state_cells",
            record: "selected row".to_owned(),
            rule: "USER state cannot expose sealed Inspector values",
        });
    }
    let value = InvokeValue::new(value).map_err(PostgresKernelError::InvocationCarrier)?;
    let revision = decode_revision(row)?;
    let updated_at: SystemTime = inspect_column(
        "_orna_kernel.user_state_cells",
        row,
        "selected row",
        "updated_at",
    )?;
    Ok(StateCellRow::new(
        key,
        value_type,
        revision,
        updated_at,
        Some(value),
    ))
}

fn decode_state_cell_key(row: &Row) -> Result<UserStateKeyWithoutPrincipal, PostgresKernelError> {
    let record = "selected row";
    let root_function = FunctionId::from_bytes(inspect_id(
        "_orna_kernel.user_state_cells",
        row,
        record,
        "root_function_id",
    )?);
    let state_profile: String = inspect_column(
        "_orna_kernel.user_state_cells",
        row,
        record,
        "root_state_profile",
    )?;
    let function = FunctionId::from_bytes(inspect_id(
        "_orna_kernel.user_state_cells",
        row,
        record,
        "function_id",
    )?);
    let instance_key: String = inspect_column(
        "_orna_kernel.user_state_cells",
        row,
        record,
        "function_instance_key",
    )?;
    let state_slot = StateSlotId::from_bytes(inspect_id(
        "_orna_kernel.user_state_cells",
        row,
        record,
        "state_slot_id",
    )?);
    UserStateKeyWithoutPrincipal::new(
        root_function,
        state_profile,
        function,
        instance_key,
        state_slot,
    )
    .map_err(PostgresKernelError::UserState)
}

fn decode_revision(row: &Row) -> Result<u64, PostgresKernelError> {
    let record = "selected row";
    let revision: i64 = inspect_column("_orna_kernel.user_state_cells", row, record, "revision")?;
    let revision = u64::try_from(revision).map_err(|_| PostgresKernelError::DurableInvariant {
        relation: "_orna_kernel.user_state_cells",
        record: record.to_owned(),
        rule: "USER state revision must be a positive unsigned integer",
    })?;
    if revision == 0 {
        return Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.user_state_cells",
            record: record.to_owned(),
            rule: "USER state revision must be positive",
        });
    }
    Ok(revision)
}

/// Builds the verified standard's opaque codec registry, mirroring the USER
/// state kernel's registry.
pub(super) fn inspect_value_registry(
    active: &ActiveDatabaseRevision,
) -> Result<OpaqueCodecRegistry, PostgresKernelError> {
    let standard = active.catalogue_hash_context().standard().ok_or_else(|| {
        PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.active_revision",
            record: active.pair().catalogue().canonical(),
            rule: "inspection requires the accepted verified standard snapshot",
        }
    })?;
    registered_opaque_codecs(standard).map_err(|_| PostgresKernelError::DurableInvariant {
        relation: "_orna_kernel.standard_library_revisions",
        record: standard.revision().canonical(),
        rule: "the verified standard snapshot must bind its opaque codec registry",
    })
}

/// Returns the codec context that authored one retained inspection payload.
///
/// Inspection bytes are pinned by their source/catalogue pair. Reusing the
/// current active revision here would allow a later standard-library registry
/// to reinterpret historical opaque values, or to reject them for the wrong
/// reason. Historical recovery intentionally does not verify current physical
/// tables; it reconstructs only the immutable semantic context needed by the
/// canonical payload codec.
pub(super) async fn inspect_codec_context(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
    pair: RevisionPair,
) -> Result<(ActiveDatabaseRevision, OpaqueCodecRegistry), PostgresKernelError> {
    let revision = if pair == active.pair() {
        active.clone()
    } else {
        recover_revision_for_pair(transaction, pair).await?
    };
    let registry = inspect_value_registry(&revision)?;
    Ok((revision, registry))
}

/// Validates the exact column sets of the inspection relations.
async fn require_inspect_relation_columns(
    transaction: &Transaction<'_>,
) -> Result<(), PostgresKernelError> {
    let snapshot = relation_columns(transaction, "inspect_snapshots").await?;
    let expected_snapshot = [
        "epoch_id",
        "invocation_id",
        "recorded_at",
        "owner_principal_id",
        "source_revision_id",
        "catalogue_revision_id",
        "summary_bytes",
        "observer_root_invocation_id",
        "observer_parent_invocation_id",
        "observer_purpose",
    ];
    if snapshot != expected_snapshot {
        return Err(PostgresKernelError::DurableInvariant {
            relation: INSPECT_SNAPSHOT_RELATION,
            record: "relation schema".to_owned(),
            rule: "inspect_snapshots columns are not exact",
        });
    }
    let trace = relation_columns(transaction, "inspect_trace_events").await?;
    let expected_trace = [
        "invocation_id",
        "sequence",
        "kind",
        "payload_bytes",
        "observer_invocation_id",
        "recorded_at",
    ];
    if trace != expected_trace {
        return Err(PostgresKernelError::DurableInvariant {
            relation: INSPECT_TRACE_RELATION,
            record: "relation schema".to_owned(),
            rule: "inspect_trace_events columns are not exact",
        });
    }
    Ok(())
}

async fn relation_columns(
    transaction: &Transaction<'_>,
    relation: &str,
) -> Result<Vec<String>, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT attribute.attname
             FROM pg_catalog.pg_attribute AS attribute
             JOIN pg_catalog.pg_class AS class ON class.oid = attribute.attrelid
             JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = class.relnamespace
             WHERE namespace.nspname = '_orna_kernel'
               AND class.relname = $1
               AND attribute.attnum > 0
               AND NOT attribute.attisdropped
             ORDER BY attribute.attnum",
            &[&relation],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    rows.iter()
        .map(|row| inspect_column(INSPECT_SNAPSHOT_RELATION, row, "relation schema", "attname"))
        .collect()
}

fn inspect_column<T: FromSqlOwned>(
    relation: &'static str,
    row: &Row,
    record: &str,
    column: &'static str,
) -> Result<T, PostgresKernelError> {
    row.try_get(column)
        .map_err(|source| PostgresKernelError::RowDecode {
            relation,
            record: record.to_owned(),
            column,
            rule: "selected inspection column",
            source,
        })
}

fn inspect_optional_id(
    relation: &'static str,
    row: &Row,
    record: &str,
    column: &'static str,
) -> Result<Option<[u8; 16]>, PostgresKernelError> {
    let Some(bytes) = inspect_column::<Option<Vec<u8>>>(relation, row, record, column)? else {
        return Ok(None);
    };
    bytes
        .try_into()
        .map(Some)
        .map_err(|_| PostgresKernelError::DurableInvariant {
            relation,
            record: record.to_owned(),
            rule: "inspection identity column must carry exactly 16 bytes",
        })
}

pub(super) fn inspect_id(
    relation: &'static str,
    row: &Row,
    record: &str,
    column: &'static str,
) -> Result<[u8; 16], PostgresKernelError> {
    let bytes: Vec<u8> = inspect_column(relation, row, record, column)?;
    bytes
        .try_into()
        .map_err(|_| PostgresKernelError::DurableInvariant {
            relation,
            record: record.to_owned(),
            rule: "inspection identity column must carry exactly 16 bytes",
        })
}

fn remember_inspection_pair(
    pairs: &mut HashMap<InvocationId, RevisionPair>,
    invocation: InvocationId,
    pair: RevisionPair,
) -> Result<(), PostgresKernelError> {
    if let Some(previous) = pairs.get(&invocation) {
        if *previous != pair {
            return Err(PostgresKernelError::DurableInvariant {
                relation: INSPECT_SNAPSHOT_RELATION,
                record: invocation.canonical(),
                rule: "one invocation must retain one historical inspection codec context",
            });
        }
    } else {
        pairs.insert(invocation, pair);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{postgres_timestamp_precision, remember_inspection_pair};
    use crate::PostgresKernelError;
    use orna_core::{CatalogueRevisionId, InvocationId, SourceRevisionId, revision::RevisionPair};
    use std::{
        collections::HashMap,
        time::{Duration, SystemTime},
    };

    #[test]
    fn inspection_timestamp_binding_uses_postgres_microsecond_precision() {
        let captured = SystemTime::UNIX_EPOCH + Duration::new(17, 987_654_321);

        assert_eq!(
            postgres_timestamp_precision(captured),
            SystemTime::UNIX_EPOCH + Duration::new(17, 987_654_000),
            "the durable ordering key must match PostgreSQL's exact precision"
        );
    }

    #[test]
    fn inspection_timestamp_binding_matches_the_canonical_pre_epoch_encoding() {
        let pre_epoch = SystemTime::UNIX_EPOCH - Duration::from_nanos(1);

        assert_eq!(
            postgres_timestamp_precision(pre_epoch),
            SystemTime::UNIX_EPOCH,
            "recovery must use the same pre-epoch normalization as the payload codec"
        );
    }

    #[test]
    fn inspection_trace_context_rejects_cross_revision_snapshots_for_one_invocation() {
        let invocation = InvocationId::from_bytes([1; 16]);
        let first = RevisionPair::new(
            SourceRevisionId::from_bytes([2; 16]),
            CatalogueRevisionId::from_bytes([3; 16]),
        );
        let second = RevisionPair::new(
            SourceRevisionId::from_bytes([4; 16]),
            CatalogueRevisionId::from_bytes([5; 16]),
        );
        let mut pairs = HashMap::new();

        remember_inspection_pair(&mut pairs, invocation, first).expect("first context");
        let error = remember_inspection_pair(&mut pairs, invocation, second)
            .expect_err("one invocation cannot switch historical codec contexts");
        assert!(matches!(
            error,
            PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.inspect_snapshots",
                rule: "one invocation must retain one historical inspection codec context",
                ..
            }
        ));
    }

    #[test]
    fn inspection_trace_context_accepts_repeated_snapshots_at_one_revision() {
        let invocation = InvocationId::from_bytes([1; 16]);
        let pair = RevisionPair::new(
            SourceRevisionId::from_bytes([2; 16]),
            CatalogueRevisionId::from_bytes([3; 16]),
        );
        let mut pairs = HashMap::new();

        remember_inspection_pair(&mut pairs, invocation, pair).expect("first context");
        remember_inspection_pair(&mut pairs, invocation, pair)
            .expect("observer clones retain the same historical context");
    }
}
