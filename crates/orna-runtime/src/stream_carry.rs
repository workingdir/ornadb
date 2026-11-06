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
//! Live or per-attempt stream state (leases, claims, pending pause requests, and
//! in-flight failures and receipts) is not carried. A source holding such rows
//! is refused rather than reset silently. Failure records that are not in flight
//! are carried verbatim with their payload, assertion-detail, legacy and provider
//! companion rows. Settled provider effect receipts (statuses 2, 3 and 4) are
//! carried verbatim. Stream control (running or paused), the pause reason and the
//! checkpoint bootstrap pin are carried verbatim.

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
    "stream_checkpoint_reset_audit",
    "stream_lease",
    "stream_pause_pending",
    "stream_replay_claim",
    "stream_retry_claim",
];

const INSERT_CHECKPOINT: &str = "INSERT INTO stream_checkpoint (key_id, consumer_principal, consumer_root, consumer_function, consumer_binding, source_format, source, partition_format, partition, position_format, version, committed_position, next_fence) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)";
const INSERT_BRIDGE: &str = "INSERT INTO stream_checkpoint_migration_bridge (key_id, origin_version, origin_position, source_generation, source_generation_digest) VALUES (?1, ?2, ?3, ?4, ?5)";
const INSERT_ARCHIVE: &str = "INSERT INTO stream_checkpoint_migration_archive (key_id, version, committed_position, snapshot, generation_digest, transition) VALUES (?1, ?2, ?3, ?4, ?5, ?6)";
const INSERT_CONTROL: &str = "INSERT INTO stream_control (key_id, status) VALUES (?1, ?2)";
const INSERT_PAUSE_REASON: &str =
    "INSERT INTO stream_pause_reason (key_id, reason) VALUES (?1, ?2)";
const INSERT_BOOTSTRAP: &str =
    "INSERT INTO stream_checkpoint_bootstrap (key_id, selected_commit) VALUES (?1, ?2)";

const INSERT_FAILURE: &str = "INSERT INTO stream_failure (identity_id, key_id, consumer_principal, consumer_root, consumer_function, consumer_binding, source_format, source, partition_format, partition, position_format, delivery_position, successor_position, version, attempts, status, diagnostic_code, diagnostic_class) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)";
const INSERT_FAILURE_PAYLOAD: &str = "INSERT INTO stream_failure_payload (identity_id, payload, payload_reference, payload_digest, retention) VALUES (?1, ?2, ?3, ?4, ?5)";
const INSERT_FAILURE_LEGACY: &str =
    "INSERT INTO stream_failure_payload_legacy (identity_id) VALUES (?1)";
