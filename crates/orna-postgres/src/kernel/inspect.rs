//! Protected inspection storage and projections from ADR 0064.
//!
//! Every capture is one immutable `_orna_kernel.inspect_snapshots` row keyed
//! by a fresh [`InspectEpochId`] and pinned by the source and catalogue
//! revision pair active at capture time. The canonical epoch payload
//! (`summary_bytes`) is the ORV5 encoding of a `BYTES` value whose body is
//! the closed [`InspectSnapshotEpoch`] envelope defined below, so the whole
//! epoch — id, pinned pair, owner, summary, and every projection row set —
//! round-trips through the verified standard's opaque codec registry exactly
//! like a USER state value.
//!
//! The trace relation retains one row per closed stream event kind. The v1
//! sealed route captures `started`/`value_batch`/`completed` rows for the
//! produced event batch plus one `inspect_snapshot` row; the model stream
//! API returns only the events the closed model payload set can express and
//! retains the richer durable kinds for later slices.

use std::time::{Duration, SystemTime};

use orna_core::{
    CatalogueRevisionId, FunctionId, InspectEpochId, InvocationId, PrincipalId,
    SecurityAuditEventId, SourceRevisionId, StateSlotId, TypeId,
    inspect::{
        CallRow, InspectInvocationNodeKind, InspectInvocationPhase, InspectOutcomeKind,
        InspectPrivilege, InspectResultSummary, InspectSecurityDecisionKind,
        InspectSecurityDecisionOutcome, InspectSnapshotEpoch, InspectSnapshotOptions,
        InspectSnapshotSummary, InspectTraceEvent, InspectTracePayload, InvocationNodeRow,
        PresentationCandidateRow, ResourceRow, RuntimeBindingRow, SecurityDecisionRow,
        StateCellRow, UiNodeRow,
    },
    invocation::{InvocationClientOffer, InvocationEventBody, InvokeValue},
    revision::ActiveDatabaseRevision,
    security::{
        AuthenticatedSession, InspectDecision, InspectEpochScope, SecurityAuditDecision,
        authorise_inspect,
    },
    state::UserStateKeyWithoutPrincipal,
    types::TypeDescriptor,
    value::{OpaqueCodecRegistry, RuntimeValue},
};
use orna_protocol::{InvocationEventBatch, decode_constructed_value, encode_constructed_value};
use orna_standard::registered_opaque_codecs;
use tokio_postgres::{IsolationLevel, Row, Transaction, types::FromSqlOwned};

use crate::{
    PostgresKernel, PostgresKernelError, bootstrap::require_current_migrations,
    security::append_security_audit_event, server_runtime::configure_and_recover,
};

const INSPECT_SNAPSHOT_RELATION: &str = "_orna_kernel.inspect_snapshots";
const INSPECT_TRACE_RELATION: &str = "_orna_kernel.inspect_trace_events";

/// The closed durable stream-kind set admitted by migration 0027.
const INSPECT_TRACE_KINDS: &[&str] = &[
    "started",
    "value_batch",
    "completed",
    "diagnostic",
    "inspect_snapshot",
    "inspect_projection",
    "inspect_trace",
    "security_decision",
];

/// The closed envelope magic and version of the canonical epoch payload.
const INSPECT_EPOCH_MAGIC: &[u8; 4] = b"INEP";
const INSPECT_EPOCH_VERSION: u8 = 1;

impl PostgresKernel {
    /// Captures one immutable inspection epoch and its trace rows.
    ///
    /// The capture runs in one protected transaction: it builds the epoch
    /// from the invocation's produced Event batch and the client offer,
    /// persists the snapshot row, persists one trace row per batch event
    /// plus the `inspect_snapshot` marker, and appends the redacted INSPECT
    /// capture decision. Returns the new epoch identity.
    #[allow(clippy::too_many_arguments)]
    pub async fn capture_inspect_snapshot(
        &self,
        authenticated_session: &AuthenticatedSession,
        invocation: InvocationId,
        options: InspectSnapshotOptions,
        owner: PrincipalId,
        root_target: FunctionId,
        outcome: InspectOutcomeKind,
        events: &InvocationEventBatch,
        client_offer: &InvocationClientOffer,
        observer_invocation: Option<InvocationId>,
    ) -> Result<InspectEpochId, PostgresKernelError> {
        let mut database_session = self.open().await?;
        let operation = async {
            let transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .read_only(false)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let active = configure_and_recover(&transaction).await?;
            let registry = inspect_value_registry(&active)?;
            let epoch_id = capture_inspect_snapshot_in_transaction(
                &transaction,
                &active,
                &registry,
                authenticated_session,
                invocation,
                options,
                owner,
                root_target,
                outcome,
                events,
                client_offer,
                observer_invocation,
            )
            .await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(epoch_id)
        }
        .await;
        finish_inspect_session(operation, database_session.shutdown().await)
    }

    /// Loads one immutable inspection epoch by its exact epoch identity.
    ///
    /// The `summary_bytes` payload decodes through the verified standard's
    /// opaque codec registry (the ORV5 pattern from the USER state kernel)
    /// and the closed epoch envelope, and must round-trip canonically and
    /// agree with the durable identity columns.
    pub async fn load_inspect_snapshot(
        &self,
        epoch_id: InspectEpochId,
    ) -> Result<Option<InspectSnapshotEpoch>, PostgresKernelError> {
        let mut database_session = self.open().await?;
        let operation = async {
            let transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .read_only(true)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let active = configure_and_recover(&transaction).await?;
            let registry = inspect_value_registry(&active)?;
            let row = transaction
                .query_opt(
                    "SELECT epoch_id, invocation_id, recorded_at,
                            owner_principal_id, source_revision_id,
                            catalogue_revision_id, summary_bytes
                     FROM _orna_kernel.inspect_snapshots
                     WHERE epoch_id = $1",
                    &[&epoch_id.to_bytes().to_vec()],
                )
                .await
                .map_err(PostgresKernelError::Database)?;
            let Some(row) = row else {
                transaction
                    .commit()
                    .await
                    .map_err(PostgresKernelError::Database)?;
                return Ok(None);
            };
            let epoch = decode_inspect_snapshot_row(&row, &active, &registry)?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(Some(epoch))
        }
        .await;
        finish_inspect_session(operation, database_session.shutdown().await)
    }

    /// Resolves the most recent inspection epoch captured for one invocation.
    ///
    /// The lookup returns the latest epoch for the invocation (the most
    /// recently captured, breaking ties by epoch identity order) or `None`
    /// when the invocation has no captured epoch. The result is gated by the
    /// INSPECT privilege ladder against the resolved epoch owner, so a
    /// caller with no privilege that reaches the epoch's scope fails closed
    /// with the closed denial reason and no epoch identity is disclosed.
    /// The sealed dispatch auto-captures one structural epoch for every
    /// completed invocation, so a completed invocation normally resolves.
    pub async fn find_latest_inspect_epoch(
        &self,
        authenticated_session: &AuthenticatedSession,
        invocation: InvocationId,
    ) -> Result<Option<InspectEpochId>, PostgresKernelError> {
        let mut database_session = self.open().await?;
        let operation = async {
            let transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .read_only(true)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let row = transaction
                .query_opt(
                    "SELECT epoch_id, owner_principal_id
                     FROM _orna_kernel.inspect_snapshots
                     WHERE invocation_id = $1
                     ORDER BY recorded_at DESC, epoch_id DESC
                     LIMIT 1",
                    &[&invocation.to_bytes().to_vec()],
                )
                .await
                .map_err(PostgresKernelError::Database)?;
            let Some(row) = row else {
                transaction
                    .commit()
                    .await
                    .map_err(PostgresKernelError::Database)?;
                return Ok(None);
            };
            let epoch_id = InspectEpochId::from_bytes(inspect_id(
                INSPECT_SNAPSHOT_RELATION,
                &row,
                invocation.canonical().as_str(),
                "epoch_id",
            )?);
            let owner = PrincipalId::from_bytes(inspect_id(
                INSPECT_SNAPSHOT_RELATION,
                &row,
                invocation.canonical().as_str(),
                "owner_principal_id",
            )?);
            match authorise_inspect(
                authenticated_session.principal(),
                InspectPrivilege::OwnInvocation,
                Some(owner),
                &[InspectPrivilege::OwnInvocation],
            ) {
                InspectDecision::Allowed { .. } => {}
                InspectDecision::Denied(reason) => {
                    return Err(PostgresKernelError::InspectDenied { reason });
                }
            }
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(Some(epoch_id))
        }
        .await;
        finish_inspect_session(operation, database_session.shutdown().await)
    }

