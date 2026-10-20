//! Durable Query observations and local materialization projections.

use std::time::SystemTime;

use libsql::{Connection, TransactionBehavior, params};
use orna_foundation_v1::{
    FunctionRef, ObjectRef, OvbRaw, RowRef, SnapshotRef, object_reference, snapshot_reference,
};

use crate::{
    CwdCapture, InvocationObservationStatus, RuntimeError, RuntimeState, WriterLease,
    bigint_to_i64, capture_tx, decode_capture, encode_capture, fixed, now_ms,
    observation_instant, opaque_reference_key, row_reference_raw, validate_id,
    validate_observation_text, validate_writer_lease,
};

/// Stable relation identities for system rows whose typed Foundation aliases
/// are not yet defined by this implementation's reference layer.
pub const SYS_QUERY_TABLE_ID: [u8; 16] = [
    0x30, 0xea, 0x29, 0x65, 0xfc, 0xc8, 0x5e, 0xef, 0x8d, 0x64, 0x36, 0x8b, 0xaf, 0x5c, 0x9b, 0x26,
];
pub const SYS_MATERIALIZATION_TABLE_ID: [u8; 16] = [
    0x79, 0xe9, 0x91, 0x4e, 0x22, 0x4a, 0x51, 0xb3, 0xba, 0xa7, 0xa3, 0x65, 0x2c, 0x8d, 0xf8, 0xbc,
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryObservationRegistration {
    pub id: [u8; 16],
    pub expression: Option<String>,
    pub capture: CwdCapture,
}

/// Durable execution lifecycle retained for the sys.Query relation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryObservation {
    pub id: [u8; 16],
    pub expression: Option<String>,
    pub snapshot: CwdCapture,
    pub runtime_id: [u8; 16],
    pub runtime_generation: i64,
    pub started_ms: i64,
    pub ended_ms: Option<i64>,
    pub observed_ms: i64,
    pub status: InvocationObservationStatus,
}

/// Durable-field projection for sys.Query. Fields without runtime evidence
/// remain absent rather than being inferred from the displayed expression.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SysQueryProjection {
    pub reference: RowRef,
    pub id: [u8; 16],
    pub expression: Option<String>,
    pub function: Option<FunctionRef>,
    pub snapshot: SnapshotRef,
    pub started: SystemTime,
    pub ended: Option<SystemTime>,
    pub status: InvocationObservationStatus,
    pub rows: Option<u64>,
    pub bytes: Option<u64>,
    pub duration: Option<std::time::Duration>,
    pub plan: Option<RowRef>,
    pub trace: Option<RowRef>,
    pub failure: Option<crate::SafeDiagnostic>,
}

impl TryFrom<&QueryObservation> for SysQueryProjection {
    type Error = RuntimeError;

    fn try_from(observation: &QueryObservation) -> Result<Self, Self::Error> {
        validate_id(observation.id)?;
        let generation = bigint_to_i64(observation.snapshot.generation())
            .map_err(|_| RuntimeError::InvalidObservationReference)?;
        if observation.runtime_id != observation.snapshot.runtime_id()
            || observation.runtime_generation != generation
        {
            return Err(RuntimeError::InvalidObservationReference);
        }
        let reference = RowRef::new(
            observation.snapshot.database_id(),
            SYS_QUERY_TABLE_ID,
            opaque_reference_key(observation.id),
            observation.snapshot.snapshot().clone(),
        )
        .map_err(|_| RuntimeError::InvalidObservationReference)?;
        let snapshot = snapshot_reference(
            observation.snapshot.database_id(),
            observation.snapshot.snapshot().clone(),
        )
        .map_err(|_| RuntimeError::InvalidObservationReference)?;
        let started = observation_instant(observation.started_ms)?;
        let observed_at = observation_instant(observation.observed_ms)?;
        let ended = observation.ended_ms.map(observation_instant).transpose()?;
        if observed_at < started
            || observation.status.is_terminal() != ended.is_some()
            || ended.is_some_and(|ended| ended < started || observed_at < ended)
        {
            return Err(RuntimeError::RecoveryInvalid);
        }
        Ok(Self {
            reference,
            id: observation.id,
            expression: observation.expression.clone(),
            function: None,
            snapshot,
            started,
            ended,
            status: observation.status,
            rows: None,
            bytes: None,
            duration: None,
            plan: None,
            trace: None,
            failure: None,
        })
    }
}

