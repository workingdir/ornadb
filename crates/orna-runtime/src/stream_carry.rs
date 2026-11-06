//! Carries durable stream consumer checkpoints across a format-3 migration.
//!
//! The carry copies each `stream_checkpoint` row verbatim and records its
//! version and position as a migration bridge origin. The destination
//! validator starts that consumer's checkpoint chain from the origin, so no
//! history row is needed to prove the carried state. The source's
//! `stream_checkpoint_history` rows are archived verbatim in
//! `stream_checkpoint_migration_archive` and are not validated. The stream key
//! text is built from the consumer's natural identity and is unchanged by the
//! migration. Every carried row must be covered by a migration continuity
//! predecessor that the destination repository has already recorded, and the
//! import verifies that before the destination runtime is opened or any row is
//! written.
//!
//! Live or per-attempt stream state (leases, claims, failures, receipts,
//! bootstrap pins, pending pause requests) is not carried. A source holding such
//! rows is refused rather than reset silently. Stream control (running or
//! paused), the pause reason and the checkpoint reset audit are carried
//! verbatim. The reset audit is durable history: each row keeps its sequence,
//! old and new typed positions and reason, so historical reads stay exact.
//! Run and stream observations (`sys_run_observation`, `sys_stream_observation`)
//! are carried verbatim, runs before streams, because a stream references its
//! run. A source holding a live run (Starting or Running) is refused: its
//! terminal state is not yet known. `live` is never stored; it is derived at
//! read time.

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::io::ErrorKind;

use libsql::{Builder, Connection, TransactionBehavior, params};
use orna_repository::{MigrationContinuityRecord, Repository, StreamCheckpointIdentityPredecessor};
use orna_stream::{Component, ConsumerIdentity};
use orna_value::{Raw, sov3_context_free_domain_digest};
use sha2::{Digest as _, Sha256};

use crate::{RuntimeError, RuntimeState, migration_copy_runtime_identity};

/// Stream tables whose rows a migration does not carry.
const NON_CARRIABLE_TABLES: &[&str] = &[
    "stream_checkpoint_bootstrap",
    "stream_failure",
    "stream_failure_assertion_detail",
    "stream_failure_payload",
    "stream_failure_payload_legacy",
    "stream_lease",
    "stream_pause_pending",
    "stream_provider_effect_receipt",
    "stream_provider_failure",
    "stream_replay_claim",
    "stream_retry_claim",
];

const INSERT_CHECKPOINT: &str = "INSERT INTO stream_checkpoint (key_id, consumer_principal, consumer_root, consumer_function, consumer_binding, source_format, source, partition_format, partition, position_format, version, committed_position, next_fence) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)";
const INSERT_BRIDGE: &str = "INSERT INTO stream_checkpoint_migration_bridge (key_id, origin_version, origin_position, source_generation, source_generation_digest) VALUES (?1, ?2, ?3, ?4, ?5)";
const INSERT_ARCHIVE: &str = "INSERT INTO stream_checkpoint_migration_archive (key_id, version, committed_position, snapshot, generation_digest, transition) VALUES (?1, ?2, ?3, ?4, ?5, ?6)";
const INSERT_CONTROL: &str = "INSERT INTO stream_control (key_id, status) VALUES (?1, ?2)";
const INSERT_PAUSE_REASON: &str =
    "INSERT INTO stream_pause_reason (key_id, reason) VALUES (?1, ?2)";
const INSERT_RESET_AUDIT: &str = "INSERT INTO stream_checkpoint_reset_audit (sequence, key_id, observed_generation, old_version, old_position, new_version, new_position, reason, redacted) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)";
/// `sys_run_observation` status codes for a run that has not reached a terminal state.
const RUN_STATUS_STARTING: i64 = 1;
const RUN_STATUS_RUNNING: i64 = 2;
const INSERT_RUN: &str = "INSERT INTO sys_run_observation (run_id, session_id, request_id, consumer_identity, function_name, source_identity, invocation_id, snapshot, generation_digest, runtime_id, runtime_generation, started_ms, ended_ms, observed_ms, status, checkpoint_count, diagnostic_code, diagnostic_class) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)";
const INSERT_STREAM: &str = "INSERT INTO sys_stream_observation (stream_id, run_id, checkpoint_key_id, producer, consumer_name, consumer_identity, source_identity, partition, status, items_seen, items_committed, items_failed, checkpoint_version, last_failure_identity, last_item_ms, diagnostic_code, diagnostic_class, observed_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)";