    /// Returns the `invocation_nodes` projection over one epoch.
    ///
    /// The projection is gated by the INSPECT privilege ladder; a denied
    /// request fails closed with the closed denial reason.
    pub fn inspect_invocation_nodes(
        &self,
        authenticated_session: &AuthenticatedSession,
        epoch: &InspectSnapshotEpoch,
        requested: InspectPrivilege,
        granted: &[InspectPrivilege],
    ) -> Result<Vec<InvocationNodeRow>, PostgresKernelError> {
        require_inspect_privilege(authenticated_session, epoch, requested, granted)?;
        Ok(epoch.invocation_nodes().to_vec())
    }

    /// Returns the `calls` projection over one epoch.
    pub fn inspect_calls(
        &self,
        authenticated_session: &AuthenticatedSession,
        epoch: &InspectSnapshotEpoch,
        requested: InspectPrivilege,
        granted: &[InspectPrivilege],
    ) -> Result<Vec<CallRow>, PostgresKernelError> {
        require_inspect_privilege(authenticated_session, epoch, requested, granted)?;
        Ok(epoch.calls().to_vec())
    }

    /// Returns the `resources` projection over one epoch.
    ///
    /// No resource tracking exists in v1, so the captured set is always
    /// empty; the closed row type is sealed for the later resource slice.
    pub fn inspect_resources(
        &self,
        authenticated_session: &AuthenticatedSession,
        epoch: &InspectSnapshotEpoch,
        requested: InspectPrivilege,
        granted: &[InspectPrivilege],
    ) -> Result<Vec<ResourceRow>, PostgresKernelError> {
        require_inspect_privilege(authenticated_session, epoch, requested, granted)?;
        Ok(epoch.resources().to_vec())
    }

