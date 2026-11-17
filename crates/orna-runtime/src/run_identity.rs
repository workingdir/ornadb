//! Catalogue object identity carried by retained run and stream observations.
//!
//! The `sys.Run` and `sys.Stream` projections expose typed object references, so a run or stream row stores the
//! catalogue object id that its function, producer and consumer names resolve to in the snapshot the row pins. A row
//! written before this migration, or whose name does not resolve in that snapshot, keeps NULL. The projections fail
//! closed on NULL, and on a stored id that the pinned snapshot no longer resolves from the same name, instead of
//! deriving an identity from a name.

use std::collections::BTreeSet;

use libsql::{Connection, Transaction, TransactionBehavior, params};
use orna_value::{
    CwdCapture, FunctionRef, ObjectRef, function_reference, object_reference,
    validate_function_reference, validate_object_reference,
};

use crate::RuntimeState;
use crate::catalogue::{
    CatalogueObjectKind, lookup_snapshot_object_id, map_catalogue_runtime_error, object_kind_tx,
};
use crate::codec::{bigint_to_i64, encode_capture, fixed, now_ms};
use crate::error::RuntimeError;
use crate::observation::{
    RunObservation, RunObservationId, RunObservationRegistration, RunObservationStatus,
    RuntimeObservationView, StreamObservation, StreamObservationId, StreamObservationRegistration,
    StreamObservationStatus, run_status_code, stream_observation_status_code,
};

impl RuntimeState {
    /// Adds the nullable object-identity columns without reinterpreting any
    /// retained observation. Existing rows are deliberately not backfilled.
    pub(crate) async fn migrate_run_stream_object_identity(&self) -> Result<(), RuntimeError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        let run_columns = table_columns(&transaction, "sys_run_observation").await?;
        let stream_columns = table_columns(&transaction, "sys_stream_observation").await?;
        if !run_columns.contains("function_object_id") {
            transaction
                .execute(
                    "ALTER TABLE sys_run_observation
                     ADD COLUMN function_object_id BLOB
                     CHECK (function_object_id IS NULL OR length(function_object_id) = 16)",
                    (),
                )
                .await?;
        }
        if !stream_columns.contains("producer_object_id") {
            transaction
                .execute(
                    "ALTER TABLE sys_stream_observation
                     ADD COLUMN producer_object_id BLOB
                     CHECK (producer_object_id IS NULL OR length(producer_object_id) = 16)",
                    (),
                )
                .await?;
        }
        if !stream_columns.contains("consumer_object_id") {
            transaction
                .execute(
                    "ALTER TABLE sys_stream_observation
                     ADD COLUMN consumer_object_id BLOB
                     CHECK (consumer_object_id IS NULL OR length(consumer_object_id) = 16)",
                    (),
                )
                .await?;
        }
        transaction
            .execute(
                "INSERT OR IGNORE INTO runtime_schema_migration (migration)
                 VALUES ('run-stream-object-identity-v1')",
                (),
            )
            .await?;
        transaction.commit().await.map_err(RuntimeError::from)
    }
}

async fn table_columns(
    connection: &Connection,
    table: &str,
) -> Result<BTreeSet<String>, RuntimeError> {
    let mut rows = connection
        .query(&format!("PRAGMA table_info({table})"), ())
        .await?;
    let mut columns = BTreeSet::new();
    while let Some(row) = rows.next().await? {
        columns.insert(
            row.get::<String>(1)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        );
    }
    Ok(columns)
}

/// The select expression that reads `column` of `table`, or NULL when the
/// table predates the column. A carry reads its source without opening it as a
/// runtime, so the source may not have run the identity migration.
pub(crate) async fn carried_column(
    connection: &Connection,
    table: &str,
    column: &str,
) -> Result<String, RuntimeError> {
    let present = table_columns(connection, table).await?.contains(column);
    Ok(if present { column } else { "NULL" }.to_owned())
}

