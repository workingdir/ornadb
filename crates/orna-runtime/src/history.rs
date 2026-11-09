//! Historical runtime generations: selection, attestation and generation-pinned reads.

use crate::RuntimeState;
use crate::checkpoint_snapshot::StreamCheckpointWatermark;
use crate::codec::{decode_u64, fixed};
use crate::error::RuntimeError;
use crate::recovery::validate_historical_snapshot_tx;
use crate::schema::runtime_table_history_floor;
use crate::stream_admin::{
    AdminInvocationAudit, CheckpointResetAudit, load_admin_invocation_audits_through,
    load_stream_checkpoint_reset_audits_through,
};
use crate::stream_store::load_stream_checkpoint_history;
use crate::table_store::{
    RuntimeTableIdentity, TableObjectId, table_storage_key, validate_table_name,
};
use libsql::params;
use num_bigint::{BigInt, Sign};
use orna_foundation::{CanonicalSnapshot, CwdCapture, Snapshot};
use orna_stream::CheckpointKey;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

/// A read-only pin to one runtime checkpoint generation.
///
/// The mutation sequence is retained privately because generation numbers
/// alone do not identify the row state when querying the append-only history.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoricalSnapshot {
    capture: CwdCapture,
    pub(crate) generation: u64,
    pub(crate) mutation_sequence: u64,
    pub(crate) admin_audit_sequence: u64,
    pub(crate) checkpoint_reset_audit_sequence: u64,
}

impl HistoricalSnapshot {
    /// The exact database/runtime/generation identity selected by this pin.
    pub fn capture(&self) -> &CwdCapture {
        &self.capture
    }

    /// The checkpoint generation represented by this immutable pin.
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Canonical logical ID for this database/runtime/generation pin.
    ///
    /// The checkpoint digest remains separate in [`CwdCapture`], so equal
    /// payload digests at adjacent generations still have distinct IDs.
    pub fn snapshot_id(&self) -> [u8; 32] {
        historical_snapshot_id(&self.capture)
    }

    /// The immutable checkpoint mutation boundary represented by this pin.
    pub const fn mutation_sequence(&self) -> u64 {
        self.mutation_sequence
    }

    /// Last generic administrative audit row included by this checkpoint.
    pub const fn admin_audit_sequence(&self) -> u64 {
        self.admin_audit_sequence
    }

    /// Last specialized checkpoint-reset audit row included by this checkpoint.
    pub const fn checkpoint_reset_audit_sequence(&self) -> u64 {
        self.checkpoint_reset_audit_sequence
    }

    /// Whether two values can safely participate in one snapshot-scoped
    /// operation. Runtime snapshots at different generations are distinct
    /// contexts even when they belong to the same database.
    pub fn require_same_context(&self, other: &Self) -> Result<(), RuntimeError> {
        if self.capture == other.capture {
            Ok(())
        } else {
            Err(RuntimeError::SnapshotContextMismatch)
        }
    }
}

/// Context-bearing result of a governance audit read at one checkpoint pin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoricalAuditRows<T> {
    capture: CwdCapture,
    rows: Vec<T>,
}

impl<T> HistoricalAuditRows<T> {
    pub fn capture(&self) -> &CwdCapture {
        &self.capture
    }

    /// Canonical logical ID of the generation used for this audit read.
    pub fn snapshot_id(&self) -> [u8; 32] {
        historical_snapshot_id(&self.capture)
    }

    pub fn rows(&self) -> &[T] {
        &self.rows
    }

    /// Prevents audit rows observed at different checkpoint pins from being
    /// combined as one historical result.
    pub fn require_same_context<U>(
        &self,
        other: &HistoricalAuditRows<U>,
    ) -> Result<(), RuntimeError> {
        if self.capture == other.capture {
            Ok(())
        } else {
            Err(RuntimeError::SnapshotContextMismatch)
        }
    }
}

/// Integrity summary for the logical rows available at a retained checkpoint.
/// It is a local integrity statement, not a signature or external trust proof.
/// Raw-value hashes are deliberately withheld: this generic layer cannot
/// identify low-entropy secrets, whose hashes could disclose their values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoricalSnapshotAttestation {
    capture: CwdCapture,
    generation: u64,
    mutation_sequence: u64,
    row_count: u64,
    table_count: u64,
    admin_audit_sequence: u64,
    checkpoint_reset_audit_sequence: u64,
}