    /// Returns the `state_cells` projection over one epoch.
    ///
    /// The projection reads the live `_orna_kernel.user_state_cells` rows
    /// for the epoch owner and root target, decoding each typed value
    /// through the verified standard registry. Values are redacted to `None`
    /// unless the caller requested and was granted the `Values` classifier.
    pub async fn inspect_state_cells(
        &self,
        authenticated_session: &AuthenticatedSession,
        epoch: &InspectSnapshotEpoch,
        requested: InspectPrivilege,
        granted: &[InspectPrivilege],
    ) -> Result<Vec<StateCellRow>, PostgresKernelError> {
        require_inspect_privilege(authenticated_session, epoch, requested, granted)?;
        let include_values = requested == InspectPrivilege::Values;
        let mut database_session = self.open().await?;
        let operation = async {
            let transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .read_only(true)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let active = configure_and_recover(&transaction).await?;
            let registry = inspect_value_registry(&active)?;
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
                    &[
                        &epoch.owner().to_bytes().to_vec(),
                        &epoch.root_target().to_bytes().to_vec(),
                    ],
                )
                .await
                .map_err(PostgresKernelError::Database)?;
            let mut cells = Vec::with_capacity(rows.len());
            for row in &rows {
                let key = decode_state_cell_key(row)?;
                let value_bytes: Vec<u8> = inspect_column(
                    "_orna_kernel.user_state_cells",
                    row,
                    "value_bytes",
                    "value_bytes",
                )?;
                let value = decode_constructed_value(&active, &registry, &value_bytes)
                    .map_err(PostgresKernelError::InspectValueCodec)?;
                let value =
                    InvokeValue::new(value).map_err(PostgresKernelError::InvocationCarrier)?;
                let value_type = TypeId::from_bytes(inspect_id(
                    "_orna_kernel.user_state_cells",
                    row,
                    "value_type_id",
                    "value_type_id",
                )?);
                let revision = decode_revision(row)?;
                let updated_at: SystemTime = inspect_column(
                    "_orna_kernel.user_state_cells",
                    row,
                    "updated_at",
                    "updated_at",
                )?;
                let value = if include_values { Some(value) } else { None };
                cells.push(StateCellRow::new(
                    key, value_type, revision, updated_at, value,
                ));
            }
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(cells)
        }
        .await;
        finish_inspect_session(operation, database_session.shutdown().await)
    }

    /// Returns the `ui_nodes` projection over one epoch.
    ///
    /// CLIENT execution is blocked in v1, so the captured set is always
    /// empty; the closed row type is sealed for the CLIENT slice.
    pub fn inspect_ui_nodes(
        &self,
        authenticated_session: &AuthenticatedSession,
        epoch: &InspectSnapshotEpoch,
        requested: InspectPrivilege,
        granted: &[InspectPrivilege],
    ) -> Result<Vec<UiNodeRow>, PostgresKernelError> {
        require_inspect_privilege(authenticated_session, epoch, requested, granted)?;
        Ok(epoch.ui_nodes().to_vec())
    }

    /// Returns the `presentation_candidates` projection over one epoch.
    ///
    /// The sealed v1 dispatch path does not expose the presenter resolution
    /// at capture time, so the captured set is empty; the closed row type is
    /// sealed for the planner-instrumented slice.
    pub fn inspect_presentation_candidates(
        &self,
        authenticated_session: &AuthenticatedSession,
        epoch: &InspectSnapshotEpoch,
        requested: InspectPrivilege,
        granted: &[InspectPrivilege],
    ) -> Result<Vec<PresentationCandidateRow>, PostgresKernelError> {
        require_inspect_privilege(authenticated_session, epoch, requested, granted)?;
        Ok(epoch.presentation_candidates().to_vec())
    }

    /// Returns the `runtime_bindings` projection over one epoch.
    pub fn inspect_runtime_bindings(
        &self,
        authenticated_session: &AuthenticatedSession,
        epoch: &InspectSnapshotEpoch,
        requested: InspectPrivilege,
        granted: &[InspectPrivilege],
    ) -> Result<Vec<RuntimeBindingRow>, PostgresKernelError> {
        require_inspect_privilege(authenticated_session, epoch, requested, granted)?;
        Ok(epoch.runtime_bindings().to_vec())
    }

    /// Returns the `security_decisions` projection over one epoch.
    ///
    /// The projection joins the linked protected `EXECUTE` evidence: the
    /// invocation audit row's `security_audit_event_id` resolves to the
    /// exact decision that admitted the captured invocation.
    pub async fn inspect_security_decisions(
        &self,
        authenticated_session: &AuthenticatedSession,
        epoch: &InspectSnapshotEpoch,
        requested: InspectPrivilege,
        granted: &[InspectPrivilege],
    ) -> Result<Vec<SecurityDecisionRow>, PostgresKernelError> {
        require_inspect_privilege(authenticated_session, epoch, requested, granted)?;
        let mut database_session = self.open().await?;
        let operation = async {
            let transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .read_only(true)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let rows = transaction
                .query(
                    "SELECT security.event_id, security.event_kind,
                            security.outcome, security.denial_reason,
                            security.session_principal_id,
                            security.effective_principal_id,
                            security.authorising_principal_id,
                            security.function_id
                     FROM _orna_kernel.invocation_audit_events AS invocation
                     JOIN _orna_kernel.security_audit_events AS security
                       ON security.event_id = invocation.security_audit_event_id
                     WHERE invocation.invocation_id = $1
                     ORDER BY security.sequence",
                    &[&epoch.invocation_id().to_bytes().to_vec()],
                )
                .await
                .map_err(PostgresKernelError::Database)?;
            let mut decisions = Vec::with_capacity(rows.len());
            for row in &rows {
                decisions.push(decode_security_decision_row(row)?);
            }
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(decisions)
        }
        .await;
        finish_inspect_session(operation, database_session.shutdown().await)
    }

    /// Streams the model-expressible trace events of one invocation.
    ///
    /// Events are returned in contiguous sequence order with
    /// `sequence > after_sequence`. Self-observation suppression is the
    /// default: rows whose `observer_invocation_id` matches the inspecting
    /// invocation are dropped unless `include_observer` is set. Durable
    /// stream kinds the closed v1 model cannot express (for example the
    /// `inspect_snapshot` marker) are retained in the relation but not
    /// returned by this v1 model API.
    pub async fn stream_inspect_trace(
        &self,
        invocation_id: InvocationId,
        after_sequence: u64,
        observer_invocation: Option<InvocationId>,
        include_observer: bool,
    ) -> Result<Vec<InspectTraceEvent>, PostgresKernelError> {
        let after =
            i64::try_from(after_sequence).map_err(|_| PostgresKernelError::DurableInvariant {
                relation: INSPECT_TRACE_RELATION,
                record: invocation_id.canonical(),
                rule: "trace sequence must fit PostgreSQL BIGINT",
            })?;
        let mut database_session = self.open().await?;
        let operation = async {
            let transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .read_only(true)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let active = configure_and_recover(&transaction).await?;
            let registry = inspect_value_registry(&active)?;
            // `after_sequence` is a resume cursor: 0 (the spec default) means
            // "from the start" and returns the full stream including sequence
            // 0; any positive value returns only rows strictly after it.
            let rows = if after == 0 {
                transaction
                    .query(
                        "SELECT invocation_id, sequence, kind, payload_bytes,
                                observer_invocation_id, recorded_at
                         FROM _orna_kernel.inspect_trace_events
                         WHERE invocation_id = $1
                         ORDER BY sequence",
                        &[&invocation_id.to_bytes().to_vec()],
                    )
                    .await
                    .map_err(PostgresKernelError::Database)?
            } else {
                transaction
                    .query(
                        "SELECT invocation_id, sequence, kind, payload_bytes,
                                observer_invocation_id, recorded_at
                         FROM _orna_kernel.inspect_trace_events
                         WHERE invocation_id = $1 AND sequence > $2
                         ORDER BY sequence",
                        &[&invocation_id.to_bytes().to_vec(), &after],
                    )
                    .await
                    .map_err(PostgresKernelError::Database)?
            };
            let mut events = Vec::with_capacity(rows.len());
            for row in &rows {
                let record = row_invocation_record(row)?;
                if !include_observer
                    && observer_invocation.is_some()
                    && record.observer_invocation == observer_invocation
                {
                    continue;
                }
                if !matches!(
                    record.kind.as_str(),
                    "started" | "value_batch" | "completed"
                ) {
                    // The closed v1 model carries the five lifecycle payloads
                    // only; the richer durable kinds are retained for later
                    // slices and never dropped from the relation.
                    continue;
                }
                let RuntimeValue::InvokeEvent(event) =
                    decode_constructed_value(&active, &registry, &record.payload_bytes)
                        .map_err(PostgresKernelError::InspectValueCodec)?
                else {
                    return Err(PostgresKernelError::DurableInvariant {
                        relation: INSPECT_TRACE_RELATION,
                        record: record.invocation.canonical(),
                        rule: "trace payload must decode as one invocation event",
                    });
                };
                if event.invocation_id() != record.invocation || event.sequence() != record.sequence
                {
                    return Err(PostgresKernelError::DurableInvariant {
                        relation: INSPECT_TRACE_RELATION,
                        record: record.invocation.canonical(),
                        rule: "trace row must agree with its canonical event payload",
                    });
                }
                let Some(payload) = model_payload_for(&record.kind, event.body()) else {
                    return Err(PostgresKernelError::DurableInvariant {
                        relation: INSPECT_TRACE_RELATION,
                        record: record.invocation.canonical(),
                        rule: "trace kind must agree with its event payload kind",
                    });
                };
                events.push(
                    InspectTraceEvent::new(
                        record.invocation,
                        record.sequence,
                        payload,
                        record.recorded_at,
                        record.observer_invocation,
                        None,
                    )
                    .map_err(PostgresKernelError::Inspect)?,
                );
            }
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(events)
        }
        .await;
        finish_inspect_session(operation, database_session.shutdown().await)
    }
}

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
    client_offer: &InvocationClientOffer,
    observer_invocation: Option<InvocationId>,
) -> Result<InspectEpochId, PostgresKernelError> {
    let epoch = build_inspect_epoch(
        active,
        invocation,
        options,
        owner,
        root_target,
        outcome,
        events,
        client_offer,
    )?;
    let epoch_id = epoch.id();
    let record = epoch_id.canonical();
    let recorded_at = epoch.recorded_at();

    let payload = encode_epoch_payload(active, registry, &epoch)?;
    let summary_bytes = encode_constructed_value(active, registry, &RuntimeValue::Bytes(payload))
        .map_err(PostgresKernelError::InspectValueCodec)?;
    let invocation_id = invocation.to_bytes().to_vec();
    transaction
        .execute(
            "INSERT INTO _orna_kernel.inspect_snapshots
                 (epoch_id, invocation_id, recorded_at, owner_principal_id,
                  source_revision_id, catalogue_revision_id, summary_bytes)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
            &[
                &epoch_id.to_bytes().to_vec(),
                &invocation_id,
                &recorded_at,
                &epoch.owner().to_bytes().to_vec(),
                &epoch.source_revision_id().to_bytes().to_vec(),
                &epoch.catalogue_revision_id().to_bytes().to_vec(),
                &summary_bytes,
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

    let decision = InspectDecision::Allowed {
        epoch_scope: InspectEpochScope::Own,
        requested: InspectPrivilege::OwnInvocation,
    };
    let audit =
        SecurityAuditDecision::inspect_allowed(authenticated_session, decision, Some(owner))
            .map_err(|_| PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.security_audit_events",
                record: record.clone(),
                rule: "the capture decision must be an allowed INSPECT decision",
            })?;
    append_security_audit_event(transaction, audit).await?;
    Ok(epoch_id)
}

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
                    catalogue_revision_id, summary_bytes
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
    let registry = inspect_value_registry(active)?;
    let registry = &registry;

    for row in &rows {
        decode_inspect_snapshot_row(row, active, registry)?;
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
        if !matches!(
            record.kind.as_str(),
            "started" | "value_batch" | "completed"
        ) {
            continue;
        }
        let RuntimeValue::InvokeEvent(event) =
            decode_constructed_value(active, registry, &record.payload_bytes)
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

/// Builds one immutable inspection epoch from the sealed dispatch facts.
#[allow(clippy::too_many_arguments)]
fn build_inspect_epoch(
    active: &ActiveDatabaseRevision,
    invocation: InvocationId,
    options: InspectSnapshotOptions,
    owner: PrincipalId,
    root_target: FunctionId,
    outcome: InspectOutcomeKind,
    events: &InvocationEventBatch,
    client_offer: &InvocationClientOffer,
) -> Result<InspectSnapshotEpoch, PostgresKernelError> {
    let mut value_count = 0_u64;
    let mut schema = None;
    let mut duration_nanoseconds = 0_u64;
    for record in events.records() {
        match record.event().body() {
            InvocationEventBody::ValueBatch {
                schema: batch_schema,
                values,
            } => {
                value_count = values.len() as u64;
                schema = batch_schema.clone();
            }
            InvocationEventBody::Completed {
                duration_nanoseconds: duration,
            } => duration_nanoseconds = *duration,
            InvocationEventBody::Started { .. }
            | InvocationEventBody::Diagnostic(_)
            | InvocationEventBody::Failed(_)
            | InvocationEventBody::Cancelled { .. }
            | _ => {}
        }
    }
    let result = if value_count == 0 {
        InspectResultSummary::NoValues
    } else {
        InspectResultSummary::ValueBatch { value_count }
    };
    let summary = InspectSnapshotSummary::new(
        events.records().len() as u64,
        result,
        Some(duration_nanoseconds),
    )
    .map_err(PostgresKernelError::Inspect)?;
    let node = InvocationNodeRow::new(
        invocation,
        None,
        InspectInvocationNodeKind::Root,
        InspectInvocationPhase::Completed,
        root_target,
        0,
    )
    .map_err(PostgresKernelError::Inspect)?;
    let call = CallRow::new(invocation, schema, value_count, duration_nanoseconds)
        .map_err(PostgresKernelError::Inspect)?;
    let runtime_bindings = runtime_bindings_from_offer(client_offer)?;
    InspectSnapshotEpoch::new(
        InspectEpochId::new(),
        invocation,
        active.pair().source(),
        active.pair().catalogue(),
        owner,
        SystemTime::now(),
        root_target,
        outcome,
        summary,
        &options,
        vec![node],
        vec![call],
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        runtime_bindings,
        Vec::new(),
    )
    .map_err(PostgresKernelError::Inspect)
}

/// Builds the closed runtime-binding rows from one client offer.
fn runtime_bindings_from_offer(
    client_offer: &InvocationClientOffer,
) -> Result<Vec<RuntimeBindingRow>, PostgresKernelError> {
    let mut rows = Vec::with_capacity(client_offer.runtime_offers().len());
    for runtime in client_offer.runtime_offers() {
        let contracts = runtime
            .contracts()
            .iter()
            .map(|contract| {
                (
                    contract.name().to_owned(),
                    contract.version().to_owned(),
                    contract.features().to_vec(),
                )
            })
            .collect();
        // Client offers carry signed ranks; the closed model ranks are
        // unsigned, so a de-prioritised negative rank clamps to rank zero.
        let rank = runtime.preference_rank().max(0) as u32;
        rows.push(
            RuntimeBindingRow::new(
                runtime.name().to_owned(),
                runtime.version().to_owned(),
                runtime.consumed_descriptors().to_vec(),
                contracts,
                runtime.trusted(),
                rank,
            )
            .map_err(PostgresKernelError::Inspect)?,
        );
    }
    Ok(rows)
}

/// Returns the closed durable stream kind of one event body.
fn durable_trace_kind(body: &InvocationEventBody) -> Option<&'static str> {
    match body {
        InvocationEventBody::Started { .. } => Some("started"),
        InvocationEventBody::ValueBatch { .. } => Some("value_batch"),
        InvocationEventBody::Completed { .. } => Some("completed"),
        InvocationEventBody::Diagnostic(_) => Some("diagnostic"),
        InvocationEventBody::Failed(_) | InvocationEventBody::Cancelled { .. } => None,
        _ => None,
    }
}

/// Maps one durable stream kind and event body to the closed model payload.
///
/// Kinds the closed v1 model cannot express (for example `inspect_snapshot`)
/// are handled by the caller before this mapping runs.
fn model_payload_for(kind: &str, body: &InvocationEventBody) -> Option<InspectTracePayload> {
    match (kind, body) {
        ("started", InvocationEventBody::Started { .. }) => Some(InspectTracePayload::Started),
        ("value_batch", InvocationEventBody::ValueBatch { schema, values }) => {
            Some(InspectTracePayload::ValueBatch {
                schema: schema.clone(),
                values: values.clone(),
            })
        }
        (
            "completed",
            InvocationEventBody::Completed {
                duration_nanoseconds,
            },
        ) => Some(InspectTracePayload::Completed {
            duration_nanoseconds: *duration_nanoseconds,
        }),
        _ => None,
    }
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

/// The durable identity and kind facts of one trace row.
struct InvocationTraceRecord {
    invocation: InvocationId,
    sequence: u64,
    kind: String,
    payload_bytes: Vec<u8>,
    observer_invocation: Option<InvocationId>,
    recorded_at: SystemTime,
}

fn row_invocation_record(row: &Row) -> Result<InvocationTraceRecord, PostgresKernelError> {
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

fn decode_inspect_snapshot_row(
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
    let _recorded_at: SystemTime =
        inspect_column(INSPECT_SNAPSHOT_RELATION, row, &record, "recorded_at")?;
    let summary_bytes: Vec<u8> =
        inspect_column(INSPECT_SNAPSHOT_RELATION, row, &record, "summary_bytes")?;
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
    {
        return Err(PostgresKernelError::DurableInvariant {
            relation: INSPECT_SNAPSHOT_RELATION,
            record: record.clone(),
            rule: "the snapshot row must agree with its canonical epoch payload",
        });
    }
    // The recorded_at column is not cross-checked: PostgreSQL timestamptz
    // truncates to microseconds while the canonical payload retains the
    // full capture time.
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

fn decode_security_decision_row(row: &Row) -> Result<SecurityDecisionRow, PostgresKernelError> {
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
                record: record.clone(),
                rule: "security decision kind is outside the closed set",
            });
        }
    };
    let outcome: String = inspect_column(
        "_orna_kernel.security_audit_events",
        row,
        &record,
        "outcome",
    )?;
    let outcome = match outcome.as_str() {
        "allowed" => InspectSecurityDecisionOutcome::Allowed,
        "denied" => InspectSecurityDecisionOutcome::Denied,
        _ => {
            return Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.security_audit_events",
                record: record.clone(),
                rule: "security decision outcome must be allowed or denied",
            });
        }
    };
    let mut principals = Vec::new();
    for principal in [
        inspect_optional_id(
            "_orna_kernel.security_audit_events",
            row,
            &record,
            "session_principal_id",
        )?
        .map(PrincipalId::from_bytes),
        inspect_optional_id(
            "_orna_kernel.security_audit_events",
            row,
            &record,
            "effective_principal_id",
        )?
        .map(PrincipalId::from_bytes),
        inspect_optional_id(
            "_orna_kernel.security_audit_events",
            row,
            &record,
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
        &record,
        "function_id",
    )?
    .map(FunctionId::from_bytes);
    let denial_reason: Option<String> = inspect_column(
        "_orna_kernel.security_audit_events",
        row,
        &record,
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

/// Decides whether one session principal may apply one INSPECT privilege to
/// one epoch, failing closed with the closed denial reason.
fn require_inspect_privilege(
    authenticated_session: &AuthenticatedSession,
    epoch: &InspectSnapshotEpoch,
    requested: InspectPrivilege,
    granted: &[InspectPrivilege],
) -> Result<(), PostgresKernelError> {
    match authorise_inspect(
        authenticated_session.principal(),
        requested,
        Some(epoch.owner()),
        granted,
    ) {
        InspectDecision::Allowed { .. } => Ok(()),
        InspectDecision::Denied(reason) => Err(PostgresKernelError::InspectDenied { reason }),
    }
}

/// Builds the verified standard's opaque codec registry, mirroring the USER
/// state kernel's registry.
fn inspect_value_registry(
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

fn inspect_id(
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

fn finish_inspect_session<T>(
    operation: Result<T, PostgresKernelError>,
    shutdown: Result<(), PostgresKernelError>,
) -> Result<T, PostgresKernelError> {
    match (operation, shutdown) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

// ---------------------------------------------------------------------------
// The canonical epoch payload envelope
// ---------------------------------------------------------------------------

/// Encodes one immutable epoch as the closed canonical envelope.
///
/// The envelope is deterministic: every length is a fixed-width big-endian
/// integer, every identity is its raw 16 bytes, and every typed value is
/// re-encoded as canonical ORV5 through the pinned registry, so re-encoding
/// a decoded envelope always reproduces the stored bytes.
fn encode_epoch_payload(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    epoch: &InspectSnapshotEpoch,
) -> Result<Vec<u8>, PostgresKernelError> {
    let mut writer = PayloadWriter::new();
    writer.bytes.extend_from_slice(INSPECT_EPOCH_MAGIC);
    writer.push_u8(INSPECT_EPOCH_VERSION);
    writer.push_id(&epoch.id().to_bytes());
    writer.push_id(&epoch.invocation_id().to_bytes());
    writer.push_id(&epoch.source_revision_id().to_bytes());
    writer.push_id(&epoch.catalogue_revision_id().to_bytes());
    writer.push_id(&epoch.owner().to_bytes());
    push_system_time(&mut writer, epoch.recorded_at());
    writer.push_id(&epoch.root_target().to_bytes());
    writer.push_u8(outcome_tag(epoch.outcome()));
    push_summary(&mut writer, epoch.summary());
    push_invocation_nodes(&mut writer, epoch.invocation_nodes());
    push_calls(&mut writer, active, registry, epoch.calls())?;
    push_resources(&mut writer, epoch.resources());
    push_state_cells(&mut writer, active, registry, epoch.state_cells())?;
    push_ui_nodes(&mut writer, epoch.ui_nodes());
    push_presentation_candidates(&mut writer, epoch.presentation_candidates());
    push_runtime_bindings(&mut writer, epoch.runtime_bindings());
    push_security_decisions(&mut writer, epoch.security_decisions());
    Ok(writer.into_bytes())
}

/// Decodes one canonical envelope back into the immutable epoch.
fn decode_epoch_payload(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    payload: &[u8],
    record: &str,
) -> Result<InspectSnapshotEpoch, PostgresKernelError> {
    let mut reader = PayloadReader::new(payload, record);
    if reader.bytes.len() < INSPECT_EPOCH_MAGIC.len()
        || &reader.bytes[..INSPECT_EPOCH_MAGIC.len()] != INSPECT_EPOCH_MAGIC
    {
        return Err(reader.invalid("epoch payload magic is not exact"));
    }
    reader.position = INSPECT_EPOCH_MAGIC.len();
    let version = reader.take_u8("epoch payload version")?;
    if version != INSPECT_EPOCH_VERSION {
        return Err(reader.invalid("epoch payload version is unsupported"));
    }
    let id = InspectEpochId::from_bytes(reader.take_id("epoch identity")?);
    let invocation_id = InvocationId::from_bytes(reader.take_id("invocation identity")?);
    let source_revision_id =
        SourceRevisionId::from_bytes(reader.take_id("source revision identity")?);
    let catalogue_revision_id =
        CatalogueRevisionId::from_bytes(reader.take_id("catalogue revision identity")?);
    let owner = PrincipalId::from_bytes(reader.take_id("owner principal identity")?);
    let recorded_at = take_system_time(&mut reader)?;
    let root_target = FunctionId::from_bytes(reader.take_id("root target identity")?);
    let outcome = decode_outcome(reader.take_u8("outcome")?, &reader)?;
    let summary = take_summary(&mut reader)?;
    let invocation_nodes = take_invocation_nodes(&mut reader)?;
    let calls = take_calls(&mut reader, active, registry)?;
    let resources = take_resources(&mut reader)?;
    let state_cells = take_state_cells(&mut reader, active, registry)?;
    let ui_nodes = take_ui_nodes(&mut reader)?;
    let presentation_candidates = take_presentation_candidates(&mut reader)?;
    let runtime_bindings = take_runtime_bindings(&mut reader)?;
    let security_decisions = take_security_decisions(&mut reader)?;
    if reader.remaining() != 0 {
        return Err(reader.invalid("epoch payload carries trailing bytes"));
    }
    InspectSnapshotEpoch::new(
        id,
        invocation_id,
        source_revision_id,
        catalogue_revision_id,
        owner,
        recorded_at,
        root_target,
        outcome,
        summary,
        &InspectSnapshotOptions::new(true, true, true, true),
        invocation_nodes,
        calls,
        resources,
        state_cells,
        ui_nodes,
        presentation_candidates,
        runtime_bindings,
        security_decisions,
    )
    .map_err(PostgresKernelError::Inspect)
}

struct PayloadWriter {
    bytes: Vec<u8>,
}

impl PayloadWriter {
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    fn push_u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn push_u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn push_u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn push_flag(&mut self, value: bool) {
        self.bytes.push(u8::from(value));
    }

    fn push_bytes(&mut self, bytes: &[u8]) {
        self.push_u64(bytes.len() as u64);
        self.bytes.extend_from_slice(bytes);
    }

    fn push_str(&mut self, value: &str) {
        self.push_bytes(value.as_bytes());
    }

    fn push_id(&mut self, id: &[u8; 16]) {
        self.bytes.extend_from_slice(id);
    }

    fn push_opt_id(&mut self, id: Option<[u8; 16]>) {
        self.push_flag(id.is_some());
        if let Some(id) = id {
            self.push_id(&id);
        }
    }

    fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

struct PayloadReader<'a> {
    bytes: &'a [u8],
    position: usize,
    record: &'a str,
}

impl<'a> PayloadReader<'a> {
    fn new(bytes: &'a [u8], record: &'a str) -> Self {
        Self {
            bytes,
            position: 0,
            record,
        }
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.position
    }

    fn take_u8(&mut self, rule: &'static str) -> Result<u8, PostgresKernelError> {
        if self.remaining() < 1 {
            return Err(self.invalid(rule));
        }
        let value = self.bytes[self.position];
        self.position += 1;
        Ok(value)
    }

    fn take_u32(&mut self, rule: &'static str) -> Result<u32, PostgresKernelError> {
        if self.remaining() < 4 {
            return Err(self.invalid(rule));
        }
        let value = u32::from_be_bytes(
            self.bytes[self.position..self.position + 4]
                .try_into()
                .expect("four bytes are available"),
        );
        self.position += 4;
        Ok(value)
    }

    fn take_u64(&mut self, rule: &'static str) -> Result<u64, PostgresKernelError> {
        if self.remaining() < 8 {
            return Err(self.invalid(rule));
        }
        let value = u64::from_be_bytes(
            self.bytes[self.position..self.position + 8]
                .try_into()
                .expect("eight bytes are available"),
        );
        self.position += 8;
        Ok(value)
    }

    fn take_flag(&mut self, rule: &'static str) -> Result<bool, PostgresKernelError> {
        Ok(self.take_u8(rule)? != 0)
    }

    fn take_bytes(&mut self, rule: &'static str) -> Result<Vec<u8>, PostgresKernelError> {
        let length = self.take_u64(rule)?;
        let length = usize::try_from(length).map_err(|_| self.invalid(rule))?;
        if self.remaining() < length {
            return Err(self.invalid(rule));
        }
        let value = self.bytes[self.position..self.position + length].to_vec();
        self.position += length;
        Ok(value)
    }

    fn take_str(&mut self, rule: &'static str) -> Result<String, PostgresKernelError> {
        let bytes = self.take_bytes(rule)?;
        String::from_utf8(bytes).map_err(|_| self.invalid(rule))
    }

    fn take_id(&mut self, rule: &'static str) -> Result<[u8; 16], PostgresKernelError> {
        if self.remaining() < 16 {
            return Err(self.invalid(rule));
        }
        let bytes: [u8; 16] = self.bytes[self.position..self.position + 16]
            .try_into()
            .map_err(|_| self.invalid(rule))?;
        self.position += 16;
        Ok(bytes)
    }

    fn take_opt_id(&mut self, rule: &'static str) -> Result<Option<[u8; 16]>, PostgresKernelError> {
        if self.take_flag(rule)? {
            Ok(Some(self.take_id(rule)?))
        } else {
            Ok(None)
        }
    }

    fn invalid(&self, rule: &'static str) -> PostgresKernelError {
        PostgresKernelError::DurableInvariant {
            relation: INSPECT_SNAPSHOT_RELATION,
            record: self.record.to_owned(),
            rule,
        }
    }
}

fn push_system_time(writer: &mut PayloadWriter, time: SystemTime) {
    let duration = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    writer.push_u64(duration.as_secs());
    writer.push_u32(duration.subsec_nanos());
}

fn take_system_time(reader: &mut PayloadReader<'_>) -> Result<SystemTime, PostgresKernelError> {
    let seconds = reader.take_u64("recording time seconds")?;
    let nanoseconds = reader.take_u32("recording time nanoseconds")?;
    Ok(SystemTime::UNIX_EPOCH + Duration::new(seconds, nanoseconds))
}

fn outcome_tag(outcome: InspectOutcomeKind) -> u8 {
    match outcome {
        InspectOutcomeKind::Allowed => 0,
        InspectOutcomeKind::Denied => 1,
        InspectOutcomeKind::Failed => 2,
        InspectOutcomeKind::Cancelled => 3,
    }
}

fn decode_outcome(
    tag: u8,
    reader: &PayloadReader<'_>,
) -> Result<InspectOutcomeKind, PostgresKernelError> {
    match tag {
        0 => Ok(InspectOutcomeKind::Allowed),
        1 => Ok(InspectOutcomeKind::Denied),
        2 => Ok(InspectOutcomeKind::Failed),
        3 => Ok(InspectOutcomeKind::Cancelled),
        _ => Err(reader.invalid("outcome tag is outside the closed set")),
    }
}

fn push_summary(writer: &mut PayloadWriter, summary: InspectSnapshotSummary) {
    writer.push_u64(summary.event_count());
    match summary.result() {
        InspectResultSummary::NoValues => writer.push_flag(false),
        InspectResultSummary::ValueBatch { value_count } => {
            writer.push_flag(true);
            writer.push_u64(value_count);
        }
    }
    match summary.duration_nanoseconds() {
        Some(duration) => {
            writer.push_flag(true);
            writer.push_u64(duration);
        }
        None => writer.push_flag(false),
    }
}

fn take_summary(
    reader: &mut PayloadReader<'_>,
) -> Result<InspectSnapshotSummary, PostgresKernelError> {
    let event_count = reader.take_u64("summary event count")?;
    let result = if reader.take_flag("summary result flag")? {
        InspectResultSummary::ValueBatch {
            value_count: reader.take_u64("summary value count")?,
        }
    } else {
        InspectResultSummary::NoValues
    };
    let duration_nanoseconds = if reader.take_flag("summary duration flag")? {
        Some(reader.take_u64("summary duration")?)
    } else {
        None
    };
    InspectSnapshotSummary::new(event_count, result, duration_nanoseconds)
        .map_err(|_| reader.invalid("summary is not canonical"))
}

fn push_invocation_nodes(writer: &mut PayloadWriter, rows: &[InvocationNodeRow]) {
    writer.push_u64(rows.len() as u64);
    for row in rows {
        writer.push_id(&row.id().to_bytes());
        writer.push_opt_id(row.parent_id().map(|id| id.to_bytes()));
        writer.push_u8(match row.kind() {
            InspectInvocationNodeKind::Root => 0,
            InspectInvocationNodeKind::Nested => 1,
        });
        writer.push_u8(match row.phase() {
            InspectInvocationPhase::Started => 0,
            InspectInvocationPhase::Executing => 1,
            InspectInvocationPhase::Completed => 2,
            InspectInvocationPhase::Failed => 3,
            InspectInvocationPhase::Cancelled => 4,
        });
        writer.push_id(&row.target().to_bytes());
        writer.push_u64(row.sequence());
    }
}

fn take_invocation_nodes(
    reader: &mut PayloadReader<'_>,
) -> Result<Vec<InvocationNodeRow>, PostgresKernelError> {
    let count = reader.take_u64("invocation node count")?;
    let mut rows = Vec::with_capacity(
        usize::try_from(count).map_err(|_| reader.invalid("invocation node count is too large"))?,
    );
    for _ in 0..count {
        let id = InvocationId::from_bytes(reader.take_id("invocation node identity")?);
        let parent_id = reader
            .take_opt_id("invocation node parent identity")?
            .map(InvocationId::from_bytes);
        let kind = match reader.take_u8("invocation node kind")? {
            0 => InspectInvocationNodeKind::Root,
            1 => InspectInvocationNodeKind::Nested,
            _ => return Err(reader.invalid("invocation node kind is outside the closed set")),
        };
        let phase = match reader.take_u8("invocation node phase")? {
            0 => InspectInvocationPhase::Started,
            1 => InspectInvocationPhase::Executing,
            2 => InspectInvocationPhase::Completed,
            3 => InspectInvocationPhase::Failed,
            4 => InspectInvocationPhase::Cancelled,
            _ => return Err(reader.invalid("invocation node phase is outside the closed set")),
        };
        let target = FunctionId::from_bytes(reader.take_id("invocation node target identity")?);
        let sequence = reader.take_u64("invocation node sequence")?;
        rows.push(
            InvocationNodeRow::new(id, parent_id, kind, phase, target, sequence)
                .map_err(|_| reader.invalid("invocation node row is not canonical"))?,
        );
    }
    Ok(rows)
}

fn push_calls(
    writer: &mut PayloadWriter,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    rows: &[CallRow],
) -> Result<(), PostgresKernelError> {
    writer.push_u64(rows.len() as u64);
    for row in rows {
        writer.push_id(&row.invocation_id().to_bytes());
        push_optional_invoke_value(writer, active, registry, row.schema())?;
        writer.push_u64(row.value_count());
        writer.push_u64(row.duration_nanoseconds());
    }
    Ok(())
}

fn take_calls(
    reader: &mut PayloadReader<'_>,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
) -> Result<Vec<CallRow>, PostgresKernelError> {
    let count = reader.take_u64("call row count")?;
    let mut rows = Vec::with_capacity(
        usize::try_from(count).map_err(|_| reader.invalid("call row count is too large"))?,
    );
    for _ in 0..count {
        let invocation_id = InvocationId::from_bytes(reader.take_id("call invocation identity")?);
        let schema = take_optional_invoke_value(reader, active, registry)?;
        let value_count = reader.take_u64("call value count")?;
        let duration_nanoseconds = reader.take_u64("call duration")?;
        rows.push(
            CallRow::new(invocation_id, schema, value_count, duration_nanoseconds)
                .map_err(|_| reader.invalid("call row is not canonical"))?,
        );
    }
    Ok(rows)
}

fn push_resources(writer: &mut PayloadWriter, rows: &[ResourceRow]) {
    writer.push_u64(rows.len() as u64);
    for row in rows {
        writer.push_u8(match row.kind() {
            orna_core::inspect::InspectResourceKind::State => 0,
            orna_core::inspect::InspectResourceKind::Catalog => 1,
            orna_core::inspect::InspectResourceKind::Standard => 2,
            orna_core::inspect::InspectResourceKind::Runtime => 3,
        });
        writer.push_u8(match row.status() {
            orna_core::inspect::InspectResourceStatus::Active => 0,
            orna_core::inspect::InspectResourceStatus::Invalidated => 1,
            orna_core::inspect::InspectResourceStatus::Released => 2,
        });
    }
}

fn take_resources(reader: &mut PayloadReader<'_>) -> Result<Vec<ResourceRow>, PostgresKernelError> {
    use orna_core::inspect::{InspectResourceKind, InspectResourceStatus};
    let count = reader.take_u64("resource row count")?;
    let mut rows = Vec::with_capacity(
        usize::try_from(count).map_err(|_| reader.invalid("resource row count is too large"))?,
    );
    for _ in 0..count {
        let kind = match reader.take_u8("resource kind")? {
            0 => InspectResourceKind::State,
            1 => InspectResourceKind::Catalog,
            2 => InspectResourceKind::Standard,
            3 => InspectResourceKind::Runtime,
            _ => return Err(reader.invalid("resource kind is outside the closed set")),
        };
        let status = match reader.take_u8("resource status")? {
            0 => InspectResourceStatus::Active,
            1 => InspectResourceStatus::Invalidated,
            2 => InspectResourceStatus::Released,
            _ => return Err(reader.invalid("resource status is outside the closed set")),
        };
        rows.push(ResourceRow::new(kind, status));
    }
    Ok(rows)
}

fn push_state_cells(
    writer: &mut PayloadWriter,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    rows: &[StateCellRow],
) -> Result<(), PostgresKernelError> {
    writer.push_u64(rows.len() as u64);
    for row in rows {
        writer.push_id(&row.key().root_function().to_bytes());
        writer.push_str(row.key().state_profile());
        writer.push_id(&row.key().function().to_bytes());
        writer.push_str(row.key().instance_key());
        writer.push_id(&row.key().state_slot().to_bytes());
        writer.push_id(&row.value_type().to_bytes());
        writer.push_u64(row.revision());
        push_system_time(writer, row.updated_at());
        push_optional_invoke_value(writer, active, registry, row.value())?;
    }
    Ok(())
}

fn take_state_cells(
    reader: &mut PayloadReader<'_>,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
) -> Result<Vec<StateCellRow>, PostgresKernelError> {
    let count = reader.take_u64("state cell row count")?;
    let mut rows = Vec::with_capacity(
        usize::try_from(count).map_err(|_| reader.invalid("state cell row count is too large"))?,
    );
    for _ in 0..count {
        let root_function =
            FunctionId::from_bytes(reader.take_id("state cell root function identity")?);
        let state_profile = reader.take_str("state cell state profile")?;
        let function = FunctionId::from_bytes(reader.take_id("state cell function identity")?);
        let instance_key = reader.take_str("state cell instance key")?;
        let state_slot = StateSlotId::from_bytes(reader.take_id("state cell state slot identity")?);
        let value_type = TypeId::from_bytes(reader.take_id("state cell value type identity")?);
        let revision = reader.take_u64("state cell revision")?;
        let updated_at = take_system_time(reader)?;
        let value = take_optional_invoke_value(reader, active, registry)?;
        let key = UserStateKeyWithoutPrincipal::new(
            root_function,
            state_profile,
            function,
            instance_key,
            state_slot,
        )
        .map_err(|_| reader.invalid("state cell key is not canonical"))?;
        rows.push(StateCellRow::new(
            key, value_type, revision, updated_at, value,
        ));
    }
    Ok(rows)
}

fn push_ui_nodes(writer: &mut PayloadWriter, rows: &[UiNodeRow]) {
    writer.push_u64(rows.len() as u64);
    for row in rows {
        writer.push_id(&row.function().to_bytes());
        writer.push_str(row.call_site());
        writer.push_str(row.runtime_contract());
    }
}

fn take_ui_nodes(reader: &mut PayloadReader<'_>) -> Result<Vec<UiNodeRow>, PostgresKernelError> {
    let count = reader.take_u64("UI node row count")?;
    let mut rows = Vec::with_capacity(
        usize::try_from(count).map_err(|_| reader.invalid("UI node row count is too large"))?,
    );
    for _ in 0..count {
        let function = FunctionId::from_bytes(reader.take_id("UI node function identity")?);
        let call_site = reader.take_str("UI node call site")?;
        let runtime_contract = reader.take_str("UI node runtime contract")?;
        rows.push(
            UiNodeRow::new(function, call_site, runtime_contract)
                .map_err(|_| reader.invalid("UI node row is not canonical"))?,
        );
    }
    Ok(rows)
}

fn push_presentation_candidates(writer: &mut PayloadWriter, rows: &[PresentationCandidateRow]) {
    writer.push_u64(rows.len() as u64);
    for row in rows {
        writer.push_str(row.presenter());
        writer.push_flag(row.accepted());
        writer.push_str(row.reason());
        writer.push_flag(row.selected_sink().is_some());
        if let Some(sink) = row.selected_sink() {
            push_descriptor(writer, sink);
        }
        writer.push_flag(row.runtime().is_some());
        if let Some(runtime) = row.runtime() {
            writer.push_str(runtime);
        }
    }
}

fn take_presentation_candidates(
    reader: &mut PayloadReader<'_>,
) -> Result<Vec<PresentationCandidateRow>, PostgresKernelError> {
    let count = reader.take_u64("presentation candidate row count")?;
    let mut rows = Vec::with_capacity(
        usize::try_from(count)
            .map_err(|_| reader.invalid("presentation candidate row count is too large"))?,
    );
    for _ in 0..count {
        let presenter = reader.take_str("presentation candidate presenter")?;
        let accepted = reader.take_flag("presentation candidate acceptance")?;
        let reason = reader.take_str("presentation candidate reason")?;
        let selected_sink = if reader.take_flag("presentation candidate sink flag")? {
            Some(take_descriptor(reader)?)
        } else {
            None
        };
        let runtime = if reader.take_flag("presentation candidate runtime flag")? {
            Some(reader.take_str("presentation candidate runtime")?)
        } else {
            None
        };
        rows.push(
            PresentationCandidateRow::new(presenter, accepted, reason, selected_sink, runtime)
                .map_err(|_| reader.invalid("presentation candidate row is not canonical"))?,
        );
    }
    Ok(rows)
}

fn push_runtime_bindings(writer: &mut PayloadWriter, rows: &[RuntimeBindingRow]) {
    writer.push_u64(rows.len() as u64);
    for row in rows {
        writer.push_str(row.runtime_name());
        writer.push_str(row.version());
        writer.push_u64(row.consumed_descriptors().len() as u64);
        for descriptor in row.consumed_descriptors() {
            push_descriptor(writer, descriptor);
        }
        writer.push_u64(row.contracts().len() as u64);
        for (name, version, features) in row.contracts() {
            writer.push_str(name);
            writer.push_str(version);
            writer.push_u64(features.len() as u64);
            for feature in features {
                writer.push_str(feature);
            }
        }
        writer.push_flag(row.trusted());
        writer.push_u32(row.preference_rank());
    }
}

fn take_runtime_bindings(
    reader: &mut PayloadReader<'_>,
) -> Result<Vec<RuntimeBindingRow>, PostgresKernelError> {
    let count = reader.take_u64("runtime binding row count")?;
    let mut rows = Vec::with_capacity(
        usize::try_from(count)
            .map_err(|_| reader.invalid("runtime binding row count is too large"))?,
    );
    for _ in 0..count {
        let runtime_name = reader.take_str("runtime binding name")?;
        let version = reader.take_str("runtime binding version")?;
        let descriptor_count = reader.take_u64("runtime binding descriptor count")?;
        let mut consumed_descriptors = Vec::with_capacity(
            usize::try_from(descriptor_count)
                .map_err(|_| reader.invalid("runtime binding descriptor count is too large"))?,
        );
        for _ in 0..descriptor_count {
            consumed_descriptors.push(take_descriptor(reader)?);
        }
        let contract_count = reader.take_u64("runtime binding contract count")?;
        let mut contracts = Vec::with_capacity(
            usize::try_from(contract_count)
                .map_err(|_| reader.invalid("runtime binding contract count is too large"))?,
        );
        for _ in 0..contract_count {
            let name = reader.take_str("runtime binding contract name")?;
            let version = reader.take_str("runtime binding contract version")?;
            let feature_count = reader.take_u64("runtime binding contract feature count")?;
            let mut features =
                Vec::with_capacity(usize::try_from(feature_count).map_err(|_| {
                    reader.invalid("runtime binding contract feature count is too large")
                })?);
            for _ in 0..feature_count {
                features.push(reader.take_str("runtime binding contract feature")?);
            }
            contracts.push((name, version, features));
        }
        let trusted = reader.take_flag("runtime binding trust")?;
        let preference_rank = reader.take_u32("runtime binding preference rank")?;
        rows.push(
            RuntimeBindingRow::new(
                runtime_name,
                version,
                consumed_descriptors,
                contracts,
                trusted,
                preference_rank,
            )
            .map_err(|_| reader.invalid("runtime binding row is not canonical"))?,
        );
    }
    Ok(rows)
}

fn push_security_decisions(writer: &mut PayloadWriter, rows: &[SecurityDecisionRow]) {
    writer.push_u64(rows.len() as u64);
    for row in rows {
        writer.push_u8(match row.kind() {
            InspectSecurityDecisionKind::Execute => 0,
            InspectSecurityDecisionKind::Capability => 1,
            InspectSecurityDecisionKind::UserState => 2,
            InspectSecurityDecisionKind::Inspect => 3,
        });
        writer.push_u8(match row.outcome() {
            InspectSecurityDecisionOutcome::Allowed => 0,
            InspectSecurityDecisionOutcome::Denied => 1,
        });
        writer.push_u64(row.principals().len() as u64);
        for principal in row.principals() {
            writer.push_id(&principal.to_bytes());
        }
        writer.push_opt_id(row.target().map(|target| target.to_bytes()));
        writer.push_flag(row.denial_reason().is_some());
        if let Some(reason) = row.denial_reason() {
            writer.push_str(reason);
        }
        writer.push_u64(row.audit_refs().len() as u64);
        for reference in row.audit_refs() {
            writer.push_id(&reference.to_bytes());
        }
    }
}

fn take_security_decisions(
    reader: &mut PayloadReader<'_>,
) -> Result<Vec<SecurityDecisionRow>, PostgresKernelError> {
    let count = reader.take_u64("security decision row count")?;
    let mut rows = Vec::with_capacity(
        usize::try_from(count)
            .map_err(|_| reader.invalid("security decision row count is too large"))?,
    );
    for _ in 0..count {
        let kind = match reader.take_u8("security decision kind")? {
            0 => InspectSecurityDecisionKind::Execute,
            1 => InspectSecurityDecisionKind::Capability,
            2 => InspectSecurityDecisionKind::UserState,
            3 => InspectSecurityDecisionKind::Inspect,
            _ => return Err(reader.invalid("security decision kind is outside the closed set")),
        };
        let outcome = match reader.take_u8("security decision outcome")? {
            0 => InspectSecurityDecisionOutcome::Allowed,
            1 => InspectSecurityDecisionOutcome::Denied,
            _ => return Err(reader.invalid("security decision outcome is outside the closed set")),
        };
        let principal_count = reader.take_u64("security decision principal count")?;
        let mut principals = Vec::with_capacity(
            usize::try_from(principal_count)
                .map_err(|_| reader.invalid("security decision principal count is too large"))?,
        );
        for _ in 0..principal_count {
            principals.push(PrincipalId::from_bytes(
                reader.take_id("security decision principal identity")?,
            ));
        }
        let target = reader
            .take_opt_id("security decision target identity")?
            .map(FunctionId::from_bytes);
        let denial_reason = if reader.take_flag("security decision denial flag")? {
            Some(reader.take_str("security decision denial reason")?)
        } else {
            None
        };
        let reference_count = reader.take_u64("security decision audit reference count")?;
        let mut audit_refs =
            Vec::with_capacity(usize::try_from(reference_count).map_err(|_| {
                reader.invalid("security decision audit reference count is too large")
            })?);
        for _ in 0..reference_count {
            audit_refs.push(SecurityAuditEventId::from_bytes(
                reader.take_id("security decision audit reference identity")?,
            ));
        }
        rows.push(
            SecurityDecisionRow::new(kind, outcome, principals, target, denial_reason, audit_refs)
                .map_err(|_| reader.invalid("security decision row is not canonical"))?,
        );
    }
    Ok(rows)
}

fn push_optional_invoke_value(
    writer: &mut PayloadWriter,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    value: Option<&InvokeValue>,
) -> Result<(), PostgresKernelError> {
    writer.push_flag(value.is_some());
    if let Some(value) = value {
        let bytes =
            encode_constructed_value(active, registry, &RuntimeValue::InvokeValue(value.clone()))
                .map_err(PostgresKernelError::InspectValueCodec)?;
        writer.push_bytes(&bytes);
    }
    Ok(())
}

fn take_optional_invoke_value(
    reader: &mut PayloadReader<'_>,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
) -> Result<Option<InvokeValue>, PostgresKernelError> {
    if !reader.take_flag("typed value flag")? {
        return Ok(None);
    }
    let bytes = reader.take_bytes("typed value payload")?;
    let RuntimeValue::InvokeValue(value) = decode_constructed_value(active, registry, &bytes)
        .map_err(PostgresKernelError::InspectValueCodec)?
    else {
        return Err(reader.invalid("typed value payload must decode as one invoke value"));
    };
    Ok(Some(value))
}

fn push_descriptor(writer: &mut PayloadWriter, descriptor: &TypeDescriptor) {
    match descriptor.kind() {
        orna_core::types::TypeDescriptorKind::Named(id) => {
            writer.push_u8(0);
            writer.push_id(&id.to_bytes());
        }
        orna_core::types::TypeDescriptorKind::Reference(target) => {
            writer.push_u8(1);
            writer.push_id(&target.to_bytes());
        }
        orna_core::types::TypeDescriptorKind::List(element) => {
            writer.push_u8(2);
            push_descriptor(writer, element);
        }
        orna_core::types::TypeDescriptorKind::Set(element) => {
            writer.push_u8(3);
            push_descriptor(writer, element);
        }
        orna_core::types::TypeDescriptorKind::Map { key, value } => {
            writer.push_u8(4);
            push_descriptor(writer, key);
            push_descriptor(writer, value);
        }
        orna_core::types::TypeDescriptorKind::Option(value) => {
            writer.push_u8(5);
            push_descriptor(writer, value);
        }
        orna_core::types::TypeDescriptorKind::Stream(element) => {
            writer.push_u8(6);
            push_descriptor(writer, element);
        }
    }
}

fn take_descriptor(reader: &mut PayloadReader<'_>) -> Result<TypeDescriptor, PostgresKernelError> {
    let tag = reader.take_u8("type descriptor tag")?;
    match tag {
        0 => Ok(TypeDescriptor::named(TypeId::from_bytes(
            reader.take_id("type descriptor identity")?,
        ))),
        1 => Ok(TypeDescriptor::reference(TypeId::from_bytes(
            reader.take_id("type descriptor reference identity")?,
        ))),
        2 => TypeDescriptor::list(take_descriptor(reader)?)
            .map_err(|_| reader.invalid("type descriptor is not canonical")),
        3 => TypeDescriptor::set(take_descriptor(reader)?)
            .map_err(|_| reader.invalid("type descriptor is not canonical")),
        4 => {
            let key = take_descriptor(reader)?;
            let value = take_descriptor(reader)?;
            TypeDescriptor::map(key, value)
                .map_err(|_| reader.invalid("type descriptor is not canonical"))
        }
        5 => TypeDescriptor::option(take_descriptor(reader)?)
            .map_err(|_| reader.invalid("type descriptor is not canonical")),
        6 => TypeDescriptor::stream(take_descriptor(reader)?)
            .map_err(|_| reader.invalid("type descriptor is not canonical")),
        _ => Err(reader.invalid("type descriptor tag is outside the closed set")),
    }
}