/// The object id `name` resolves to in the snapshot `capture` pins, or `None`
/// when that snapshot does not catalogue the name.
async fn resolve_object(
    connection: &Connection,
    capture: &CwdCapture,
    name: &str,
) -> Result<Option<[u8; 16]>, RuntimeError> {
    lookup_snapshot_object_id(connection, &encode_capture(capture)?, name)
        .await
        .map_err(map_catalogue_runtime_error)
}

/// As [`resolve_object`], with a name bound to anything but a function
/// resolving to `None`.
async fn resolve_function(
    connection: &Connection,
    capture: &CwdCapture,
    name: &str,
) -> Result<Option<[u8; 16]>, RuntimeError> {
    let Some(id) = resolve_object(connection, capture, name).await? else {
        return Ok(None);
    };
    let kind = object_kind_tx(connection, id)
        .await
        .map_err(map_catalogue_runtime_error)?;
    Ok((kind == CatalogueObjectKind::Function).then_some(id))
}

/// The object id stored in column `index` of `row`; `None` for NULL.
pub(crate) fn object_id_column(
    row: &libsql::Row,
    index: i32,
) -> Result<Option<[u8; 16]>, RuntimeError> {
    row.get::<Option<Vec<u8>>>(index)
        .map_err(|_| RuntimeError::RecoveryInvalid)?
        .map(fixed::<16>)
        .transpose()
}

/// Inserts a stream observation with the catalogue identities of its producer
/// and consumer, resolved in the snapshot its parent run pins. A name that
/// snapshot does not catalogue stores NULL, as does a consumer that is not a
/// function; the projections then fail closed.
pub(crate) async fn insert_stream_observation_tx(
    transaction: &Transaction,
    id: StreamObservationId,
    registration: &StreamObservationRegistration,
    parent: &CwdCapture,
    key_id: &str,
    partition: Option<&str>,
) -> Result<(), RuntimeError> {
    let producer_object_id = resolve_object(transaction, parent, &registration.producer).await?;
    let consumer_object_id = match &registration.consumer {
        Some(name) => resolve_function(transaction, parent, name).await?,
        None => None,
    };
    transaction
        .execute(
            "INSERT INTO sys_stream_observation (stream_id, run_id, checkpoint_key_id, producer, consumer_name, consumer_identity, source_identity, partition, status, items_seen, items_committed, items_failed, checkpoint_version, last_failure_identity, last_item_ms, diagnostic_code, diagnostic_class, observed_ms, producer_object_id, consumer_object_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, 0, 0, NULL, NULL, NULL, NULL, NULL, ?10, ?11, ?12)",
            params![
                id.0.to_vec(),
                registration.run.0.to_vec(),
                key_id,
                registration.producer.clone(),
                registration.consumer.clone(),
                registration.checkpoint.consumer.canonical(),
                registration.checkpoint.source.as_str(),
                partition,
                stream_observation_status_code(StreamObservationStatus::Starting),
                now_ms()?,
                producer_object_id.map(|object_id| object_id.to_vec()),
                consumer_object_id.map(|object_id| object_id.to_vec()),
            ],
        )
        .await?;
    Ok(())
}

/// Inserts a run observation with the catalogue identity of its function,
/// resolved in the admission capture. A name the capture does not catalogue
/// stores NULL, and the projections then fail closed instead of deriving an
/// identity from the name.
pub(crate) async fn insert_run_observation_tx(
    transaction: &Transaction,
    id: RunObservationId,
    registration: &RunObservationRegistration,
    capture: &CwdCapture,
    status: RunObservationStatus,
) -> Result<(), RuntimeError> {
    let function_object_id = resolve_function(transaction, capture, &registration.function).await?;
    transaction
        .execute(
            "INSERT INTO sys_run_observation (run_id, session_id, request_id, consumer_identity, function_name, source_identity, invocation_id, snapshot, generation_digest, runtime_id, runtime_generation, started_ms, ended_ms, observed_ms, status, checkpoint_count, diagnostic_code, diagnostic_class, function_object_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, NULL, ?12, ?13, 0, NULL, NULL, ?14)",
            params![
                id.0.to_vec(),
                registration.request.session_id.to_vec(),
                registration.request.request_id.to_vec(),
                registration.consumer_identity.canonical(),
                registration.function.clone(),
                registration.source_identity.clone(),
                registration.invocation_id.to_vec(),
                encode_capture(capture)?,
                capture.generation_digest().to_vec(),
                capture.runtime_id().to_vec(),
                bigint_to_i64(capture.generation())?,
                now_ms()?,
                run_status_code(status),
                function_object_id.map(|object_id| object_id.to_vec()),
            ],
        )
        .await?;
    Ok(())
}