#[derive(Clone, Debug, Eq, PartialEq)]
struct CheckpointRow {
    key_id: String,
    principal: String,
    root: String,
    function: String,
    binding: String,
    source_format: String,
    source: String,
    partition_format: String,
    partition: Option<String>,
    position_format: String,
    version: i64,
    committed_position: Option<String>,
    next_fence: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct HistoryRow {
    key_id: String,
    version: i64,
    committed_position: String,
    snapshot: Vec<u8>,
    generation_digest: Vec<u8>,
    transition: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct BridgeRow {
    key_id: String,
    origin_version: i64,
    origin_position: Option<String>,
    source_generation: i64,
    source_generation_digest: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ControlRow {
    key_id: String,
    status: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PauseReasonRow {
    key_id: String,
    reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ResetAuditRow {
    sequence: i64,
    key_id: String,
    observed_generation: i64,
    old_version: i64,
    old_position: Option<String>,
    new_version: i64,
    new_position: String,
    reason: String,
    redacted: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RunRow {
    run_id: Vec<u8>,
    session_id: Vec<u8>,
    request_id: Vec<u8>,
    consumer_identity: String,
    function_name: String,
    source_identity: Option<String>,
    invocation_id: Vec<u8>,
    snapshot: Vec<u8>,
    generation_digest: Vec<u8>,
    runtime_id: Vec<u8>,
    runtime_generation: i64,
    started_ms: i64,
    ended_ms: Option<i64>,
    observed_ms: i64,
    status: i64,
    checkpoint_count: i64,
    diagnostic_code: Option<i64>,
    diagnostic_class: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StreamRow {
    stream_id: Vec<u8>,
    run_id: Vec<u8>,
    checkpoint_key_id: String,
    producer: String,
    consumer_name: Option<String>,
    consumer_identity: String,
    source_identity: String,
    partition: Option<String>,
    status: i64,
    items_seen: i64,
    items_committed: i64,
    items_failed: i64,
    checkpoint_version: Option<i64>,
    last_failure_identity: Option<String>,
    last_item_ms: Option<i64>,
    diagnostic_code: Option<i64>,
    diagnostic_class: Option<i64>,
    observed_ms: i64,
}

/// Exact stream checkpoint state read from a source repository for carry.
#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct StreamCarry {
    checkpoints: Vec<CheckpointRow>,
    bridges: Vec<BridgeRow>,
    history: Vec<HistoryRow>,
    controls: Vec<ControlRow>,
    pause_reasons: Vec<PauseReasonRow>,
    reset_audits: Vec<ResetAuditRow>,
    runs: Vec<RunRow>,
    streams: Vec<StreamRow>,
}

/// Why a source's stream state could not be carried.
#[derive(Debug, Eq, PartialEq)]
pub enum StreamCarryError {
    /// The source holds non-carriable stream rows in the listed tables.
    NotCarriable(Vec<&'static str>),
    Runtime(RuntimeError),
}

impl StreamCarry {
    pub fn checkpoint_count(&self) -> usize {
        self.checkpoints.len()
    }

    pub fn history_count(&self) -> usize {
        self.history.len()
    }

    /// Identity correspondence for every carried consumer. The result must be
    /// recorded in the destination repository before the carry is imported.
    pub fn continuity_record(&self) -> Result<MigrationContinuityRecord, RuntimeError> {
        let mut seen = BTreeSet::new();
        let mut predecessors = Vec::new();
        for row in &self.checkpoints {
            let legacy = legacy_consumer_identity(row)?;
            let format3 = format3_consumer_identity(row)?;
            let source = row.source.as_bytes().to_vec();
            let partition = row
                .partition
                .as_ref()
                .map(|value| value.as_bytes().to_vec());
            if !seen.insert((legacy, source.clone(), partition.clone())) {
                continue;
            }
            predecessors.push(
                StreamCheckpointIdentityPredecessor::new(
                    legacy,
                    source.clone(),
                    partition.clone(),
                    format3,
                    source,
                    partition,
                )
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
            );
        }
        MigrationContinuityRecord::new(predecessors).map_err(|_| RuntimeError::RecoveryInvalid)
    }
}

/// Legacy consumer identity: SHA-256 of `orna.consumer.v1`, NUL, and the
/// canonical `consumer/v1` text of the natural consumer identity.
fn legacy_consumer_identity(row: &CheckpointRow) -> Result<[u8; 32], RuntimeError> {
    let consumer = ConsumerIdentity {
        principal: component(&row.principal)?,
        root: component(&row.root)?,
        function: component(&row.function)?,
        binding: component(&row.binding)?,
    };
    let mut hash = Sha256::new();
    hash.update(b"orna.consumer.v1");
    hash.update([0]);
    hash.update(consumer.canonical().as_bytes());
    Ok(hash.finalize().into())
}

/// Format-3 consumer identity: SOV-3 domain digest over the natural consumer
/// identity components. Code revision is excluded.
fn format3_consumer_identity(row: &CheckpointRow) -> Result<[u8; 32], RuntimeError> {
    let payload = Raw::Array(vec![
        Raw::Text(row.principal.clone()),
        Raw::Text(row.root.clone()),
        Raw::Text(row.function.clone()),
        Raw::Text(row.binding.clone()),
    ]);
    sov3_context_free_domain_digest("orna.consumer.v3", &payload)
        .map_err(|_| RuntimeError::RecoveryInvalid)
}

fn component(value: &str) -> Result<Component, RuntimeError> {
    Component::new(value.to_owned()).map_err(|_| RuntimeError::RecoveryInvalid)
}

/// Reads the source's carriable stream checkpoints. A source with no
/// `state.db` holds no state. Any non-carriable row refuses the carry.
pub async fn read_stream_carry(source: &Repository) -> Result<StreamCarry, StreamCarryError> {
    let path = source.runtime_paths().state_db();
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(StreamCarry::default()),
        Ok(metadata) if metadata.is_file() => {}
        _ => return Err(StreamCarryError::Runtime(RuntimeError::StorageUnavailable)),
    }
    let database = Builder::new_local(&path)
        .build()
        .await
        .map_err(|_| StreamCarryError::Runtime(RuntimeError::StorageUnavailable))?;
    let connection = database
        .connect()
        .map_err(|_| StreamCarryError::Runtime(RuntimeError::StorageUnavailable))?;
    let tables = table_names(&connection)
        .await
        .map_err(StreamCarryError::Runtime)?;

    let mut refused = Vec::new();
    for table in NON_CARRIABLE_TABLES {
        if tables.contains(*table)
            && count_rows(&connection, table)
                .await
                .map_err(StreamCarryError::Runtime)?
                > 0
        {
            refused.push(*table);
        }
    }
    if !refused.is_empty() {
        return Err(StreamCarryError::NotCarriable(refused));
    }

    let checkpoints = if tables.contains("stream_checkpoint") {
        read_checkpoints(&connection)
            .await
            .map_err(StreamCarryError::Runtime)?
    } else {
        Vec::new()
    };
    let history = if tables.contains("stream_checkpoint_history") {
        read_history(&connection, "stream_checkpoint_history")
            .await
            .map_err(StreamCarryError::Runtime)?
    } else {
        Vec::new()
    };
    let bridges = if checkpoints.is_empty() {
        Vec::new()
    } else {
        let (source_generation, source_generation_digest) = read_source_generation(&connection)
            .await
            .map_err(StreamCarryError::Runtime)?;
        checkpoints
            .iter()
            .map(|row| BridgeRow {
                key_id: row.key_id.clone(),
                origin_version: row.version,
                origin_position: row.committed_position.clone(),
                source_generation,
                source_generation_digest: source_generation_digest.clone(),
            })
            .collect()
    };
    let controls = if tables.contains("stream_control") {
        read_controls(&connection)
            .await
            .map_err(StreamCarryError::Runtime)?
    } else {
        Vec::new()
    };
    let pause_reasons = if tables.contains("stream_pause_reason") {
        read_pause_reasons(&connection)
            .await
            .map_err(StreamCarryError::Runtime)?
    } else {
        Vec::new()
    };
    let reset_audits = if tables.contains("stream_checkpoint_reset_audit") {
        read_reset_audits(&connection)
            .await
            .map_err(StreamCarryError::Runtime)?
    } else {
        Vec::new()
    };
    let runs = if tables.contains("sys_run_observation") {
        read_run_rows(&connection)
            .await
            .map_err(StreamCarryError::Runtime)?
    } else {
        Vec::new()
    };
    if runs
        .iter()
        .any(|run| run.status == RUN_STATUS_STARTING || run.status == RUN_STATUS_RUNNING)
    {
        return Err(StreamCarryError::NotCarriable(vec!["sys_run_observation"]));
    }
    let streams = if tables.contains("sys_stream_observation") {
        read_stream_rows(&connection)
            .await
            .map_err(StreamCarryError::Runtime)?
    } else {
        Vec::new()
    };
    Ok(StreamCarry {
        checkpoints,
        bridges,
        history,
        controls,
        pause_reasons,
        reset_audits,
        runs,
        streams,
    })
}

/// Imports a carry into the destination repository. The destination's recorded
/// continuity must cover every carried row before the destination runtime is
/// opened or any row is written; otherwise the import refuses and writes
/// nothing.
pub async fn import_stream_carry(
    destination: &Repository,
    carry: &StreamCarry,
    recorded: Option<&MigrationContinuityRecord>,
) -> Result<(), RuntimeError> {
    let recorded = recorded.ok_or(RuntimeError::StreamIdentityMismatch)?;
    for row in &carry.checkpoints {
        let legacy = legacy_consumer_identity(row)?;
        let format3 = format3_consumer_identity(row)?;
        let source = row.source.as_bytes();
        let partition = row.partition.as_deref().map(str::as_bytes);
        let Some((mapped, mapped_source, mapped_partition)) =
            recorded.successor_for(legacy, source, partition)
        else {
            return Err(RuntimeError::StreamIdentityMismatch);
        };
        if mapped != format3 || mapped_source != source || mapped_partition != partition {
            return Err(RuntimeError::StreamIdentityMismatch);
        }
    }

    let (identity, digest) = migration_copy_runtime_identity(destination)?;
    let state = RuntimeState::open(destination, identity, digest).await?;
    let present = read_carry_target(&state.connection).await?;
    if present == *carry {
        return Ok(());
    }
    if present != StreamCarry::default() {
        return Err(RuntimeError::MigrationCarryTargetDiffers);
    }
    let transaction = state
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    for row in &carry.checkpoints {
        transaction
            .execute(
                INSERT_CHECKPOINT,
                params![
                    row.key_id.as_str(),
                    row.principal.as_str(),
                    row.root.as_str(),
                    row.function.as_str(),
                    row.binding.as_str(),
                    row.source_format.as_str(),
                    row.source.as_str(),
                    row.partition_format.as_str(),
                    row.partition.as_deref(),
                    row.position_format.as_str(),
                    row.version,
                    row.committed_position.as_deref(),
                    row.next_fence
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
    }
    for row in &carry.bridges {
        transaction
            .execute(
                INSERT_BRIDGE,
                params![
                    row.key_id.as_str(),
                    row.origin_version,
                    row.origin_position.as_deref(),
                    row.source_generation,
                    row.source_generation_digest.as_slice()
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
    }
    for row in &carry.controls {
        transaction
            .execute(INSERT_CONTROL, params![row.key_id.as_str(), row.status])
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
    }
    for row in &carry.pause_reasons {
        transaction
            .execute(
                INSERT_PAUSE_REASON,
                params![row.key_id.as_str(), row.reason.as_str()],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
    }
    for row in &carry.history {
        transaction
            .execute(
                INSERT_ARCHIVE,
                params![
                    row.key_id.as_str(),
                    row.version,
                    row.committed_position.as_str(),
                    row.snapshot.as_slice(),
                    row.generation_digest.as_slice(),
                    row.transition
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
    }
    for row in &carry.reset_audits {
        transaction
            .execute(
                INSERT_RESET_AUDIT,
                params![
                    row.sequence,
                    row.key_id.as_str(),
                    row.observed_generation,
                    row.old_version,
                    row.old_position.as_deref(),
                    row.new_version,
                    row.new_position.as_str(),
                    row.reason.as_str(),
                    row.redacted
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
    }
    for row in &carry.runs {
        transaction
            .execute(
                INSERT_RUN,
                params![
                    row.run_id.as_slice(),
                    row.session_id.as_slice(),
                    row.request_id.as_slice(),
                    row.consumer_identity.as_str(),
                    row.function_name.as_str(),
                    row.source_identity.as_deref(),
                    row.invocation_id.as_slice(),
                    row.snapshot.as_slice(),
                    row.generation_digest.as_slice(),
                    row.runtime_id.as_slice(),
                    row.runtime_generation,
                    row.started_ms,
                    row.ended_ms,
                    row.observed_ms,
                    row.status,
                    row.checkpoint_count,
                    row.diagnostic_code,
                    row.diagnostic_class
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
    }
    for row in &carry.streams {
        transaction
            .execute(
                INSERT_STREAM,
                params![
                    row.stream_id.as_slice(),
                    row.run_id.as_slice(),
                    row.checkpoint_key_id.as_str(),
                    row.producer.as_str(),
                    row.consumer_name.as_deref(),
                    row.consumer_identity.as_str(),
                    row.source_identity.as_str(),
                    row.partition.as_deref(),
                    row.status,
                    row.items_seen,
                    row.items_committed,
                    row.items_failed,
                    row.checkpoint_version,
                    row.last_failure_identity.as_deref(),
                    row.last_item_ms,
                    row.diagnostic_code,
                    row.diagnostic_class,
                    row.observed_ms
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
    }
    transaction
        .commit()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    Ok(())
}

async fn read_source_generation(connection: &Connection) -> Result<(i64, Vec<u8>), RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT generation, generation_digest FROM runtime_meta WHERE singleton = 1",
            (),
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let row = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
        .ok_or(RuntimeError::RecoveryInvalid)?;
    let generation: i64 = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let digest: Vec<u8> = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
    if generation < 0 || digest.len() != 32 {
        return Err(RuntimeError::RecoveryInvalid);
    }
    Ok((generation, digest))
}

async fn table_names(connection: &Connection) -> Result<HashSet<String>, RuntimeError> {
    let mut rows = connection
        .query("SELECT name FROM sqlite_master WHERE type = 'table'", ())
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let mut names = HashSet::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    {
        let name: String = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
        names.insert(name);
    }
    Ok(names)
}

async fn count_rows(connection: &Connection, table: &str) -> Result<i64, RuntimeError> {
    let mut rows = connection
        .query(&format!("SELECT COUNT(*) FROM {table}"), ())
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let row = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
        .ok_or(RuntimeError::RecoveryInvalid)?;
    let count: i64 = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
    Ok(count)
}

async fn read_checkpoints(connection: &Connection) -> Result<Vec<CheckpointRow>, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT key_id, consumer_principal, consumer_root, consumer_function, consumer_binding, source_format, source, partition_format, partition, position_format, version, committed_position, next_fence FROM stream_checkpoint ORDER BY key_id",
            (),
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let mut checkpoints = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    {
        checkpoints.push(CheckpointRow {
            key_id: row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?,
            principal: row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?,
            root: row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?,
            function: row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?,
            binding: row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?,
            source_format: row.get(5).map_err(|_| RuntimeError::RecoveryInvalid)?,
            source: row.get(6).map_err(|_| RuntimeError::RecoveryInvalid)?,
            partition_format: row.get(7).map_err(|_| RuntimeError::RecoveryInvalid)?,
            partition: row.get(8).map_err(|_| RuntimeError::RecoveryInvalid)?,
            position_format: row.get(9).map_err(|_| RuntimeError::RecoveryInvalid)?,
            version: row.get(10).map_err(|_| RuntimeError::RecoveryInvalid)?,
            committed_position: row.get(11).map_err(|_| RuntimeError::RecoveryInvalid)?,
            next_fence: row.get(12).map_err(|_| RuntimeError::RecoveryInvalid)?,
        });
    }
    Ok(checkpoints)
}

async fn read_history(
    connection: &Connection,
    table: &str,
) -> Result<Vec<HistoryRow>, RuntimeError> {
    let mut rows = connection
        .query(
            &format!("SELECT key_id, version, committed_position, snapshot, generation_digest, transition FROM {table} ORDER BY key_id, version"),
            (),
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let mut history = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    {
        history.push(HistoryRow {
            key_id: row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?,
            version: row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?,
            committed_position: row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?,
            snapshot: row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?,
            generation_digest: row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?,
            transition: row.get(5).map_err(|_| RuntimeError::RecoveryInvalid)?,
        });
    }
    Ok(history)
}

/// Reads the rows a carry writes into the destination, in `StreamCarry` order.
async fn read_carry_target(connection: &Connection) -> Result<StreamCarry, RuntimeError> {
    Ok(StreamCarry {
        checkpoints: read_checkpoints(connection).await?,
        bridges: read_bridges(connection).await?,
        history: read_history(connection, "stream_checkpoint_migration_archive").await?,
        controls: read_controls(connection).await?,
        pause_reasons: read_pause_reasons(connection).await?,
        reset_audits: read_reset_audits(connection).await?,
        runs: read_run_rows(connection).await?,
        streams: read_stream_rows(connection).await?,
    })
}

async fn read_bridges(connection: &Connection) -> Result<Vec<BridgeRow>, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT key_id, origin_version, origin_position, source_generation, source_generation_digest FROM stream_checkpoint_migration_bridge ORDER BY key_id",
            (),
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let mut bridges = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    {
        bridges.push(BridgeRow {
            key_id: row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?,
            origin_version: row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?,
            origin_position: row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?,
            source_generation: row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?,
            source_generation_digest: row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?,
        });
    }
    Ok(bridges)
}

async fn read_controls(connection: &Connection) -> Result<Vec<ControlRow>, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT key_id, status FROM stream_control ORDER BY key_id",
            (),
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let mut controls = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    {
        controls.push(ControlRow {
            key_id: row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?,
            status: row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?,
        });
    }
    Ok(controls)
}

async fn read_pause_reasons(connection: &Connection) -> Result<Vec<PauseReasonRow>, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT key_id, reason FROM stream_pause_reason ORDER BY key_id",
            (),
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let mut reasons = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    {
        reasons.push(PauseReasonRow {
            key_id: row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?,
            reason: row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?,
        });
    }
    Ok(reasons)
}

async fn read_reset_audits(connection: &Connection) -> Result<Vec<ResetAuditRow>, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT sequence, key_id, observed_generation, old_version, old_position, new_version, new_position, reason, redacted FROM stream_checkpoint_reset_audit ORDER BY sequence",
            (),
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let mut audits = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    {
        audits.push(ResetAuditRow {
            sequence: row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?,
            key_id: row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?,
            observed_generation: row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?,
            old_version: row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?,
            old_position: row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?,
            new_version: row.get(5).map_err(|_| RuntimeError::RecoveryInvalid)?,
            new_position: row.get(6).map_err(|_| RuntimeError::RecoveryInvalid)?,
            reason: row.get(7).map_err(|_| RuntimeError::RecoveryInvalid)?,
            redacted: row.get(8).map_err(|_| RuntimeError::RecoveryInvalid)?,
        });
    }
    Ok(audits)
}

/// Reads `sys_run_observation` rows in primary-key order, for a carry and for
/// the destination comparison.
async fn read_run_rows(connection: &Connection) -> Result<Vec<RunRow>, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT run_id, session_id, request_id, consumer_identity, function_name, source_identity, invocation_id, snapshot, generation_digest, runtime_id, runtime_generation, started_ms, ended_ms, observed_ms, status, checkpoint_count, diagnostic_code, diagnostic_class FROM sys_run_observation ORDER BY run_id",
            (),
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let mut runs = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    {
        runs.push(RunRow {
            run_id: row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?,
            session_id: row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?,
            request_id: row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?,
            consumer_identity: row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?,
            function_name: row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?,
            source_identity: row.get(5).map_err(|_| RuntimeError::RecoveryInvalid)?,
            invocation_id: row.get(6).map_err(|_| RuntimeError::RecoveryInvalid)?,
            snapshot: row.get(7).map_err(|_| RuntimeError::RecoveryInvalid)?,
            generation_digest: row.get(8).map_err(|_| RuntimeError::RecoveryInvalid)?,
            runtime_id: row.get(9).map_err(|_| RuntimeError::RecoveryInvalid)?,
            runtime_generation: row.get(10).map_err(|_| RuntimeError::RecoveryInvalid)?,
            started_ms: row.get(11).map_err(|_| RuntimeError::RecoveryInvalid)?,
            ended_ms: row.get(12).map_err(|_| RuntimeError::RecoveryInvalid)?,
            observed_ms: row.get(13).map_err(|_| RuntimeError::RecoveryInvalid)?,
            status: row.get(14).map_err(|_| RuntimeError::RecoveryInvalid)?,
            checkpoint_count: row.get(15).map_err(|_| RuntimeError::RecoveryInvalid)?,
            diagnostic_code: row.get(16).map_err(|_| RuntimeError::RecoveryInvalid)?,
            diagnostic_class: row.get(17).map_err(|_| RuntimeError::RecoveryInvalid)?,
        });
    }
    Ok(runs)
}

/// Reads `sys_stream_observation` rows in primary-key order, for a carry and
/// for the destination comparison.
async fn read_stream_rows(connection: &Connection) -> Result<Vec<StreamRow>, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT stream_id, run_id, checkpoint_key_id, producer, consumer_name, consumer_identity, source_identity, partition, status, items_seen, items_committed, items_failed, checkpoint_version, last_failure_identity, last_item_ms, diagnostic_code, diagnostic_class, observed_ms FROM sys_stream_observation ORDER BY stream_id",
            (),
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let mut streams = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    {
        streams.push(StreamRow {
            stream_id: row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?,
            run_id: row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?,
            checkpoint_key_id: row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?,
            producer: row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?,
            consumer_name: row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?,
            consumer_identity: row.get(5).map_err(|_| RuntimeError::RecoveryInvalid)?,
            source_identity: row.get(6).map_err(|_| RuntimeError::RecoveryInvalid)?,
            partition: row.get(7).map_err(|_| RuntimeError::RecoveryInvalid)?,
            status: row.get(8).map_err(|_| RuntimeError::RecoveryInvalid)?,
            items_seen: row.get(9).map_err(|_| RuntimeError::RecoveryInvalid)?,
            items_committed: row.get(10).map_err(|_| RuntimeError::RecoveryInvalid)?,
            items_failed: row.get(11).map_err(|_| RuntimeError::RecoveryInvalid)?,
            checkpoint_version: row.get(12).map_err(|_| RuntimeError::RecoveryInvalid)?,
            last_failure_identity: row.get(13).map_err(|_| RuntimeError::RecoveryInvalid)?,
            last_item_ms: row.get(14).map_err(|_| RuntimeError::RecoveryInvalid)?,
            diagnostic_code: row.get(15).map_err(|_| RuntimeError::RecoveryInvalid)?,
            diagnostic_class: row.get(16).map_err(|_| RuntimeError::RecoveryInvalid)?,
            observed_ms: row.get(17).map_err(|_| RuntimeError::RecoveryInvalid)?,
        });
    }
    Ok(streams)
}