/// Local durable projection of one cached/materialized catalogue function.
/// The current runtime cache retains function metadata, not query-result rows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SysMaterializationProjection {
    pub reference: RowRef,
    pub object: ObjectRef,
    pub snapshot: SnapshotRef,
    pub digest: [u8; 32],
    pub rows: Option<u64>,
    pub bytes: Option<u64>,
    pub created: SystemTime,
    pub valid: bool,
}

impl RuntimeState {
    /// Durably starts a query observation at the exact staged-activation
    /// capture. The row is visible only after this owner-checked commit.
    pub async fn begin_query_observation(
        &self,
        writer: WriterLease,
        registration: QueryObservationRegistration,
    ) -> Result<QueryObservation, RuntimeError> {
        validate_id(registration.id)?;
        validate_writer_lease(writer)?;
        if let Some(expression) = &registration.expression {
            validate_observation_text(expression)?;
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        self.require_owner(&tx, writer).await?;
        let current = capture_tx(&tx).await?;
        if current != registration.capture {
            return Err(RuntimeError::StaleCapture {
                current: Box::new(current),
            });
        }
        let started_ms = now_ms()?;
        let runtime_generation = bigint_to_i64(registration.capture.generation())
            .map_err(|_| RuntimeError::InvalidObservationReference)?;
        let snapshot = encode_capture(&registration.capture)?;
        tx.execute(
            "INSERT INTO sys_query_observation
             (query_id, expression, snapshot, generation_digest, runtime_id,
              runtime_generation, started_ms, ended_ms, observed_ms, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?7, ?8)",
            params![
                registration.id.to_vec(),
                registration.expression.clone(),
                snapshot,
                registration.capture.generation_digest().to_vec(),
                registration.capture.runtime_id().to_vec(),
                runtime_generation,
                started_ms,
                query_status_code(InvocationObservationStatus::Running),
            ],
        )
        .await
        .map_err(|_| RuntimeError::QueryObservationConflict)?;
        tx.commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(QueryObservation {
            id: registration.id,
            expression: registration.expression,
            snapshot: registration.capture.clone(),
            runtime_id: registration.capture.runtime_id(),
            runtime_generation,
            started_ms,
            ended_ms: None,
            observed_ms: started_ms,
            status: InvocationObservationStatus::Running,
        })
    }

    /// Ends a running query observation. Repeating the same terminal outcome
    /// is idempotent; a different outcome cannot rewrite the retained row.
    pub async fn finish_query_observation(
        &self,
        writer: WriterLease,
        id: [u8; 16],
        status: InvocationObservationStatus,
    ) -> Result<QueryObservation, RuntimeError> {
        validate_id(id)?;
        validate_writer_lease(writer)?;
        if !status.is_terminal() {
            return Err(RuntimeError::QueryStateConflict);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        self.require_owner(&tx, writer).await?;
        let mut observation = load_query_observation_tx(&tx, id)
            .await?
            .ok_or(RuntimeError::QueryObservationConflict)?;
        if observation.status.is_terminal() {
            if observation.status == status {
                tx.commit()
                    .await
                    .map_err(|_| RuntimeError::StorageUnavailable)?;
                return Ok(observation);
            }
            return Err(RuntimeError::QueryStateConflict);
        }
        if observation.status != InvocationObservationStatus::Running {
            return Err(RuntimeError::QueryStateConflict);
        }
        let ended_ms = now_ms()?.max(observation.started_ms);
        tx.execute(
            "UPDATE sys_query_observation
             SET ended_ms = ?1, observed_ms = ?1, status = ?2
             WHERE query_id = ?3 AND status = ?4 AND ended_ms IS NULL",
            params![
                ended_ms,
                query_status_code(status),
                id.to_vec(),
                query_status_code(InvocationObservationStatus::Running),
            ],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
        tx.commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        observation.ended_ms = Some(ended_ms);
        observation.observed_ms = ended_ms;
        observation.status = status;
        Ok(observation)
    }

    /// Reads all durable query observations from one database snapshot.
    pub async fn query_observations(&self) -> Result<Vec<QueryObservation>, RuntimeError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let observations = load_query_observations(&tx).await?;
        tx.commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(observations)
    }

    /// Reads one durable query observation without changing runtime state.
    pub async fn query_observation(
        &self,
        id: [u8; 16],
    ) -> Result<Option<QueryObservation>, RuntimeError> {
        validate_id(id)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let observation = load_query_observation_tx(&tx, id).await?;
        tx.commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(observation)
    }

    /// Reads the local durable catalogue materialization cache as sys.Materialization.
    pub async fn sys_materialization_projections(
        &self,
    ) -> Result<Vec<SysMaterializationProjection>, RuntimeError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let projections = load_sys_materialization_projections(&tx).await?;
        tx.commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(projections)
    }
}

pub(super) fn project_query_observations(
    observations: &[QueryObservation],
) -> Result<Vec<SysQueryProjection>, RuntimeError> {
    observations.iter().map(SysQueryProjection::try_from).collect()
}

pub(super) async fn load_query_observations(
    connection: &Connection,
) -> Result<Vec<QueryObservation>, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT query_id FROM sys_query_observation ORDER BY started_ms, query_id",
            (),
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let mut observations = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    {
        let id = fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        observations.push(
            load_query_observation_tx(connection, id)
                .await?
                .ok_or(RuntimeError::RecoveryInvalid)?,
        );
    }
    Ok(observations)
}

