//! Carries durable stream consumer checkpoints across a format-3 migration.
//!
//! The carry copies `stream_checkpoint` and `stream_checkpoint_history` rows
//! verbatim. The stream key text is built from the consumer's natural identity
//! and is unchanged by the migration, so the rows keep their meaning. Every
//! carried row must be covered by a migration continuity predecessor that the
//! destination repository has already recorded, and the import verifies that
//! before the destination runtime is opened or any row is written.
//!
//! Live or per-attempt stream state (leases, claims, failures, receipts,
//! controls, pause state, reset audit, bootstrap pins) is not carried. A source
//! holding such rows is refused rather than reset silently.

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::io::ErrorKind;

use libsql::{Builder, Connection, TransactionBehavior, params};
use orna_repository_v1::{
    MigrationContinuityRecord, Repository, StreamCheckpointIdentityPredecessor,
};
use orna_stream_v1::{Component, ConsumerIdentity};
use orna_value_v1::{Raw, sov3_context_free_domain_digest};
use sha2::{Digest as _, Sha256};

use crate::{RuntimeError, RuntimeState, repository_runtime_identity};

/// Stream tables whose rows a migration does not carry.
const NON_CARRIABLE_TABLES: &[&str] = &[
    "stream_checkpoint_bootstrap",
    "stream_checkpoint_reset_audit",
    "stream_control",
    "stream_failure",
    "stream_failure_assertion_detail",
    "stream_failure_payload",
    "stream_failure_payload_legacy",
    "stream_lease",
    "stream_pause_pending",
    "stream_pause_reason",
    "stream_provider_effect_receipt",
    "stream_provider_failure",
    "stream_replay_claim",
    "stream_retry_claim",
];

const INSERT_CHECKPOINT: &str = "INSERT INTO stream_checkpoint (key_id, consumer_principal, consumer_root, consumer_function, consumer_binding, source_format, source, partition_format, partition, position_format, version, committed_position, next_fence) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)";
const INSERT_HISTORY: &str = "INSERT INTO stream_checkpoint_history (key_id, version, committed_position, snapshot, generation_digest, transition) VALUES (?1, ?2, ?3, ?4, ?5, ?6)";

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

/// Exact stream checkpoint state read from a source repository for carry.
#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct StreamCarry {
    checkpoints: Vec<CheckpointRow>,
    history: Vec<HistoryRow>,
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
            let partition = row.partition.as_ref().map(|value| value.as_bytes().to_vec());
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
    let tables = table_names(&connection).await.map_err(StreamCarryError::Runtime)?;

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
        read_history(&connection)
            .await
            .map_err(StreamCarryError::Runtime)?
    } else {
        Vec::new()
    };
    Ok(StreamCarry {
        checkpoints,
        history,
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

    let (identity, digest) = repository_runtime_identity(destination)?;
    let state = RuntimeState::open(destination, identity, digest).await?;
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
    for row in &carry.history {
        transaction
            .execute(
                INSERT_HISTORY,
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
    transaction
        .commit()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    Ok(())
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

async fn read_history(connection: &Connection) -> Result<Vec<HistoryRow>, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT key_id, version, committed_position, snapshot, generation_digest, transition FROM stream_checkpoint_history ORDER BY key_id, version",
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