const INSERT_ASSERTION_DETAIL: &str = "INSERT INTO stream_failure_assertion_detail (identity_id, code, assertion_ref, owner_ref, owner_kind, predicate_ref, source_file_ref, start_byte, end_byte, start_line, start_column, end_line, end_column, safe_witness) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)";
const INSERT_PROVIDER_FAILURE: &str = "INSERT INTO stream_provider_failure (key_id, checkpoint_version, committed_position, attempts, diagnostic_code, diagnostic_class) VALUES (?1, ?2, ?3, ?4, ?5, ?6)";
const INSERT_RECEIPT: &str = "INSERT INTO stream_provider_effect_receipt (effect_identity, request_digest, idempotency_key, lease_fence, attempts_in_fence, total_attempts, status, receipt) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)";

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
struct BootstrapRow {
    key_id: String,
    selected_commit: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ReceiptRow {
    effect_identity: Vec<u8>,
    request_digest: Vec<u8>,
    idempotency_key: Vec<u8>,
    lease_fence: i64,
    attempts_in_fence: i64,
    total_attempts: i64,
    status: i64,
    receipt: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FailureRow {
    identity_id: String,
    key_id: String,
    consumer_principal: String,
    consumer_root: String,
    consumer_function: String,
    consumer_binding: String,
    source_format: String,
    source: String,
    partition_format: String,
    partition: Option<String>,
    position_format: String,
    delivery_position: String,
    successor_position: String,
    version: i64,
    attempts: i64,
    status: i64,
    diagnostic_code: i64,
    diagnostic_class: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FailurePayloadRow {
    identity_id: String,
    payload: Option<Vec<u8>>,
    payload_reference: Option<String>,
    payload_digest: Option<Vec<u8>>,
    retention: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FailureLegacyRow {
    identity_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AssertionDetailRow {
    identity_id: String,
    code: String,
    assertion_ref: Vec<u8>,
    owner_ref: Vec<u8>,
    owner_kind: i64,
    predicate_ref: Vec<u8>,
    source_file_ref: Vec<u8>,
    start_byte: Vec<u8>,
    end_byte: Vec<u8>,
    start_line: Vec<u8>,
    start_column: Vec<u8>,
    end_line: Vec<u8>,
    end_column: Vec<u8>,
    safe_witness: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProviderFailureRow {
    key_id: String,
    checkpoint_version: i64,
    committed_position: Option<String>,
    attempts: i64,
    diagnostic_code: i64,
    diagnostic_class: i64,
}

/// Failure records and their companion rows, carried verbatim.
#[derive(Clone, Debug, Eq, PartialEq, Default)]
struct FailureCarry {
    failures: Vec<FailureRow>,
    payloads: Vec<FailurePayloadRow>,
    legacy: Vec<FailureLegacyRow>,
    assertion_details: Vec<AssertionDetailRow>,
    provider_failures: Vec<ProviderFailureRow>,
}

/// Exact stream checkpoint state read from a source repository for carry.
#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct StreamCarry {
    checkpoints: Vec<CheckpointRow>,
    bridges: Vec<BridgeRow>,
    history: Vec<HistoryRow>,
    controls: Vec<ControlRow>,
    pause_reasons: Vec<PauseReasonRow>,
    bootstraps: Vec<BootstrapRow>,
    failures: FailureCarry,
    receipts: Vec<ReceiptRow>,
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
    if tables.contains("stream_failure")
        && in_flight_failure_count(&connection)
            .await
            .map_err(StreamCarryError::Runtime)?
            > 0
    {
        refused.push("stream_failure");
    }
    if tables.contains("stream_provider_effect_receipt")
        && in_flight_receipt_count(&connection)
            .await
            .map_err(StreamCarryError::Runtime)?
            > 0
    {
        refused.push("stream_provider_effect_receipt");
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
    let bootstraps = if tables.contains("stream_checkpoint_bootstrap") {
        read_bootstraps(&connection)
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
        bootstraps,
        failures: read_failure_carry(&connection)
            .await
            .map_err(StreamCarryError::Runtime)?,
        receipts: read_receipt_carry(&connection)
            .await
            .map_err(StreamCarryError::Runtime)?,
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
    for row in &carry.bootstraps {
        transaction
            .execute(
                INSERT_BOOTSTRAP,
                params![row.key_id.as_str(), row.selected_commit.as_str()],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
    }
    insert_failure_carry(&transaction, &carry.failures).await?;
    insert_receipt_carry(&transaction, &carry.receipts).await?;
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
        bootstraps: read_bootstraps(connection).await?,
        failures: read_failure_carry(connection).await?,
        receipts: read_receipt_carry(connection).await?,
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

async fn read_bootstraps(connection: &Connection) -> Result<Vec<BootstrapRow>, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT key_id, selected_commit FROM stream_checkpoint_bootstrap ORDER BY key_id",
            (),
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let mut bootstraps = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    {
        bootstraps.push(BootstrapRow {
            key_id: row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?,
            selected_commit: row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?,
        });
    }
    Ok(bootstraps)
}

/// Reads the provider effect receipts that a carry copies verbatim. A source
/// without the table carries none. Receipts in flight (status 1) are refused
/// before this read runs.
async fn read_receipt_carry(connection: &Connection) -> Result<Vec<ReceiptRow>, RuntimeError> {
    if !table_names(connection)
        .await?
        .contains("stream_provider_effect_receipt")
    {
        return Ok(Vec::new());
    }
    let mut rows = connection
        .query(
            "SELECT effect_identity, request_digest, idempotency_key, lease_fence, attempts_in_fence, total_attempts, status, receipt FROM stream_provider_effect_receipt ORDER BY effect_identity",
            (),
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let mut receipts = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    {
        receipts.push(ReceiptRow {
            effect_identity: row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?,
            request_digest: row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?,
            idempotency_key: row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?,
            lease_fence: row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?,
            attempts_in_fence: row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?,
            total_attempts: row.get(5).map_err(|_| RuntimeError::RecoveryInvalid)?,
            status: row.get(6).map_err(|_| RuntimeError::RecoveryInvalid)?,
            receipt: row.get(7).map_err(|_| RuntimeError::RecoveryInvalid)?,
        });
    }
    Ok(receipts)
}

/// Counts receipts in flight (status 1, the lease-fenced attempt). Such a receipt
/// names a provider effect claim that is not carried, so the carry refuses it.
async fn in_flight_receipt_count(connection: &Connection) -> Result<i64, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT COUNT(*) FROM stream_provider_effect_receipt WHERE status = 1",
            (),
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let row = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
        .ok_or(RuntimeError::RecoveryInvalid)?;
    row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)
}

/// Reads every failure table that exists in `connection`. The source may not
/// have the tables; the destination always has them after its runtime opens.
async fn read_failure_carry(connection: &Connection) -> Result<FailureCarry, RuntimeError> {
    let tables = table_names(connection).await?;
    Ok(FailureCarry {
        failures: if tables.contains("stream_failure") {
            read_failures(connection).await?
        } else {
            Vec::new()
        },
        payloads: if tables.contains("stream_failure_payload") {
            read_failure_payloads(connection).await?
        } else {
            Vec::new()
        },
        legacy: if tables.contains("stream_failure_payload_legacy") {
            read_failure_legacy(connection).await?
        } else {
            Vec::new()
        },
        assertion_details: if tables.contains("stream_failure_assertion_detail") {
            read_assertion_details(connection).await?
        } else {
            Vec::new()
        },
        provider_failures: if tables.contains("stream_provider_failure") {
            read_provider_failures(connection).await?
        } else {
            Vec::new()
        },
    })
}

/// Counts failure rows in an in-flight claim state (`Retrying`, `Replaying`).
/// Those rows name a claim that is not carried, so the carry refuses them.
async fn in_flight_failure_count(connection: &Connection) -> Result<i64, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT COUNT(*) FROM stream_failure WHERE status IN (2, 5)",
            (),
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let row = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
        .ok_or(RuntimeError::RecoveryInvalid)?;
    row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)
}

async fn read_failures(connection: &Connection) -> Result<Vec<FailureRow>, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT identity_id, key_id, consumer_principal, consumer_root, consumer_function, consumer_binding, source_format, source, partition_format, partition, position_format, delivery_position, successor_position, version, attempts, status, diagnostic_code, diagnostic_class FROM stream_failure ORDER BY identity_id",
            (),
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let mut failures = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    {
        failures.push(FailureRow {
            identity_id: row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?,
            key_id: row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?,
            consumer_principal: row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?,
            consumer_root: row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?,
            consumer_function: row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?,
            consumer_binding: row.get(5).map_err(|_| RuntimeError::RecoveryInvalid)?,
            source_format: row.get(6).map_err(|_| RuntimeError::RecoveryInvalid)?,
            source: row.get(7).map_err(|_| RuntimeError::RecoveryInvalid)?,
            partition_format: row.get(8).map_err(|_| RuntimeError::RecoveryInvalid)?,
            partition: row.get(9).map_err(|_| RuntimeError::RecoveryInvalid)?,
            position_format: row.get(10).map_err(|_| RuntimeError::RecoveryInvalid)?,
            delivery_position: row.get(11).map_err(|_| RuntimeError::RecoveryInvalid)?,
            successor_position: row.get(12).map_err(|_| RuntimeError::RecoveryInvalid)?,
            version: row.get(13).map_err(|_| RuntimeError::RecoveryInvalid)?,
            attempts: row.get(14).map_err(|_| RuntimeError::RecoveryInvalid)?,
            status: row.get(15).map_err(|_| RuntimeError::RecoveryInvalid)?,
            diagnostic_code: row.get(16).map_err(|_| RuntimeError::RecoveryInvalid)?,
            diagnostic_class: row.get(17).map_err(|_| RuntimeError::RecoveryInvalid)?,
        });
    }
    Ok(failures)
}

async fn read_failure_payloads(
    connection: &Connection,
) -> Result<Vec<FailurePayloadRow>, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT identity_id, payload, payload_reference, payload_digest, retention FROM stream_failure_payload ORDER BY identity_id",
            (),
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let mut payloads = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    {
        payloads.push(FailurePayloadRow {
            identity_id: row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?,
            payload: row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?,
            payload_reference: row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?,
            payload_digest: row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?,
            retention: row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?,
        });
    }
    Ok(payloads)
}

async fn read_failure_legacy(
    connection: &Connection,
) -> Result<Vec<FailureLegacyRow>, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT identity_id FROM stream_failure_payload_legacy ORDER BY identity_id",
            (),
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let mut legacy = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    {
        legacy.push(FailureLegacyRow {
            identity_id: row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?,
        });
    }
    Ok(legacy)
}

async fn read_assertion_details(
    connection: &Connection,
) -> Result<Vec<AssertionDetailRow>, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT identity_id, code, assertion_ref, owner_ref, owner_kind, predicate_ref, source_file_ref, start_byte, end_byte, start_line, start_column, end_line, end_column, safe_witness FROM stream_failure_assertion_detail ORDER BY identity_id",
            (),
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let mut details = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    {
        details.push(AssertionDetailRow {
            identity_id: row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?,
            code: row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?,
            assertion_ref: row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?,
            owner_ref: row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?,
            owner_kind: row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?,
            predicate_ref: row.get(5).map_err(|_| RuntimeError::RecoveryInvalid)?,
            source_file_ref: row.get(6).map_err(|_| RuntimeError::RecoveryInvalid)?,
            start_byte: row.get(7).map_err(|_| RuntimeError::RecoveryInvalid)?,
            end_byte: row.get(8).map_err(|_| RuntimeError::RecoveryInvalid)?,
            start_line: row.get(9).map_err(|_| RuntimeError::RecoveryInvalid)?,
            start_column: row.get(10).map_err(|_| RuntimeError::RecoveryInvalid)?,
            end_line: row.get(11).map_err(|_| RuntimeError::RecoveryInvalid)?,
            end_column: row.get(12).map_err(|_| RuntimeError::RecoveryInvalid)?,
            safe_witness: row.get(13).map_err(|_| RuntimeError::RecoveryInvalid)?,
        });
    }
    Ok(details)
}

async fn read_provider_failures(
    connection: &Connection,
) -> Result<Vec<ProviderFailureRow>, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT key_id, checkpoint_version, committed_position, attempts, diagnostic_code, diagnostic_class FROM stream_provider_failure ORDER BY key_id",
            (),
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let mut failures = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    {
        failures.push(ProviderFailureRow {
            key_id: row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?,
            checkpoint_version: row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?,
            committed_position: row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?,
            attempts: row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?,
            diagnostic_code: row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?,
            diagnostic_class: row.get(5).map_err(|_| RuntimeError::RecoveryInvalid)?,
        });
    }
    Ok(failures)
}