impl HistoricalSnapshotAttestation {
    pub fn capture(&self) -> &CwdCapture {
        &self.capture
    }

    /// The checkpoint generation whose retained rows were verified.
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Canonical logical ID of the generation whose rows were verified.
    pub fn snapshot_id(&self) -> [u8; 32] {
        historical_snapshot_id(&self.capture)
    }

    pub const fn mutation_sequence(&self) -> u64 {
        self.mutation_sequence
    }

    pub const fn row_count(&self) -> u64 {
        self.row_count
    }

    pub const fn table_count(&self) -> u64 {
        self.table_count
    }

    pub const fn admin_audit_sequence(&self) -> u64 {
        self.admin_audit_sequence
    }

    pub const fn checkpoint_reset_audit_sequence(&self) -> u64 {
        self.checkpoint_reset_audit_sequence
    }
}

fn historical_snapshot_id(capture: &CwdCapture) -> [u8; 32] {
    // The canonical Snapshot ID hashes database, runtime and generation. Keep
    // the mutable-data digest out of this stable logical key; CwdCapture still
    // carries that digest for exact context validation.
    match capture.snapshot() {
        Snapshot::Cwd { id, .. } => id,
        Snapshot::Commit { .. } => unreachable!("CwdCapture only contains CWD snapshots"),
    }
}

/// The result of reading one table at a pinned historical runtime generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoricalTableRows {
    capture: CwdCapture,
    table: String,
    rows: Vec<(Vec<u8>, Vec<u8>)>,
}

impl HistoricalTableRows {
    pub fn capture(&self) -> &CwdCapture {
        &self.capture
    }

    /// Canonical logical ID of the generation used for this table read.
    pub fn snapshot_id(&self) -> [u8; 32] {
        historical_snapshot_id(&self.capture)
    }

    /// Whether rows read under these pins can be combined as one historical
    /// value context.
    pub fn require_same_context(&self, other: &Self) -> Result<(), RuntimeError> {
        if self.capture == other.capture {
            Ok(())
        } else {
            Err(RuntimeError::SnapshotContextMismatch)
        }
    }

    pub fn table(&self) -> &str {
        &self.table
    }

    pub fn rows(&self) -> &[(Vec<u8>, Vec<u8>)] {
        &self.rows
    }
}

/// Several admitted relations read from one published `HEAD` pin.
///
/// Every relation carries the same capture, so a caller joining them cannot
/// combine rows from two generations. This mirrors
/// [`HistoricalTableRows::require_same_context`] per pair without needing the
/// caller to re-check each one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmittedHeadRelations {
    snapshot: HistoricalSnapshot,
    rows: Vec<HistoricalTableRows>,
}

impl AdmittedHeadRelations {
    /// The published pin every relation in this result was read from.
    pub fn snapshot(&self) -> &HistoricalSnapshot {
        &self.snapshot
    }

    /// The admitted relation reads, in the order they were requested.
    pub fn rows(&self) -> &[HistoricalTableRows] {
        &self.rows
    }

    /// Returns the rows for the admitted relation named `table`, if requested.
    pub fn relation(&self, table: &str) -> Option<&HistoricalTableRows> {
        self.rows.iter().find(|rows| rows.table() == table)
    }
}