async fn load_query_observation_tx(
    connection: &Connection,
    id: [u8; 16],
) -> Result<Option<QueryObservation>, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT expression, snapshot, generation_digest, runtime_id,
                    runtime_generation, started_ms, ended_ms, observed_ms, status
             FROM sys_query_observation WHERE query_id = ?1",
            params![id.to_vec()],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    else {
        return Ok(None);
    };
    let expression: Option<String> = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
    if let Some(expression) = &expression {
        validate_observation_text(expression)?;
    }
    let snapshot_bytes: Vec<u8> = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let generation_digest = fixed(row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    let runtime_id = fixed(row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    let runtime_generation: i64 = row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let snapshot = decode_capture(snapshot_bytes, generation_digest)?;
    if snapshot.runtime_id() != runtime_id
        || bigint_to_i64(snapshot.generation())? != runtime_generation
    {
        return Err(RuntimeError::RecoveryInvalid);
    }
    let started_ms: i64 = row.get(5).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let ended_ms: Option<i64> = row.get(6).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let observed_ms: i64 = row.get(7).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let status = query_status_from_code(row.get(8).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    if observed_ms < started_ms
        || status.is_terminal() != ended_ms.is_some()
        || ended_ms.is_some_and(|ended| ended < started_ms || observed_ms < ended)
    {
        return Err(RuntimeError::RecoveryInvalid);
    }
    Ok(Some(QueryObservation {
        id,
        expression,
        snapshot,
        runtime_id,
        runtime_generation,
        started_ms,
        ended_ms,
        observed_ms,
        status,
    }))
}

async fn load_sys_materialization_projections(
    connection: &Connection,
) -> Result<Vec<SysMaterializationProjection>, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT materialization.function_object_id, materialization.snapshot,
                    materialization.revision_id, materialization.semantic_hash,
                    materialization.result_type_object_id, materialization.materialized_ms,
                    capture.database_id, capture.runtime_id, capture.generation,
                    capture.generation_digest, revision.revision_id, revision.semantic_hash,
                    function.result_type_object_id
             FROM sys_procedure_materialization AS materialization
             LEFT JOIN runtime_catalogue_capture AS capture
               ON capture.snapshot = materialization.snapshot
             LEFT JOIN runtime_catalogue_revision AS revision
               ON revision.object_id = materialization.function_object_id
              AND revision.snapshot = materialization.snapshot
             LEFT JOIN runtime_catalogue_function AS function
               ON function.object_id = materialization.function_object_id
              AND function.snapshot = materialization.snapshot
             ORDER BY materialization.materialized_ms,
                      materialization.function_object_id, materialization.snapshot",
            (),
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let mut projections = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    {
        let function_object_id = fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        let snapshot_bytes: Vec<u8> = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let revision_id = fixed::<32>(row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        let digest = fixed::<32>(row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        let result_type_id = fixed::<16>(row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        let materialized_ms: i64 = row.get(5).map_err(|_| RuntimeError::RecoveryInvalid)?;
        if materialized_ms < 0 {
            return Err(RuntimeError::RecoveryInvalid);
        }
        let database_id = optional_fixed::<16>(row.get(6).map_err(|_| RuntimeError::RecoveryInvalid)?)?
            .ok_or(RuntimeError::RecoveryInvalid)?;
        let runtime_id = optional_fixed::<16>(row.get(7).map_err(|_| RuntimeError::RecoveryInvalid)?)?
            .ok_or(RuntimeError::RecoveryInvalid)?;
        let generation: Option<i64> = row.get(8).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let generation_digest = optional_fixed::<32>(row.get(9).map_err(|_| RuntimeError::RecoveryInvalid)?)?
            .ok_or(RuntimeError::RecoveryInvalid)?;
        let capture = decode_capture(snapshot_bytes, generation_digest)?;
        if capture.database_id() != database_id
            || capture.runtime_id() != runtime_id
            || bigint_to_i64(capture.generation())? != generation.ok_or(RuntimeError::RecoveryInvalid)?
        {
            return Err(RuntimeError::RecoveryInvalid);
        }
        let catalogue_revision_id = optional_fixed::<32>(row.get(10).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        let catalogue_digest = optional_fixed::<32>(row.get(11).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        let catalogue_result_type_id = optional_fixed::<16>(row.get(12).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        let valid = catalogue_revision_id == Some(revision_id)
            && catalogue_digest == Some(digest)
            && catalogue_result_type_id == Some(result_type_id);
        let object = object_reference(
            capture.database_id(),
            function_object_id,
            capture.snapshot().clone(),
        )
        .map_err(|_| RuntimeError::InvalidObservationReference)?;
        let snapshot = snapshot_reference(
            capture.database_id(),
            capture.snapshot().clone(),
        )
        .map_err(|_| RuntimeError::InvalidObservationReference)?;
        let reference = RowRef::new(
            capture.database_id(),
            SYS_MATERIALIZATION_TABLE_ID,
            OvbRaw::Array(vec![
                row_reference_raw(object.as_row_ref()),
                row_reference_raw(snapshot.as_row_ref()),
            ]),
            capture.snapshot().clone(),
        )
        .map_err(|_| RuntimeError::InvalidObservationReference)?;
        projections.push(SysMaterializationProjection {
            reference,
            object,
            snapshot,
            digest,
            rows: None,
            bytes: None,
            created: observation_instant(materialized_ms)?,
            valid,
        });
    }
    Ok(projections)
}

fn optional_fixed<const N: usize>(value: Option<Vec<u8>>) -> Result<Option<[u8; N]>, RuntimeError> {
    value.map(fixed).transpose()
}

fn query_status_code(status: InvocationObservationStatus) -> i64 {
    match status {
        InvocationObservationStatus::Queued => 1,
        InvocationObservationStatus::Running => 2,
        InvocationObservationStatus::Succeeded => 3,
        InvocationObservationStatus::Failed => 4,
        InvocationObservationStatus::Cancelled => 5,
        InvocationObservationStatus::Orphaned => 6,
    }
}

fn query_status_from_code(code: i64) -> Result<InvocationObservationStatus, RuntimeError> {
    match code {
        1 => Ok(InvocationObservationStatus::Queued),
        2 => Ok(InvocationObservationStatus::Running),
        3 => Ok(InvocationObservationStatus::Succeeded),
        4 => Ok(InvocationObservationStatus::Failed),
        5 => Ok(InvocationObservationStatus::Cancelled),
        6 => Ok(InvocationObservationStatus::Orphaned),
        _ => Err(RuntimeError::RecoveryInvalid),
    }
}