/// Inserts provider effect receipts verbatim. A receipt references no other
/// carried row, so the order of insertion does not matter.
async fn insert_receipt_carry(
    connection: &Connection,
    receipts: &[ReceiptRow],
) -> Result<(), RuntimeError> {
    for row in receipts {
        connection
            .execute(
                INSERT_RECEIPT,
                params![
                    row.effect_identity.as_slice(),
                    row.request_digest.as_slice(),
                    row.idempotency_key.as_slice(),
                    row.lease_fence,
                    row.attempts_in_fence,
                    row.total_attempts,
                    row.status,
                    row.receipt.as_deref()
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
    }
    Ok(())
}

/// Inserts failure rows parent-first: a failure precedes its companion rows.
async fn insert_failure_carry(
    connection: &Connection,
    carry: &FailureCarry,
) -> Result<(), RuntimeError> {
    for row in &carry.failures {
        connection
            .execute(
                INSERT_FAILURE,
                params![
                    row.identity_id.as_str(),
                    row.key_id.as_str(),
                    row.consumer_principal.as_str(),
                    row.consumer_root.as_str(),
                    row.consumer_function.as_str(),
                    row.consumer_binding.as_str(),
                    row.source_format.as_str(),
                    row.source.as_str(),
                    row.partition_format.as_str(),
                    row.partition.as_deref(),
                    row.position_format.as_str(),
                    row.delivery_position.as_str(),
                    row.successor_position.as_str(),
                    row.version,
                    row.attempts,
                    row.status,
                    row.diagnostic_code,
                    row.diagnostic_class
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
    }
    for row in &carry.payloads {
        connection
            .execute(
                INSERT_FAILURE_PAYLOAD,
                params![
                    row.identity_id.as_str(),
                    row.payload.as_deref(),
                    row.payload_reference.as_deref(),
                    row.payload_digest.as_deref(),
                    row.retention
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
    }
    for row in &carry.legacy {
        connection
            .execute(INSERT_FAILURE_LEGACY, params![row.identity_id.as_str()])
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
    }
    for row in &carry.assertion_details {
        connection
            .execute(
                INSERT_ASSERTION_DETAIL,
                params![
                    row.identity_id.as_str(),
                    row.code.as_str(),
                    row.assertion_ref.as_slice(),
                    row.owner_ref.as_slice(),
                    row.owner_kind,
                    row.predicate_ref.as_slice(),
                    row.source_file_ref.as_slice(),
                    row.start_byte.as_slice(),
                    row.end_byte.as_slice(),
                    row.start_line.as_slice(),
                    row.start_column.as_slice(),
                    row.end_line.as_slice(),
                    row.end_column.as_slice(),
                    row.safe_witness.as_deref()
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
    }
    for row in &carry.provider_failures {
        connection
            .execute(
                INSERT_PROVIDER_FAILURE,
                params![
                    row.key_id.as_str(),
                    row.checkpoint_version,
                    row.committed_position.as_deref(),
                    row.attempts,
                    row.diagnostic_code,
                    row.diagnostic_class
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
    }
    Ok(())
}