/// Confirms every stored identity of `view` against the snapshot its row pins:
/// the stored id must be what the row's name resolves to there, and the
/// consumer's a function. NULL is left to the projections, which fail closed on
/// it. A mismatch means the row was altered or does not belong to this
/// catalogue, so no reference is built from it.
pub(crate) async fn verify_observation_identities(
    connection: &Connection,
    view: &RuntimeObservationView,
) -> Result<(), RuntimeError> {
    for run in &view.runs {
        if let Some(stored) = run.function_object_id
            && resolve_function(connection, &run.snapshot, &run.function).await? != Some(stored)
        {
            return Err(RuntimeError::RecoveryInvalid);
        }
    }
    for stream in &view.streams {
        let parent = &stream.parent_capture;
        if let Some(stored) = stream.producer_object_id
            && resolve_object(connection, parent, &stream.producer).await? != Some(stored)
        {
            return Err(RuntimeError::RecoveryInvalid);
        }
        let consistent = match (&stream.consumer, stream.consumer_object_id) {
            (_, None) => true,
            (None, Some(_)) => false,
            (Some(name), Some(stored)) => {
                resolve_function(connection, parent, name).await? == Some(stored)
            }
        };
        if !consistent {
            return Err(RuntimeError::RecoveryInvalid);
        }
    }
    Ok(())
}

fn object_ref(capture: &CwdCapture, object_id: [u8; 16]) -> Result<ObjectRef, RuntimeError> {
    let object = object_reference(capture.database_id(), object_id, capture.snapshot())
        .map_err(|_| RuntimeError::InvalidObservationReference)?;
    validate_object_reference(object.into_row_ref(), capture)
        .map_err(|_| RuntimeError::InvalidObservationReference)
}

fn function_ref(capture: &CwdCapture, object_id: [u8; 16]) -> Result<FunctionRef, RuntimeError> {
    let function = function_reference(object_ref(capture, object_id)?)
        .map_err(|_| RuntimeError::InvalidObservationReference)?;
    validate_function_reference(function.into_row_ref(), capture)
        .map_err(|_| RuntimeError::InvalidObservationReference)
}

impl RunObservation {
    /// Reconstructs this retained observation's checked `sys.FunctionRef` from
    /// the catalogue identity stored with the run, pinned to the run's own
    /// capture. A run without one (retained before the identity migration, or
    /// admitted with a function its snapshot did not catalogue) fails closed.
    pub fn function_reference(&self) -> Result<FunctionRef, RuntimeError> {
        let object_id = self
            .function_object_id
            .ok_or(RuntimeError::RecoveryInvalid)?;
        function_ref(&self.snapshot, object_id)
    }
}

impl StreamObservation {
    /// Reconstructs the checked `sys.ObjectRef` of this stream's producer from
    /// the catalogue identity stored with the stream, pinned to its parent
    /// run's capture. A stream without one fails closed.
    pub fn producer_reference(&self) -> Result<ObjectRef, RuntimeError> {
        let object_id = self
            .producer_object_id
            .ok_or(RuntimeError::RecoveryInvalid)?;
        object_ref(&self.parent_capture, object_id)
    }

    /// Reconstructs the checked optional `sys.FunctionRef` of this stream's
    /// consumer. A stream with no consumer name has none; a recorded consumer
    /// without a stored identity fails closed.
    pub fn consumer_reference(&self) -> Result<Option<FunctionRef>, RuntimeError> {
        match (&self.consumer, self.consumer_object_id) {
            (None, None) => Ok(None),
            (Some(_), Some(object_id)) => function_ref(&self.parent_capture, object_id).map(Some),
            _ => Err(RuntimeError::RecoveryInvalid),
        }
    }
}