impl RuntimeState {
    /// Selects one retained runtime checkpoint generation without changing the
    /// live CWD or writer state. A generation is a data pin only: this runtime
    /// layer does not select historical function code or silently substitute
    /// current semantic assets when a caller evaluates a historical program.
    pub async fn select_historical_snapshot(
        &self,
        generation: u64,
    ) -> Result<HistoricalSnapshot, RuntimeError> {
        let transaction = self.connection.transaction().await?;
        let mut rows = transaction
            .query(
                "SELECT database_id, runtime_id, generation FROM runtime_meta
                 WHERE singleton = 1",
                (),
            )
            .await?;
        let meta = rows.next().await?.ok_or(RuntimeError::RecoveryInvalid)?;
        let database_id = fixed(meta.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        let runtime_id = fixed(meta.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        let current_generation = decode_u64(
            meta.get::<i64>(2)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?;
        if generation > current_generation {
            return Err(RuntimeError::SnapshotNotFound);
        }

        let (floor_generation, floor_digest) = runtime_table_history_floor(&transaction).await?;
        if generation < floor_generation {
            return Err(RuntimeError::SnapshotIncomplete);
        }
        let (digest, mutation_sequence) = if generation == 0 {
            if floor_generation != 0 {
                return Err(RuntimeError::SnapshotIncomplete);
            }
            (floor_digest, 0)
        } else {
            let mut rows = transaction
                .query(
                    "SELECT digest, mutation_sequence FROM checkpoint WHERE generation = ?1",
                    params![i64::try_from(generation).map_err(|_| RuntimeError::SnapshotNotFound)?],
                )
                .await?;
            let Some(row) = rows.next().await? else {
                return Err(RuntimeError::SnapshotNotFound);
            };
            (
                fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
                decode_u64(
                    row.get::<i64>(1)
                        .map_err(|_| RuntimeError::RecoveryInvalid)?,
                )?,
            )
        };
        let mut boundary_rows = transaction
            .query(
                "SELECT admin_audit_sequence, checkpoint_reset_audit_sequence
                 FROM runtime_history_governance_boundary WHERE generation = ?1",
                params![i64::try_from(generation).map_err(|_| RuntimeError::SnapshotNotFound)?],
            )
            .await?;
        let boundary = boundary_rows
            .next()
            .await?
            .ok_or(RuntimeError::SnapshotIncomplete)?;
        let admin_audit_sequence = decode_u64(
            boundary
                .get::<i64>(0)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?;
        let checkpoint_reset_audit_sequence = decode_u64(
            boundary
                .get::<i64>(1)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?;
        let capture = CwdCapture::new(
            Snapshot::cwd(database_id, runtime_id, BigInt::from(generation))
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
            digest,
        )
        .map_err(|_| RuntimeError::RecoveryInvalid)?;
        transaction.commit().await?;
        Ok(HistoricalSnapshot {
            capture,
            generation,
            mutation_sequence,
            admin_audit_sequence,
            checkpoint_reset_audit_sequence,
        })
    }

    /// Selects the published `HEAD` pin for admitted relation reads.
    ///
    /// `HEAD` is the highest checkpoint generation whose publication completed
    /// with a Git commit (`ORNA-STATE-001`). A frozen intent that never
    /// completed, and every unpublished CWD generation after the last
    /// completed publication, are excluded, so an admitted read of
    /// `relation.as_of(HEAD)` never observes the unpublished tail
    /// (`ORNA-STATE-003`).
    ///
    /// With no completed publication this fails closed rather than resolving
    /// an empty baseline: an unpublished store has no `HEAD` to read.
    pub async fn select_published_snapshot(&self) -> Result<HistoricalSnapshot, RuntimeError> {
        let mut rows = self
            .connection
            .query(
                "SELECT MAX(publication_freeze.checkpoint_generation) \
                 FROM publication_freeze JOIN publication_commit \
                 ON publication_commit.intent_id = publication_freeze.intent_id",
                (),
            )
            .await?;
        let row = rows.next().await?.ok_or(RuntimeError::RecoveryInvalid)?;
        let published: Option<i64> = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let generation = u64::try_from(published.ok_or(RuntimeError::SnapshotNotFound)?)
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
        self.select_historical_snapshot(generation).await
    }

    /// Reads one admitted relation at published `HEAD` by its committed
    /// identity, so the rows match the live relation across a rename while the
    /// unpublished tail stays invisible (`ORNA-STATE-003`).
    pub async fn read_admitted_table_at_head(
        &self,
        identity: &RuntimeTableIdentity,
    ) -> Result<HistoricalTableRows, RuntimeError> {
        let snapshot = self.select_published_snapshot().await?;
        self.read_admitted_table_at(&snapshot, identity).await
    }

    /// Reads several admitted relations from one published `HEAD` pin.
    ///
    /// Every returned relation shares the same immutable checkpoint pin, so an
    /// expression joining two admitted tables cannot combine rows from two
    /// generations even if a publication completes while it reads
    /// (`ORNA-STATE-003`, `ORNA-STATE-004A`). Duplicate names or duplicate
    /// committed identities fail closed rather than aliasing one relation.
    pub async fn read_admitted_tables_at_head(
        &self,
        identities: &[RuntimeTableIdentity],
    ) -> Result<AdmittedHeadRelations, RuntimeError> {
        let mut names = BTreeSet::new();
        let mut object_ids = BTreeSet::new();
        for identity in identities {
            if !names.insert(identity.table().to_owned())
                || !object_ids.insert(identity.object_id())
            {
                return Err(RuntimeError::InvalidTableMutation);
            }
        }
        let snapshot = self.select_published_snapshot().await?;
        let mut rows = Vec::with_capacity(identities.len());
        for identity in identities {
            rows.push(self.read_admitted_table_at(&snapshot, identity).await?);
        }
        Ok(AdmittedHeadRelations { snapshot, rows })
    }

    /// Resolves one already-pinned CWD snapshot descriptor to its retained
    /// generation. Resolution validates the canonical ID and never substitutes
    /// the current generation for an unknown, expired, or mismatched pin.
    /// Committed snapshots are resolved by the repository layer, which owns
    /// their Git object lifetime rather than this runtime history store.
    pub async fn resolve_historical_snapshot(
        &self,
        selected: &CanonicalSnapshot,
    ) -> Result<HistoricalSnapshot, RuntimeError> {
        let Snapshot::Cwd {
            database,
            runtime,
            generation,
            id,
        } = selected
        else {
            return Err(RuntimeError::SnapshotNotFound);
        };

        let canonical = Snapshot::cwd(*database, *runtime, generation.clone())
            .map_err(|_| RuntimeError::SnapshotContextMismatch)?;
        if &canonical != selected {
            return Err(RuntimeError::SnapshotContextMismatch);
        }

        let current = self.capture().await?;
        if *database != current.database_id() || *runtime != current.runtime_id() {
            return Err(RuntimeError::SnapshotContextMismatch);
        }

        let (sign, digits) = generation.to_u64_digits();
        if sign == Sign::Minus {
            return Err(RuntimeError::SnapshotContextMismatch);
        }
        let generation = match digits.as_slice() {
            [] => 0,
            [generation] => *generation,
            // The runtime history API stores and selects u64 generations. A
            // wider, otherwise canonical descriptor cannot name a retained
            // checkpoint here, so fail closed instead of narrowing it.
            _ => return Err(RuntimeError::SnapshotNotFound),
        };
        let resolved = self.select_historical_snapshot(generation).await?;
        if resolved.capture.snapshot() != canonical || resolved.snapshot_id() != *id {
            return Err(RuntimeError::SnapshotContextMismatch);
        }
        Ok(resolved)
    }

    /// Reads one table as it existed at an immutable runtime checkpoint pin.
    /// Row keys and values remain in their canonical stored byte encodings;
    /// callers must decode them using schema metadata from this same snapshot.
    pub async fn read_table_at(
        &self,
        snapshot: &HistoricalSnapshot,
        table: &str,
    ) -> Result<HistoricalTableRows, RuntimeError> {
        self.read_storage_table_at(snapshot, table, None).await
    }

    /// Reads an admitted relation at an immutable checkpoint pin by its
    /// committed identity, so the rows match the live relation after a rename.
    pub async fn read_admitted_table_at(
        &self,
        snapshot: &HistoricalSnapshot,
        identity: &RuntimeTableIdentity,
    ) -> Result<HistoricalTableRows, RuntimeError> {
        self.read_storage_table_at(snapshot, identity.table(), Some(identity.object_id()))
            .await
    }

    async fn read_storage_table_at(
        &self,
        snapshot: &HistoricalSnapshot,
        table: &str,
        object_id: Option<TableObjectId>,
    ) -> Result<HistoricalTableRows, RuntimeError> {
        validate_table_name(table)?;
        let storage = table_storage_key(table, object_id);
        let transaction = self.connection.transaction().await?;
        let mut rows = transaction
            .query(
                "SELECT database_id, runtime_id, generation FROM runtime_meta
                 WHERE singleton = 1",
                (),
            )
            .await?;
        let meta = rows.next().await?.ok_or(RuntimeError::RecoveryInvalid)?;
        let database_id = fixed(meta.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        let runtime_id = fixed(meta.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        let current_generation = decode_u64(
            meta.get::<i64>(2)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?;
        let capture = snapshot.capture();
        let generation = capture
            .generation()
            .to_u64_digits()
            .1
            .first()
            .copied()
            .unwrap_or(0);
        if capture.database_id() != database_id
            || capture.runtime_id() != runtime_id
            || generation > current_generation
        {
            return Err(RuntimeError::SnapshotContextMismatch);
        }
        let (floor_generation, floor_digest) = runtime_table_history_floor(&transaction).await?;
        if generation < floor_generation {
            return Err(RuntimeError::SnapshotIncomplete);
        }
        let (digest, mutation_sequence) = if generation == 0 {
            if floor_generation != 0 {
                return Err(RuntimeError::SnapshotIncomplete);
            }
            (floor_digest, 0)
        } else {
            let mut rows = transaction
                .query(
                    "SELECT digest, mutation_sequence FROM checkpoint WHERE generation = ?1",
                    params![i64::try_from(generation).map_err(|_| RuntimeError::SnapshotNotFound)?],
                )
                .await?;
            let Some(row) = rows.next().await? else {
                return Err(RuntimeError::SnapshotNotFound);
            };
            (
                fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
                decode_u64(
                    row.get::<i64>(1)
                        .map_err(|_| RuntimeError::RecoveryInvalid)?,
                )?,
            )
        };
        if digest != capture.generation_digest() || mutation_sequence != snapshot.mutation_sequence
        {
            return Err(RuntimeError::SnapshotContextMismatch);
        }

        let mut query = transaction
            .query(
                "SELECT history.row_key, history.row_value, history.row_digest
                 FROM runtime_table_history AS history
                 WHERE history.table_id = ?1
                   AND history.mutation_sequence <= ?2
                   AND history.deleted = 0
                   AND NOT EXISTS (
                       SELECT 1 FROM runtime_table_history AS newer
                       WHERE newer.table_id = history.table_id
                         AND newer.row_key = history.row_key
                         AND newer.mutation_sequence <= ?2
                         AND newer.mutation_sequence > history.mutation_sequence
                   )
                 ORDER BY history.row_key",
                params![
                    storage,
                    i64::try_from(mutation_sequence).map_err(|_| RuntimeError::RecoveryInvalid)?
                ],
            )
            .await?;
        let mut table_rows = Vec::new();
        while let Some(row) = query.next().await? {
            let key: Vec<u8> = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let value: Vec<u8> = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let row_digest: [u8; 32] =
                fixed(row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
            if <[u8; 32]>::from(Sha256::digest(&value)) != row_digest {
                return Err(RuntimeError::SnapshotIncomplete);
            }
            table_rows.push((key, value));
        }
        transaction.commit().await?;
        Ok(HistoricalTableRows {
            capture: capture.clone(),
            table: table.to_owned(),
            rows: table_rows,
        })
    }

    /// Reads administrative invocation history through the immutable audit
    /// sequence captured by this checkpoint generation.
    pub async fn admin_invocation_audits_at(
        &self,
        snapshot: &HistoricalSnapshot,
    ) -> Result<HistoricalAuditRows<AdminInvocationAudit>, RuntimeError> {
        let transaction = self.connection.transaction().await?;
        validate_historical_snapshot_tx(&transaction, snapshot).await?;
        transaction.commit().await?;
        let rows =
            load_admin_invocation_audits_through(&self.connection, snapshot.admin_audit_sequence)
                .await?;
        Ok(HistoricalAuditRows {
            capture: snapshot.capture.clone(),
            rows,
        })
    }

    /// Reads stream checkpoint-reset governance history through the immutable
    /// audit sequence captured by this checkpoint generation. Legacy reset
    /// audits with no recorded generation remain available only in the
    /// unpinned audit read because their historical membership is unknown.
    pub async fn checkpoint_reset_audits_at(
        &self,
        key: &CheckpointKey,
        snapshot: &HistoricalSnapshot,
    ) -> Result<HistoricalAuditRows<CheckpointResetAudit>, RuntimeError> {
        let transaction = self.connection.transaction().await?;
        validate_historical_snapshot_tx(&transaction, snapshot).await?;
        transaction.commit().await?;
        let rows = load_stream_checkpoint_reset_audits_through(
            &self.connection,
            key,
            snapshot.checkpoint_reset_audit_sequence,
            false,
        )
        .await?;
        Ok(HistoricalAuditRows {
            capture: snapshot.capture.clone(),
            rows,
        })
    }

    /// Verifies that the pin is retained and that every visible row version
    /// has its recorded SHA-256, then returns verified row counts and the
    /// governance cut included in the pin. The summary attests runtime-retained
    /// bytes, not function or package code; it deliberately withholds hashes
    /// because this generic layer cannot distinguish secret row fields.
    pub async fn attest_historical_snapshot(
        &self,
        snapshot: &HistoricalSnapshot,
    ) -> Result<HistoricalSnapshotAttestation, RuntimeError> {
        let transaction = self.connection.transaction().await?;
        validate_historical_snapshot_tx(&transaction, snapshot).await?;
        let mut rows = transaction
            .query(
                "SELECT history.table_id, history.row_value, history.row_digest, history.deleted
                 FROM runtime_table_history AS history
                 WHERE history.mutation_sequence <= ?1
                   AND NOT EXISTS (
                       SELECT 1 FROM runtime_table_history AS newer
                       WHERE newer.table_id = history.table_id
                         AND newer.row_key = history.row_key
                         AND newer.mutation_sequence <= ?1
                         AND newer.mutation_sequence > history.mutation_sequence
                   )
                 ORDER BY history.table_id, history.row_key",
                params![
                    i64::try_from(snapshot.mutation_sequence)
                        .map_err(|_| RuntimeError::RecoveryInvalid)?
                ],
            )
            .await?;
        let mut row_count = 0_u64;
        let mut table_count = 0_u64;
        let mut previous_table: Option<String> = None;
        while let Some(row) = rows.next().await? {
            let table: String = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
            validate_table_name(&table)?;
            let value: Option<Vec<u8>> = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let digest: Option<Vec<u8>> = row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let deleted = row
                .get::<i64>(3)
                .map_err(|_| RuntimeError::RecoveryInvalid)?;
            let (value, digest) = match (deleted, value, digest) {
                (0, Some(value), Some(digest)) => (value, digest),
                (1, None, None) => {
                    // The latest tombstone is history bookkeeping, not part of
                    // the logical row image described by this attestation.
                    continue;
                }
                // Retained history is required to satisfy the same marker
                // contract as the storage schema. A contradictory tombstone
                // cannot be counted as either a live row or a complete delete.
                _ => return Err(RuntimeError::SnapshotIncomplete),
            };
            let digest: [u8; 32] = fixed(digest).map_err(|_| RuntimeError::SnapshotIncomplete)?;
            if <[u8; 32]>::from(Sha256::digest(&value)) != digest {
                return Err(RuntimeError::SnapshotIncomplete);
            }
            if previous_table.as_deref() != Some(&table) {
                table_count = table_count
                    .checked_add(1)
                    .ok_or(RuntimeError::RecoveryInvalid)?;
                previous_table = Some(table.clone());
            }
            row_count = row_count
                .checked_add(1)
                .ok_or(RuntimeError::RecoveryInvalid)?;
        }
        transaction.commit().await?;
        Ok(HistoricalSnapshotAttestation {
            capture: snapshot.capture.clone(),
            generation: snapshot.generation,
            mutation_sequence: snapshot.mutation_sequence,
            row_count,
            table_count,
            admin_audit_sequence: snapshot.admin_audit_sequence,
            checkpoint_reset_audit_sequence: snapshot.checkpoint_reset_audit_sequence,
        })
    }

    /// Reads terminal Complete/Skip watermarks retained through a selected
    /// CWD snapshot. Rows are ordered by checkpoint version and remain bound
    /// to the supplied stream identity.
    pub async fn stream_checkpoint_history_at(
        &self,
        key: &CheckpointKey,
        selected: &CwdCapture,
    ) -> Result<Vec<StreamCheckpointWatermark>, RuntimeError> {
        load_stream_checkpoint_history(&self.connection, key, selected).await
    }
}
