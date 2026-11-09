//! Reopen-time validation of the durable runtime state.

use crate::codec::{
    bigint_to_i64, decode_bool, decode_capture, decode_position, decode_u32, decode_u64, fixed,
    validate_digest, validate_id, validate_observation_text,
};
use crate::error::RuntimeError;
use crate::history::HistoricalSnapshot;
use crate::publication::PublicationCommitId;
use crate::request::decode_request_status;
use crate::schema::{load_stream_checkpoint_migration_origin, runtime_table_history_floor};
use crate::stream_store::{
    StoredFailurePayload, decode_class, decode_code, decode_stream_status,
    validate_recoverable_stream_attempts,
};
use crate::table_store::{
    AcceptedContentPin, AcceptedContentPinManifestEntry, accepted_content_pin_manifest_digest,
};
use crate::{RuntimeState, decode_request_execution_evidence, validate_request_execution_evidence};
use libsql::{Connection, Transaction, params};
use orna_foundation::CwdCapture;
use orna_repository::{CompactRuntimeReceipt, GitHashAlgorithm, NativeOid};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) async fn validate_historical_snapshot_tx(
    transaction: &Transaction,
    snapshot: &HistoricalSnapshot,
) -> Result<(), RuntimeError> {
    let mut meta = transaction
        .query(
            "SELECT database_id, runtime_id, generation
             FROM runtime_meta WHERE singleton = 1",
            (),
        )
        .await?;
    let row = meta.next().await?.ok_or(RuntimeError::RecoveryInvalid)?;
    let database_id = fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    let runtime_id = fixed(row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    let current_generation = decode_u64(
        row.get::<i64>(2)
            .map_err(|_| RuntimeError::RecoveryInvalid)?,
    )?;
    let capture = snapshot.capture();
    let generation = u64::try_from(bigint_to_i64(capture.generation())?)
        .map_err(|_| RuntimeError::SnapshotContextMismatch)?;
    if capture.database_id() != database_id
        || capture.runtime_id() != runtime_id
        || generation != snapshot.generation
        || generation > current_generation
    {
        return Err(RuntimeError::SnapshotContextMismatch);
    }
    let (floor_generation, floor_digest) = runtime_table_history_floor(transaction).await?;
    if generation < floor_generation {
        return Err(RuntimeError::SnapshotIncomplete);
    }
    let (digest, mutation_sequence) = if generation == 0 {
        if floor_generation != 0 {
            return Err(RuntimeError::SnapshotIncomplete);
        }
        (floor_digest, 0)
    } else {
        let mut checkpoint = transaction
            .query(
                "SELECT digest, mutation_sequence FROM checkpoint WHERE generation = ?1",
                params![i64::try_from(generation).map_err(|_| RuntimeError::SnapshotNotFound)?],
            )
            .await?;
        let row = checkpoint
            .next()
            .await?
            .ok_or(RuntimeError::SnapshotNotFound)?;
        (
            fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
            decode_u64(
                row.get::<i64>(1)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?,
        )
    };
    if digest != capture.generation_digest() || mutation_sequence != snapshot.mutation_sequence {
        return Err(RuntimeError::SnapshotContextMismatch);
    }
    let mut boundary = transaction
        .query(
            "SELECT admin_audit_sequence, checkpoint_reset_audit_sequence
             FROM runtime_history_governance_boundary WHERE generation = ?1",
            params![i64::try_from(generation).map_err(|_| RuntimeError::SnapshotNotFound)?],
        )
        .await?;
    let row = boundary
        .next()
        .await?
        .ok_or(RuntimeError::SnapshotIncomplete)?;
    let admin_audit_sequence = decode_u64(
        row.get::<i64>(0)
            .map_err(|_| RuntimeError::RecoveryInvalid)?,
    )?;
    let checkpoint_reset_audit_sequence = decode_u64(
        row.get::<i64>(1)
            .map_err(|_| RuntimeError::RecoveryInvalid)?,
    )?;
    if admin_audit_sequence != snapshot.admin_audit_sequence
        || checkpoint_reset_audit_sequence != snapshot.checkpoint_reset_audit_sequence
    {
        return Err(RuntimeError::SnapshotContextMismatch);
    }
    Ok(())
}

/// Applies retained reset audits as explicit version transitions when
/// validating stream history. A reset is governance history, not a delivery
/// Complete/Skip watermark, but it still anchors the checkpoint version chain.
async fn validate_stream_checkpoint_reset_bridge(
    connection: &Connection,
    key_id: &str,
    start_version: u64,
    start_position: Option<&str>,
    end_version: u64,
) -> Result<Option<String>, RuntimeError> {
    if end_version < start_version {
        return Err(RuntimeError::RecoveryInvalid);
    }
    let mut version = start_version;
    let mut position = start_position.map(str::to_owned);
    if let Some(position) = position.as_deref() {
        decode_position(position.to_owned())?;
    }
    let mut rows = connection
        .query(
            "SELECT old_version, old_position, new_version, new_position,
                    reason, redacted, observed_generation
             FROM stream_checkpoint_reset_audit
             WHERE key_id = ?1 AND new_version > ?2 AND new_version <= ?3
             ORDER BY new_version, sequence",
            params![
                key_id,
                i64::try_from(start_version).map_err(|_| RuntimeError::RecoveryInvalid)?,
                i64::try_from(end_version).map_err(|_| RuntimeError::RecoveryInvalid)?
            ],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        let old_version = decode_u64(
            row.get::<i64>(0)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?;
        let old_position = row
            .get::<Option<String>>(1)
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
        if let Some(old_position) = &old_position {
            decode_position(old_position.clone())?;
        }
        let new_version = decode_u64(
            row.get::<i64>(2)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?;
        let new_position: String = row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?;
        decode_position(new_position.clone())?;
        let reason: String = row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?;
        validate_observation_text(&reason)?;
        decode_bool(
            row.get::<i64>(5)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?;
        let observed_generation: i64 = row.get(6).map_err(|_| RuntimeError::RecoveryInvalid)?;
        if observed_generation < -1
            || old_version != version
            || old_position != position
            || new_version != version.saturating_add(1)
        {
            return Err(RuntimeError::RecoveryInvalid);
        }
        version = new_version;
        position = Some(new_position);
    }
    if version != end_version {
        return Err(RuntimeError::RecoveryInvalid);
    }
    Ok(position)
}

impl RuntimeState {
    pub async fn validate_recovery(&self) -> Result<(), RuntimeError> {
        self.validate_recovery_with_pin_manifest().await.map(|_| ())
    }

    pub(crate) async fn validate_recovery_with_pin_manifest(
        &self,
    ) -> Result<BTreeMap<[u8; 16], BTreeMap<[u8; 16], NativeOid>>, RuntimeError> {
        let capture = self.capture().await?;
        let generation = capture
            .generation()
            .to_u64_digits()
            .1
            .first()
            .copied()
            .unwrap_or(0);
        let mut rows = self
            .connection
            .query(
                "SELECT COUNT(*), COALESCE(MIN(generation), 0), COALESCE(MAX(generation), 0) FROM checkpoint",
                (),
            )
            .await
            ?;
        let row = rows.next().await?.ok_or(RuntimeError::RecoveryInvalid)?;
        let count: i64 = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let minimum: i64 = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let maximum: i64 = row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let checkpoint_count = u64::try_from(count).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let checkpoint_minimum =
            u64::try_from(minimum).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let checkpoint_maximum =
            u64::try_from(maximum).map_err(|_| RuntimeError::RecoveryInvalid)?;
        if (generation == 0 && checkpoint_count != 0)
            || (generation > 0
                && (checkpoint_count != generation
                    || checkpoint_minimum != 1
                    || checkpoint_maximum != generation))
        {
            return Err(RuntimeError::RecoveryInvalid);
        }
        match (generation, self.latest_checkpoint().await?) {
            (0, None) => {}
            (value, Some(checkpoint))
                if checkpoint.generation == value
                    && checkpoint.digest == capture.generation_digest() => {}
            _ => return Err(RuntimeError::RecoveryInvalid),
        }
        self.validate_checkpoint_anchors().await?;
        let accepted_pending = self
            .validate_accepted_content_pin_recovery(generation)
            .await?;
        self.validate_stream_checkpoint_history(&capture, generation)
            .await?;
        self.validate_stream_controls().await?;
        self.validate_stream_provider_failures().await?;
        self.validate_stream_failure_payloads().await?;
        validate_recoverable_stream_attempts(&self.connection).await?;
        self.validate_session_deletions().await?;
        self.validate_request_admissions(generation).await?;
        let mut request_rows = self
            .connection
            .query(
                "SELECT session_id, request_id, fingerprint, state, terminal_outcome,
                        owner_id, owner_epoch, effect_evidence, recovery_disposition,
                        controlled_transaction_proof, controlled_rollback_proof
                 FROM request_ledger",
                (),
            )
            .await?;
        while let Some(row) = request_rows.next().await? {
            let status = decode_request_status(&row).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let evidence = decode_request_execution_evidence(&row, 5)?;
            validate_request_execution_evidence(&status, evidence)?;
        }
        Ok(accepted_pending)
    }

    /// Lists the accepted content pins this runtime's graph repository holds
    /// for `descriptor_oid`, ordered by pin id. A listed pin is only a
    /// candidate: the caller must admit it through the graph, which checks Git.
    /// A runtime without a graph repository identity holds no candidates.
    pub async fn accepted_content_pins_for_descriptor(
        &self,
        descriptor_oid: &NativeOid,
    ) -> Result<Vec<AcceptedContentPin>, RuntimeError> {
        let Some(repository_id) = self.graph_repository_id else {
            return Ok(Vec::new());
        };
        let algorithm = match descriptor_oid.algorithm() {
            GitHashAlgorithm::Sha1 => 1_i64,
            GitHashAlgorithm::Sha256 => 2_i64,
        };
        let mut rows = self
            .connection
            .query(
                "SELECT pin.pin_id, pin.content_length, pin.content_sha256
                 FROM accepted_content_pin AS pin
                 JOIN runtime_meta AS runtime
                   ON runtime.singleton = 1 AND runtime.database_id = pin.database_id
                 WHERE pin.repository_id = ?1
                   AND pin.descriptor_oid_algorithm = ?2
                   AND pin.descriptor_oid = ?3
                 ORDER BY pin.pin_id",
                params![&repository_id[..], algorithm, descriptor_oid.as_bytes()],
            )
            .await?;
        let mut pins = Vec::new();
        while let Some(row) = rows.next().await? {
            let pin_id: [u8; 16] = fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
            let content_length: i64 = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let content_length =
                u64::try_from(content_length).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let content_sha256: [u8; 32] =
                fixed(row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
            pins.push(AcceptedContentPin {
                pin_id,
                content_length,
                content_sha256,
            });
        }
        Ok(pins)
    }

    async fn validate_accepted_content_pin_recovery(
        &self,
        current_generation: u64,
    ) -> Result<BTreeMap<[u8; 16], BTreeMap<[u8; 16], NativeOid>>, RuntimeError> {
        let mut accepted_pending = BTreeMap::<[u8; 16], BTreeMap<[u8; 16], NativeOid>>::new();
        let mut accepted_pin_ids = BTreeSet::new();
        let mut entries_by_generation: BTreeMap<i64, Vec<AcceptedContentPinManifestEntry>> =
            BTreeMap::new();
        let mut rows = self
            .connection
            .query(
                "SELECT pin.pin_id, pin.repository_id, pin.database_id,
                        pin.descriptor_oid_algorithm, pin.descriptor_oid,
                        pin.content_length, pin.content_sha256, pin.owner_id,
                        pin.owner_epoch, pin.checkpoint_generation,
                        checkpoint.generation, runtime.database_id
                 FROM accepted_content_pin AS pin
                 LEFT JOIN checkpoint
                   ON checkpoint.generation = pin.checkpoint_generation
                 JOIN runtime_meta AS runtime ON runtime.singleton = 1",
                (),
            )
            .await?;
        while let Some(row) = rows.next().await? {
            let pin_id: [u8; 16] = fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)
                .map_err(|_| RuntimeError::RecoveryInvalid)?;
            let repository_id: [u8; 32] =
                fixed(row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?;
            let database_id: [u8; 16] =
                fixed(row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?;
            let descriptor_oid_algorithm: i64 =
                row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let descriptor_oid: Vec<u8> = row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let expected_oid_length = match descriptor_oid_algorithm {
                1 => 20,
                2 => 32,
                _ => return Err(RuntimeError::RecoveryInvalid),
            };
            let content_length: i64 = row.get(5).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let content_sha256: [u8; 32] =
                fixed(row.get(6).map_err(|_| RuntimeError::RecoveryInvalid)?)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?;
            let owner_id: [u8; 16] = fixed(row.get(7).map_err(|_| RuntimeError::RecoveryInvalid)?)
                .map_err(|_| RuntimeError::RecoveryInvalid)?;
            let owner_epoch: i64 = row.get(8).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let checkpoint_generation: Option<i64> =
                row.get(9).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let anchored_generation: Option<i64> =
                row.get(10).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let runtime_database_id: [u8; 16] =
                fixed(row.get(11).map_err(|_| RuntimeError::RecoveryInvalid)?)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?;
            validate_id(pin_id).map_err(|_| RuntimeError::RecoveryInvalid)?;
            validate_id(owner_id).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let Some(checkpoint_generation) = checkpoint_generation else {
                return Err(RuntimeError::RecoveryInvalid);
            };
            if database_id != runtime_database_id
                || self.graph_repository_id != Some(repository_id)
                || content_length < 0
                || descriptor_oid.len() != expected_oid_length
                || owner_epoch <= 0
                || checkpoint_generation <= 0
                || u64::try_from(checkpoint_generation)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?
                    > current_generation
                || anchored_generation != Some(checkpoint_generation)
            {
                return Err(RuntimeError::RecoveryInvalid);
            }
            let algorithm = match descriptor_oid_algorithm {
                1 => GitHashAlgorithm::Sha1,
                2 => GitHashAlgorithm::Sha256,
                _ => return Err(RuntimeError::RecoveryInvalid),
            };
            let accepted_oid = NativeOid::new(algorithm, &descriptor_oid)
                .map_err(|_| RuntimeError::RecoveryInvalid)?;
            entries_by_generation
                .entry(checkpoint_generation)
                .or_default()
                .push(AcceptedContentPinManifestEntry {
                    pin_id,
                    repository_id,
                    database_id,
                    descriptor_oid_algorithm,
                    descriptor_oid,
                    content_length,
                    content_sha256,
                    owner_id,
                    owner_epoch,
                    checkpoint_generation,
                });
            if !accepted_pin_ids.insert(pin_id) {
                return Err(RuntimeError::RecoveryInvalid);
            }
            if accepted_pending
                .entry(owner_id)
                .or_default()
                .insert(pin_id, accepted_oid)
                .is_some()
            {
                return Err(RuntimeError::RecoveryInvalid);
            }
        }

        let mut checkpoints = self
            .connection
            .query(
                "SELECT generation, accepted_pin_manifest_digest FROM checkpoint ORDER BY generation",
                (),
            )
            .await
            ?;
        while let Some(row) = checkpoints.next().await? {
            let generation: i64 = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let stored_digest: [u8; 32] =
                fixed(row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?;
            let entries = entries_by_generation
                .remove(&generation)
                .unwrap_or_default();
            let expected_digest = if entries.is_empty() {
                [0; 32]
            } else {
                accepted_content_pin_manifest_digest(&entries)
            };
            if stored_digest != expected_digest {
                return Err(RuntimeError::RecoveryInvalid);
            }
        }
        if !entries_by_generation.is_empty() {
            return Err(RuntimeError::RecoveryInvalid);
        }
        Ok(accepted_pending)
    }

    /// A checkpoint is an anchor into the durable mutation ledger. Recovery
    /// must not accept a contiguous generation history whose anchors have
    /// been corrupted, removed, or reordered.
    async fn validate_checkpoint_anchors(&self) -> Result<(), RuntimeError> {
        let mut rows = self
            .connection
            .query(
                "SELECT checkpoint.generation, checkpoint.digest, checkpoint.mutation_sequence, pending_mutation.sequence, \
                        (SELECT publication_commit.intent_id FROM publication_freeze \
                         JOIN publication_commit ON publication_commit.intent_id = publication_freeze.intent_id \
                         WHERE publication_freeze.checkpoint_mutation_sequence >= checkpoint.mutation_sequence LIMIT 1) \
                 FROM checkpoint LEFT JOIN pending_mutation ON pending_mutation.sequence = checkpoint.mutation_sequence \
                 ORDER BY checkpoint.generation",
                (),
            )
            .await
            ?;
        let mut previous_sequence = 0;
        while let Some(row) = rows.next().await? {
            let _: u64 = u64::try_from(
                row.get::<i64>(0)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
            validate_digest(fixed(
                row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?)
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
            let sequence = u64::try_from(
                row.get::<i64>(2)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
            let referenced: Option<i64> = row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let completed: Option<Vec<u8>> =
                row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?;
            // Generations can advance without a new mutation, so repeated mutation
            // sequence numbers are valid. A committed publication consumes every
            // pending row through its frozen sequence, so an earlier checkpoint whose
            // pending row was consumed by a later committed publication is anchored.
            if sequence == 0
                || sequence < previous_sequence
                || (referenced.and_then(|value| u64::try_from(value).ok()) != Some(sequence)
                    && completed.is_none())
            {
                return Err(RuntimeError::RecoveryInvalid);
            }
            previous_sequence = sequence;
        }
        let mut freezes = self
            .connection
            .query(
                "SELECT publication_freeze.intent_id, publication_freeze.checkpoint_generation, \
                 publication_freeze.checkpoint_mutation_sequence, publication_freeze.checkpoint_digest, \
                 publication_freeze.frozen, checkpoint.generation, \
                 publication_freeze.compact_watermark, publication_freeze.compact_commit_id, \
                 publication_freeze.compact_journal_verifier \
                 FROM publication_freeze LEFT JOIN checkpoint ON checkpoint.generation = publication_freeze.checkpoint_generation \
                 AND checkpoint.mutation_sequence = publication_freeze.checkpoint_mutation_sequence \
                 AND checkpoint.digest = publication_freeze.checkpoint_digest",
                (),
            )
            .await
            ?;
        while let Some(row) = freezes.next().await? {
            validate_id(fixed(
                row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?)
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
            let generation = u64::try_from(
                row.get::<i64>(1)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
            let sequence = u64::try_from(
                row.get::<i64>(2)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
            validate_digest(fixed(
                row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?)
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
            let frozen: i64 = row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let referenced: Option<i64> = row.get(5).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let watermark: Option<Vec<u8>> =
                row.get(6).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let commit: Option<Vec<u8>> = row.get(7).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let verifier: Option<Vec<u8>> =
                row.get(8).map_err(|_| RuntimeError::RecoveryInvalid)?;
            if generation == 0 || sequence == 0 || frozen != 1 || referenced.is_none() {
                return Err(RuntimeError::RecoveryInvalid);
            }
            match (watermark, commit, verifier) {
                (None, None, None) => {}
                (Some(watermark), Some(commit), Some(verifier)) => {
                    if fixed::<32>(watermark)?
                        != fixed::<32>(row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?)?
                        || PublicationCommitId::new(commit).is_err()
                        || fixed::<32>(verifier).is_err()
                    {
                        return Err(RuntimeError::RecoveryInvalid);
                    }
                }
                _ => return Err(RuntimeError::RecoveryInvalid),
            }
        }
        let mut publications = self
            .connection
            .query(
                "SELECT publication_commit.intent_id, publication_commit.commit_id, publication_commit.compact_receipt, publication_freeze.intent_id, \
                 publication_freeze.compact_watermark, publication_freeze.compact_commit_id, \
                 publication_freeze.compact_journal_verifier, publication_freeze.checkpoint_digest \
                 FROM publication_commit LEFT JOIN publication_freeze \
                 ON publication_freeze.intent_id = publication_commit.intent_id",
                (),
            )
            .await
            ?;
        while let Some(row) = publications.next().await? {
            let intent_id = fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
            validate_id(intent_id).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let commit = PublicationCommitId::new(
                row.get::<Vec<u8>>(1)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
            let receipt: Option<Vec<u8>> = row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let watermark: Option<Vec<u8>> =
                row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let bound_commit: Option<Vec<u8>> =
                row.get(5).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let verifier: Option<Vec<u8>> =
                row.get(6).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let freeze_digest: [u8; 32] =
                fixed(row.get(7).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
            match (receipt, watermark, bound_commit, verifier) {
                (Some(receipt), Some(watermark), Some(bound_commit), Some(verifier)) => {
                    let receipt = CompactRuntimeReceipt::decode(&receipt)
                        .map_err(|_| RuntimeError::RecoveryInvalid)?;
                    if receipt.runtime_intent_id() != intent_id
                        || receipt.commit().as_str().as_bytes() != commit.as_bytes()
                        || receipt.cleanup_watermark() != fixed::<32>(watermark)?
                        || receipt.cleanup_watermark() != freeze_digest
                        || receipt.commit().as_str().as_bytes() != bound_commit
                        || receipt.journal_verifier() != fixed::<32>(verifier)?
                    {
                        return Err(RuntimeError::RecoveryInvalid);
                    }
                    self.verify_compact_receipt(&receipt)
                        .map_err(|_| RuntimeError::RecoveryInvalid)?;
                }
                (None, None, None, None) => {}
                _ => return Err(RuntimeError::RecoveryInvalid),
            }
            validate_id(fixed(
                row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?)
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
        }
        Ok(())
    }

    async fn validate_stream_controls(&self) -> Result<(), RuntimeError> {
        let mut rows = self
            .connection
            .query(
                "SELECT stream_control.key_id, stream_control.status,
                        stream_checkpoint.key_id
                 FROM stream_control
                 LEFT JOIN stream_checkpoint
                   ON stream_checkpoint.key_id = stream_control.key_id",
                (),
            )
            .await?;
        while let Some(row) = rows.next().await? {
            let key_id: String = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
            if key_id.is_empty() {
                return Err(RuntimeError::RecoveryInvalid);
            }
            let referenced: Option<String> =
                row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?;
            if referenced.as_deref() != Some(key_id.as_str()) {
                return Err(RuntimeError::RecoveryInvalid);
            }
            decode_stream_status(
                row.get::<i64>(1)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?;
        }
        let mut pending = self
            .connection
            .query(
                "SELECT pause.key_id, checkpoint.key_id
                 FROM stream_pause_pending AS pause
                 LEFT JOIN stream_checkpoint AS checkpoint
                   ON checkpoint.key_id = pause.key_id",
                (),
            )
            .await?;
        while let Some(row) = pending.next().await? {
            let key_id: String = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let referenced: Option<String> =
                row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
            if key_id.is_empty() || referenced.as_deref() != Some(key_id.as_str()) {
                return Err(RuntimeError::RecoveryInvalid);
            }
        }
        Ok(())
    }

    async fn validate_stream_checkpoint_history(
        &self,
        current_capture: &CwdCapture,
        current_generation: u64,
    ) -> Result<(), RuntimeError> {
        let mut rows = self
            .connection
            .query(
                "SELECT history.key_id, history.version, history.committed_position,
                        history.snapshot, history.generation_digest, history.transition,
                        checkpoint.key_id, checkpoint.version, checkpoint.committed_position
                 FROM stream_checkpoint_history AS history
                 LEFT JOIN stream_checkpoint AS checkpoint
                   ON checkpoint.key_id = history.key_id
                 ORDER BY history.key_id, history.version",
                (),
            )
            .await?;
        let mut active_key: Option<String> = None;
        let mut previous_version = 0_u64;
        let mut latest_position: Option<String> = None;
        let mut checkpoint_version = 0_u64;
        let mut checkpoint_position: Option<String> = None;
        while let Some(row) = rows.next().await? {
            let key_id: String = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
            if key_id.is_empty() {
                return Err(RuntimeError::RecoveryInvalid);
            }
            let version = decode_u64(
                row.get::<i64>(1)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?;
            if version == 0 {
                return Err(RuntimeError::RecoveryInvalid);
            }
            let committed: String = row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?;
            decode_position(committed.clone())?;
            let capture = decode_capture(
                row.get::<Vec<u8>>(3)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
                fixed(
                    row.get::<Vec<u8>>(4)
                        .map_err(|_| RuntimeError::RecoveryInvalid)?,
                )?,
            )?;
            if capture.database_id() != current_capture.database_id()
                || capture.runtime_id() != current_capture.runtime_id()
                || capture.generation() > current_capture.generation()
            {
                return Err(RuntimeError::RecoveryInvalid);
            }
            let history_generation = capture
                .generation()
                .to_u64_digits()
                .1
                .first()
                .copied()
                .unwrap_or(0);
            if history_generation > current_generation {
                return Err(RuntimeError::RecoveryInvalid);
            }
            if history_generation == current_generation
                && capture.generation_digest() != current_capture.generation_digest()
            {
                return Err(RuntimeError::RecoveryInvalid);
            }
            if history_generation > 0 {
                let mut checkpoints = self
                    .connection
                    .query(
                        "SELECT digest FROM checkpoint WHERE generation = ?1",
                        params![
                            i64::try_from(history_generation)
                                .map_err(|_| RuntimeError::RecoveryInvalid)?
                        ],
                    )
                    .await?;
                let checkpoint = checkpoints
                    .next()
                    .await?
                    .ok_or(RuntimeError::RecoveryInvalid)?;
                if fixed(
                    checkpoint
                        .get::<Vec<u8>>(0)
                        .map_err(|_| RuntimeError::RecoveryInvalid)?,
                )? != capture.generation_digest()
                {
                    return Err(RuntimeError::RecoveryInvalid);
                }
            }
            let transition = row
                .get::<i64>(5)
                .map_err(|_| RuntimeError::RecoveryInvalid)?;
            if !matches!(transition, 1 | 2) {
                return Err(RuntimeError::RecoveryInvalid);
            }
            let joined_key: Option<String> =
                row.get(6).map_err(|_| RuntimeError::RecoveryInvalid)?;
            if joined_key.as_deref() != Some(key_id.as_str()) {
                return Err(RuntimeError::RecoveryInvalid);
            }
            let joined_version = decode_u64(
                row.get::<i64>(7)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?;
            let joined_position: Option<String> =
                row.get(8).map_err(|_| RuntimeError::RecoveryInvalid)?;
            if joined_version == 0 || joined_position.is_none() {
                return Err(RuntimeError::RecoveryInvalid);
            }
            if active_key.as_deref() != Some(key_id.as_str()) {
                if let Some(previous_key) = active_key.as_deref() {
                    let final_position = validate_stream_checkpoint_reset_bridge(
                        &self.connection,
                        previous_key,
                        previous_version,
                        latest_position.as_deref(),
                        checkpoint_version,
                    )
                    .await?;
                    if final_position != checkpoint_position {
                        return Err(RuntimeError::RecoveryInvalid);
                    }
                }
                let (origin_version, origin_position) =
                    load_stream_checkpoint_migration_origin(&self.connection, &key_id).await?;
                active_key = Some(key_id);
                previous_version = origin_version;
                latest_position = origin_position;
                checkpoint_version = joined_version;
                checkpoint_position = joined_position;
            } else if joined_version != checkpoint_version || joined_position != checkpoint_position
            {
                return Err(RuntimeError::RecoveryInvalid);
            }
            if version > previous_version.saturating_add(1) {
                let reset_through = version - 1;
                let _reset_position = validate_stream_checkpoint_reset_bridge(
                    &self.connection,
                    active_key.as_deref().ok_or(RuntimeError::RecoveryInvalid)?,
                    previous_version,
                    latest_position.as_deref(),
                    reset_through,
                )
                .await?;
                previous_version = reset_through;
            }
            if version != previous_version.saturating_add(1) {
                return Err(RuntimeError::RecoveryInvalid);
            }
            previous_version = version;
            latest_position = Some(committed);
        }
        if let Some(active_key) = active_key.as_deref() {
            let final_position = validate_stream_checkpoint_reset_bridge(
                &self.connection,
                active_key,
                previous_version,
                latest_position.as_deref(),
                checkpoint_version,
            )
            .await?;
            if final_position != checkpoint_position {
                return Err(RuntimeError::RecoveryInvalid);
            }
        }
        let mut reset_only = self
            .connection
            .query(
                "SELECT checkpoint.key_id, checkpoint.version,
                        checkpoint.committed_position
                 FROM stream_checkpoint AS checkpoint
                 WHERE checkpoint.version > 0
                   AND NOT EXISTS (
                       SELECT 1 FROM stream_checkpoint_history AS history
                       WHERE history.key_id = checkpoint.key_id
                   )",
                (),
            )
            .await?;
        let mut reset_only_checkpoints = Vec::new();
        while let Some(row) = reset_only.next().await? {
            let key_id: String = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let version = decode_u64(
                row.get::<i64>(1)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?;
            let position: Option<String> = row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?;
            reset_only_checkpoints.push((key_id, version, position));
        }
        drop(reset_only);
        for (key_id, version, position) in reset_only_checkpoints {
            let (origin_version, origin_position) =
                load_stream_checkpoint_migration_origin(&self.connection, &key_id).await?;
            let reset_position = validate_stream_checkpoint_reset_bridge(
                &self.connection,
                &key_id,
                origin_version,
                origin_position.as_deref(),
                version,
            )
            .await?;
            if reset_position != position {
                return Err(RuntimeError::RecoveryInvalid);
            }
        }
        Ok(())
    }

    async fn validate_stream_provider_failures(&self) -> Result<(), RuntimeError> {
        let mut rows = self
            .connection
            .query(
                "SELECT failure.key_id, failure.checkpoint_version,
                        failure.committed_position, failure.attempts,
                        failure.diagnostic_code, failure.diagnostic_class,
                        checkpoint.version, checkpoint.committed_position
                 FROM stream_provider_failure AS failure
                 LEFT JOIN stream_checkpoint AS checkpoint
                   ON checkpoint.key_id = failure.key_id",
                (),
            )
            .await?;
        while let Some(row) = rows.next().await? {
            let key_id: String = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
            if key_id.is_empty() {
                return Err(RuntimeError::RecoveryInvalid);
            }
            let failure_version = decode_u64(
                row.get::<i64>(1)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?;
            let failure_position: Option<String> =
                row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let attempts = decode_u32(
                row.get::<i64>(3)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?;
            if attempts == 0 {
                return Err(RuntimeError::RecoveryInvalid);
            }
            decode_code(
                row.get::<i64>(4)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?;
            decode_class(
                row.get::<i64>(5)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?;
            let checkpoint_version = row
                .get::<Option<i64>>(6)
                .map_err(|_| RuntimeError::RecoveryInvalid)?
                .ok_or(RuntimeError::RecoveryInvalid)
                .and_then(decode_u64)?;
            let checkpoint_position: Option<String> =
                row.get(7).map_err(|_| RuntimeError::RecoveryInvalid)?;
            if failure_version != checkpoint_version || failure_position != checkpoint_position {
                return Err(RuntimeError::RecoveryInvalid);
            }
        }
        Ok(())
    }

    async fn validate_stream_failure_payloads(&self) -> Result<(), RuntimeError> {
        let mut rows = self
            .connection
            .query(
                "SELECT failure.identity_id, payload.identity_id, payload.payload,
                        payload.payload_reference, payload.payload_digest, payload.retention,
                        legacy.identity_id
                 FROM stream_failure AS failure
                 LEFT JOIN stream_failure_payload AS payload
                   ON payload.identity_id = failure.identity_id
                 LEFT JOIN stream_failure_payload_legacy AS legacy
                   ON legacy.identity_id = failure.identity_id",
                (),
            )
            .await?;
        while let Some(row) = rows.next().await? {
            let identity_id: String = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
            if identity_id.is_empty() {
                return Err(RuntimeError::RecoveryInvalid);
            }
            let referenced: Option<String> =
                row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let legacy: Option<String> = row.get(6).map_err(|_| RuntimeError::RecoveryInvalid)?;
            if referenced.is_none() {
                if legacy.as_deref() != Some(identity_id.as_str()) {
                    return Err(RuntimeError::RecoveryInvalid);
                }
                continue;
            }
            if referenced.as_deref() != Some(identity_id.as_str()) || legacy.is_some() {
                return Err(RuntimeError::RecoveryInvalid);
            }
            StoredFailurePayload::from_columns(
                row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?,
                row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?,
                row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?,
                row.get(5).map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?;
        }

        let mut orphans = self
            .connection
            .query(
                "SELECT payload.identity_id
                 FROM stream_failure_payload AS payload
                 LEFT JOIN stream_failure AS failure
                   ON failure.identity_id = payload.identity_id
                 WHERE failure.identity_id IS NULL",
                (),
            )
            .await?;
        if orphans.next().await?.is_some() {
            return Err(RuntimeError::RecoveryInvalid);
        }
        Ok(())
    }
}
