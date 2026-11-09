//! Publication policy, metadata, freezes and the pending-mutation prefix they consume.

use crate::codec::{fixed, now_ms, validate_digest, validate_id};
use crate::error::RuntimeError;
use crate::table_store::{Mutation, TableMutation, validate_table_name};
use crate::{RuntimeState, process_owner_id};
use ed25519_dalek::{Signature, Signer, Verifier, VerifyingKey};
use libsql::{Connection, TransactionBehavior, params};
use orna_foundation::{CanonicalValue, OvbRaw};
use orna_repository::{
    CompactPublicationPending, CompactRuntimeReceipt, Repository, RepositoryError,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

/// Validates the schema-admission identities a compact freeze pins.
fn validate_compact_identities(
    checkpoint: &Checkpoint,
    identities: &[PublicationTableIdentity],
) -> Result<(), RuntimeError> {
    if checkpoint.generation == 0 || identities.is_empty() {
        return Err(RuntimeError::RecoveryInvalid);
    }
    let mut seen = BTreeMap::new();
    for identity in identities {
        validate_table_name(&identity.table)?;
        if identity.candidate_generation != checkpoint.generation
            || identity.table_id == [0; 16]
            || identity.schema_fingerprint == [0; 32]
            || identity.candidate_generation == 0
            || seen.insert(identity.table.clone(), ()).is_some()
        {
            return Err(RuntimeError::RecoveryInvalid);
        }
    }
    Ok(())
}

/// Effective publication policy used by the runtime's compact publisher.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimePublicationPolicy {
    /// Target compressed bytes per publication batch.
    pub compressed_target_bytes: u64,
    /// Maximum encoded file size for the active compact profile.
    pub max_file_bytes: u64,
    /// Maximum age, in seconds, for a pending batch before publication.
    pub max_pending_age_seconds: u64,
}

impl Default for RuntimePublicationPolicy {
    fn default() -> Self {
        Self {
            compressed_target_bytes: 16 * 1024 * 1024,
            max_file_bytes: 64 * 1024 * 1024,
            max_pending_age_seconds: 60,
        }
    }
}

/// One coherent read of effective policy and durable publication state.
///
/// Published counters and `last_publication_ms` cover completions recorded
/// since this metadata ledger was installed. Publications completed before
/// installation cannot be reconstructed because their pending payloads have
/// already been removed; a zero count after upgrading an older runtime means
/// "not recorded by this ledger," not necessarily "never published."
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimePublicationMetadata {
    pub publication_policy: RuntimePublicationPolicy,
    /// Durable mutations waiting for publication.
    pub pending_rows: u64,
    /// Encoded payload bytes waiting for publication.
    pub pending_bytes: u64,
    /// Durable mutations consumed by completed publications.
    pub published_rows: u64,
    /// Encoded payload bytes consumed by completed publications.
    pub published_bytes: u64,
    /// Unix epoch milliseconds for the most recently completed publication.
    pub last_publication_ms: Option<i64>,
}

/// Canonical publication field records corresponding to both system views.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimePublicationMetadataRows {
    pub sys_storage: CanonicalValue,
    pub maintenance_job: CanonicalValue,
}

fn publication_metadata_record(
    metadata: &RuntimePublicationMetadata,
) -> Result<CanonicalValue, RuntimeError> {
    let policy = publication_text_map(vec![
        (
            "compressed_target_bytes",
            OvbRaw::Int(metadata.publication_policy.compressed_target_bytes.into()),
        ),
        (
            "max_file_bytes",
            OvbRaw::Int(metadata.publication_policy.max_file_bytes.into()),
        ),
        (
            "max_pending_age_seconds",
            OvbRaw::Int(metadata.publication_policy.max_pending_age_seconds.into()),
        ),
    ]);
    let last_publication = match metadata.last_publication_ms {
        Some(milliseconds) => {
            let seconds = milliseconds.div_euclid(1_000);
            let nanoseconds = milliseconds.rem_euclid(1_000) * 1_000_000;
            OvbRaw::Tag(
                60002,
                Box::new(OvbRaw::Array(vec![
                    OvbRaw::Int(seconds.into()),
                    OvbRaw::Int(nanoseconds.into()),
                ])),
            )
        }
        None => OvbRaw::Null,
    };
    CanonicalValue::new(publication_text_map(vec![
        ("publication_policy", policy),
        ("pending_rows", OvbRaw::Int(metadata.pending_rows.into())),
        ("pending_bytes", OvbRaw::Int(metadata.pending_bytes.into())),
        (
            "published_rows",
            OvbRaw::Int(metadata.published_rows.into()),
        ),
        (
            "published_bytes",
            OvbRaw::Int(metadata.published_bytes.into()),
        ),
        ("last_publication", last_publication),
    ]))
    .map_err(|_| RuntimeError::RecoveryInvalid)
}

pub(crate) fn publication_text_map(fields: Vec<(&str, OvbRaw)>) -> OvbRaw {
    let mut fields = fields;
    fields.sort_by(|(left, _), (right, _)| {
        left.len().cmp(&right.len()).then_with(|| left.cmp(right))
    });
    OvbRaw::Map(
        fields
            .into_iter()
            .map(|(key, value)| (OvbRaw::Text(key.into()), value))
            .collect(),
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Checkpoint {
    pub generation: u64,
    pub digest: [u8; 32],
    pub mutation_sequence: u64,
}

/// The canonical row encoding selected by the compact publication profile.
///
/// This is deliberately a closed type: publication must not silently accept
/// an arbitrary encoder label from an adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationRowEncoding {
    CompactOvb1,
}

/// The canonical value encoding selected by the compact publication profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationValueEncoding {
    Ovb1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublicationMutationState {
    Replacement { value: Vec<u8> },
    Deletion,
}

/// Evidence tying one published key/value to the exact candidate generated
/// from the frozen, ordered mutation prefix.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublicationEquivalenceWitness {
    pub key_digest: [u8; 32],
    pub value_digest: Option<[u8; 32]>,
    pub candidate_digest: [u8; 32],
}

/// One validated runtime mutation in a compact-publication freeze.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationFreezeMutation {
    pub table_id: [u8; 16],
    pub sequence: u64,
    pub mutation_id: [u8; 16],
    pub table: String,
    pub logical_key: Vec<u8>,
    pub state: PublicationMutationState,
    pub schema_fingerprint: [u8; 32],
    pub row_encoding_identity: PublicationRowEncoding,
    pub value_encoding_identity: PublicationValueEncoding,
    pub candidate_generation: u64,
    pub equivalence_witness: PublicationEquivalenceWitness,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationFreeze {
    pub intent_id: [u8; 16],
    pub checkpoint: Checkpoint,
    pub candidate_digest: [u8; 32],
    pub mutations: Vec<PublicationFreezeMutation>,
}

/// The compact-binding columns of one `publication_freeze` row.
///
/// The schema CHECK admits only all-NULL (an ordinary freeze) or all-set (a
/// freeze bound to one compact publication). The impossible mixed shape stays
/// a third case because the two readers report it differently: binding refuses
/// it as a conflicting intent, while completing an ordinary publication keeps
/// reporting that a compact receipt is required.
#[derive(Clone, Debug, Eq, PartialEq)]
enum FreezeBinding {
    /// No compact publication claims the freeze.
    Unbound,
    /// Every compact column is present.
    CompactBound {
        watermark: Vec<u8>,
        commit: Vec<u8>,
        verifier: Vec<u8>,
    },
    /// Some, but not all, compact columns are present.
    Inconsistent,
}

/// Decodes the three nullable compact columns selected from
/// `publication_freeze`. Lengths are checked by the caller that consumes the
/// digests, so the bind path and the ordinary completion path keep their
/// existing validation.
fn decode_freeze_binding(
    watermark: Option<Vec<u8>>,
    commit: Option<Vec<u8>>,
    verifier: Option<Vec<u8>>,
) -> FreezeBinding {
    match (watermark, commit, verifier) {
        (None, None, None) => FreezeBinding::Unbound,
        (Some(watermark), Some(commit), Some(verifier)) => FreezeBinding::CompactBound {
            watermark,
            commit,
            verifier,
        },
        _ => FreezeBinding::Inconsistent,
    }
}

/// An operation for projecting frozen runtime mutations against a pinned row root.
///
/// Insert remains insert-only; replacement may create or replace a row, while
/// deletion removes the key. Rekey preserves the explicit source-to-destination
/// move instead of reducing it to a replacement at the source key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublicationRowOperation {
    Insert {
        value: Vec<u8>,
    },
    Replacement {
        value: Vec<u8>,
    },
    Deletion,
    Rekey {
        destination_key: Vec<u8>,
        value: Vec<u8>,
    },
}

/// One validated operation from the runtime's ordered publication prefix.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationRowMutation {
    pub table_id: [u8; 16],
    pub sequence: u64,
    pub mutation_id: [u8; 16],
    pub table: String,
    pub logical_key: Vec<u8>,
    pub operation: PublicationRowOperation,
    pub schema_fingerprint: [u8; 32],
    pub row_encoding_identity: PublicationRowEncoding,
    pub value_encoding_identity: PublicationValueEncoding,
    pub candidate_generation: u64,
    pub equivalence_witness: PublicationEquivalenceWitness,
}

/// Validated, ordered operations covered by one persisted publication freeze.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationRowMutationBatch {
    pub intent_id: [u8; 16],
    pub checkpoint: Checkpoint,
    pub candidate_digest: [u8; 32],
    pub mutations: Vec<PublicationRowMutation>,
}

/// Authoritative compact profile identity supplied by schema admission.
///
/// Runtime table rows intentionally do not invent a schema or physical table
/// identity. Compact publication must receive this checked profile explicitly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationTableIdentity {
    pub table: String,
    pub table_id: [u8; 16],
    pub schema_fingerprint: [u8; 32],
    pub candidate_generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationCommitId(Vec<u8>);

impl PublicationCommitId {
    pub fn new(value: impl Into<Vec<u8>>) -> Result<Self, RuntimeError> {
        let value = value.into();
        if !matches!(value.len(), 40 | 64) || !value.iter().all(u8::is_ascii_hexdigit) {
            return Err(RuntimeError::InvalidPublicationCommit);
        }
        Ok(Self(value))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

fn publication_freeze_mutation(
    sequence: u64,
    mutation: &TableMutation,
    candidate_generation: u64,
    candidate_digest: [u8; 32],
    identity: &PublicationTableIdentity,
) -> Result<PublicationFreezeMutation, RuntimeError> {
    if sequence == 0
        || candidate_generation == 0
        || identity.table != mutation.table()
        || identity.candidate_generation != candidate_generation
    {
        return Err(RuntimeError::RecoveryInvalid);
    }
    let state = publication_mutation_state(mutation);
    let value_digest = mutation.value().map(|value| Sha256::digest(value).into());
    let witness = PublicationEquivalenceWitness {
        key_digest: Sha256::digest(mutation.key()).into(),
        value_digest,
        candidate_digest,
    };
    Ok(PublicationFreezeMutation {
        table_id: identity.table_id,
        sequence,
        mutation_id: mutation.id(),
        table: mutation.table().to_owned(),
        logical_key: mutation.key().to_vec(),
        state,
        schema_fingerprint: identity.schema_fingerprint,
        row_encoding_identity: PublicationRowEncoding::CompactOvb1,
        value_encoding_identity: PublicationValueEncoding::Ovb1,
        candidate_generation,
        equivalence_witness: witness,
    })
}

fn publication_mutation_state(mutation: &TableMutation) -> PublicationMutationState {
    match mutation.value() {
        Some(value) => PublicationMutationState::Replacement {
            value: value.to_vec(),
        },
        None => PublicationMutationState::Deletion,
    }
}

fn publication_row_operation(
    mutation: &TableMutation,
) -> Result<PublicationRowOperation, RuntimeError> {
    match (mutation.rekey_to(), mutation.is_insert(), mutation.value()) {
        (Some(destination_key), false, Some(value)) => Ok(PublicationRowOperation::Rekey {
            destination_key: destination_key.to_vec(),
            value: value.to_vec(),
        }),
        (None, true, Some(value)) => Ok(PublicationRowOperation::Insert {
            value: value.to_vec(),
        }),
        (None, false, Some(value)) => Ok(PublicationRowOperation::Replacement {
            value: value.to_vec(),
        }),
        (None, false, None) => Ok(PublicationRowOperation::Deletion),
        _ => Err(RuntimeError::InvalidTableMutation),
    }
}

fn publication_candidate_digest<'a>(
    generation: u64,
    mutations: impl IntoIterator<Item = (u64, &'a TableMutation)>,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"ORNA-COMPACT-CANDIDATE-1\0");
    hasher.update(generation.to_be_bytes());
    for (sequence, mutation) in mutations {
        hasher.update(sequence.to_be_bytes());
        hasher.update((mutation.key().len() as u64).to_be_bytes());
        hasher.update(mutation.key());
        match mutation.value() {
            Some(value) => {
                hasher.update([1]);
                hasher.update((value.len() as u64).to_be_bytes());
                hasher.update(value);
            }
            None => {
                hasher.update([2]);
                hasher.update(0_u64.to_be_bytes());
            }
        }
    }
    hasher.finalize().into()
}

/// Decodes one `checkpoint` row selected as
/// `(generation, mutation_sequence, digest)`.
fn decode_checkpoint_row(row: &libsql::Row) -> Result<Checkpoint, RuntimeError> {
    let generation = u64::try_from(
        row.get::<i64>(0)
            .map_err(|_| RuntimeError::RecoveryInvalid)?,
    )
    .map_err(|_| RuntimeError::RecoveryInvalid)?;
    let mutation_sequence = u64::try_from(
        row.get::<i64>(1)
            .map_err(|_| RuntimeError::RecoveryInvalid)?,
    )
    .map_err(|_| RuntimeError::RecoveryInvalid)?;
    Ok(Checkpoint {
        generation,
        digest: fixed(row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
        mutation_sequence,
    })
}

/// Counts the frozen pending prefix through `upper` and adds it to the
/// publication statistics row inside the caller's transaction.
///
/// Ordinary and compact completion publish the same prefix; one helper keeps
/// the measured range and the accumulated counters equal on both paths.
async fn record_published_prefix_tx(
    transaction: &Connection,
    upper: i64,
) -> Result<(), RuntimeError> {
    let mut batch_rows = transaction
        .query(
            "SELECT COUNT(*), COALESCE(SUM(length(payload)), 0) \
             FROM pending_mutation WHERE sequence <= ?1",
            params![upper],
        )
        .await?;
    let batch = batch_rows
        .next()
        .await?
        .ok_or(RuntimeError::RecoveryInvalid)?;
    let published_rows: i64 = batch.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let published_bytes: i64 = batch.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
    transaction
        .execute(
            "UPDATE publication_metadata SET \
             published_mutations = published_mutations + ?1, \
             published_payload_bytes = published_payload_bytes + ?2, \
             last_publication_ms = ?3 WHERE singleton = 1",
            params![published_rows, published_bytes, now_ms()?],
        )
        .await?;
    Ok(())
}

impl RuntimeState {
    pub async fn pending(&self) -> Result<Vec<Mutation>, RuntimeError> {
        let mut rows = self
            .connection
            .query(
                "SELECT mutation_id, payload, digest FROM pending_mutation ORDER BY sequence",
                (),
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            out.push(Mutation {
                id: fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
                payload: row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?,
                digest: fixed(row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
            });
        }
        Ok(out)
    }

    /// Returns the number of durable mutations still in the unpublished CWD tail.
    ///
    /// This summary query avoids loading mutation payloads when a caller only
    /// needs to report whether and how much local state remains unpublished.
    pub async fn pending_count(&self) -> Result<usize, RuntimeError> {
        Self::count_pending(&self.connection).await
    }

    /// Counts the unpublished tail of the runtime at `repository` without
    /// writing to its database. Unlike [`RuntimeState::open`], this neither
    /// creates `state.db` nor runs schema setup: the database is opened
    /// read-only. A repository with no `state.db` has an empty tail. The count
    /// is keyed by table, not by runtime identity, so it needs no identity.
    ///
    /// The probe takes the runtime owner lock without blocking. Only while it
    /// holds that lock may it remove the `-wal`/`-shm` sidecars it created: a
    /// live owner using those sidecars holds the lock, so they are then left
    /// intact. A busy lock still counts read-only and removes nothing.
    pub async fn read_pending_count(repository: &Repository) -> Result<usize, RuntimeError> {
        let state_db = repository.runtime_paths().state_db();
        if !state_db.exists() {
            return Ok(0);
        }
        // Acquiring the owner lock rewrites `runtime-owner.state`. The bytes are
        // read before the lock is taken, so a restore reinstates what the probe
        // found. Another owner could write between this read and the lock; a
        // probe that wins the lock then restores the earlier bytes over that write.
        let owner_state_path = repository
            .runtime_paths()
            .locks()
            .join("runtime-owner.state");
        let owner_state_before = match std::fs::read(&owner_state_path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err(RuntimeError::StorageUnavailable),
        };
        let owner_lock = match repository.acquire_runtime_owner_lock(process_owner_id()) {
            Ok(lock) => Some(lock),
            Err(RepositoryError::RepositoryBusy) => None,
            Err(_) => return Err(RuntimeError::StorageUnavailable),
        };
        let sidecars = ["-wal", "-shm"].map(|suffix| {
            let mut path = state_db.clone().into_os_string();
            path.push(suffix);
            std::path::PathBuf::from(path)
        });
        let existed = sidecars.clone().map(|path| path.exists());
        let count = async {
            let database = libsql::Builder::new_local(&state_db)
                .flags(libsql::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .build()
                .await?;
            let connection = database.connect()?;
            Self::count_pending(&connection).await
        }
        .await;
        let restored = if owner_lock.is_some() {
            for (path, existed) in sidecars.iter().zip(existed) {
                if !existed {
                    let _ = std::fs::remove_file(path);
                }
            }
            Self::restore_owner_state(&owner_state_path, owner_state_before.as_deref())
        } else {
            Ok(())
        };
        drop(owner_lock);
        match (count, restored) {
            (Ok(pending), Ok(())) => Ok(pending),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }

    /// Reinstates `runtime-owner.state` to the bytes captured before a probe
    /// acquired the owner lock, or removes it when it was absent. Called only
    /// while the probe holds the lock, so the write cannot race another owner.
    fn restore_owner_state(path: &Path, bytes: Option<&[u8]>) -> Result<(), RuntimeError> {
        match bytes {
            Some(bytes) => {
                let temporary = path.with_extension("restore");
                let mut file = std::fs::File::create(&temporary)
                    .map_err(|_| RuntimeError::StorageUnavailable)?;
                file.write_all(bytes)
                    .and_then(|()| file.sync_all())
                    .map_err(|_| RuntimeError::StorageUnavailable)?;
                std::fs::rename(&temporary, path).map_err(|_| RuntimeError::StorageUnavailable)
            }
            None => match std::fs::remove_file(path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(_) => Err(RuntimeError::StorageUnavailable),
            },
        }
    }

    async fn count_pending(connection: &libsql::Connection) -> Result<usize, RuntimeError> {
        let mut rows = connection
            .query("SELECT COUNT(*) FROM pending_mutation", ())
            .await?;
        let row = rows.next().await?.ok_or(RuntimeError::RecoveryInvalid)?;
        let count: i64 = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
        usize::try_from(count).map_err(|_| RuntimeError::RecoveryInvalid)
    }

    /// Reads effective publication policy and pending/published state from one
    /// SQLite statement so maintenance and storage projections share a
    /// coherent snapshot. Row and byte counts describe durable runtime
    /// mutation payloads, not physical compact-file layout.
    pub(crate) async fn set_publication_compressed_target_bytes_inner(
        &self,
        compressed_target_bytes: u64,
    ) -> Result<(), RuntimeError> {
        if !(8 * 1024 * 1024..=32 * 1024 * 1024).contains(&compressed_target_bytes) {
            return Err(RuntimeError::InvalidPublicationPolicy);
        }
        self.connection
            .execute(
                "UPDATE publication_metadata SET compressed_target_bytes = ?1 \
                 WHERE singleton = 1",
                [i64::try_from(compressed_target_bytes)
                    .map_err(|_| RuntimeError::InvalidPublicationPolicy)?],
            )
            .await?;
        Ok(())
    }

    pub async fn publication_metadata(&self) -> Result<RuntimePublicationMetadata, RuntimeError> {
        let mut rows = self
            .connection
            .query(
                "SELECT (SELECT COUNT(*) FROM pending_mutation), \
                 (SELECT COALESCE(SUM(length(payload)), 0) FROM pending_mutation), \
                 compressed_target_bytes, published_mutations, published_payload_bytes, last_publication_ms \
                 FROM publication_metadata WHERE singleton = 1",
                (),
            )
            .await
            ?;
        let row = rows.next().await?.ok_or(RuntimeError::RecoveryInvalid)?;
        let pending_rows: i64 = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let pending_bytes: i64 = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let compressed_target_bytes: i64 = row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let compressed_target_bytes =
            u64::try_from(compressed_target_bytes).map_err(|_| RuntimeError::RecoveryInvalid)?;
        if !(8 * 1024 * 1024..=32 * 1024 * 1024).contains(&compressed_target_bytes) {
            return Err(RuntimeError::RecoveryInvalid);
        }
        let published_rows: i64 = row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let published_bytes: i64 = row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?;
        Ok(RuntimePublicationMetadata {
            publication_policy: RuntimePublicationPolicy {
                compressed_target_bytes,
                ..RuntimePublicationPolicy::default()
            },
            pending_rows: u64::try_from(pending_rows).map_err(|_| RuntimeError::RecoveryInvalid)?,
            pending_bytes: u64::try_from(pending_bytes)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
            published_rows: u64::try_from(published_rows)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
            published_bytes: u64::try_from(published_bytes)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
            last_publication_ms: row.get(5).map_err(|_| RuntimeError::RecoveryInvalid)?,
        })
    }

    /// Reads one durable snapshot and projects its publication fields into
    /// records corresponding to the declared `sys.Storage` and
    /// `sys.MaintenanceJob` views.
    pub async fn publication_metadata_rows(
        &self,
    ) -> Result<RuntimePublicationMetadataRows, RuntimeError> {
        let row = publication_metadata_record(&self.publication_metadata().await?)?;
        Ok(RuntimePublicationMetadataRows {
            sys_storage: row.clone(),
            maintenance_job: row,
        })
    }

    /// Returns only the pending mutation prefix covered by a persisted freeze.
    /// Mutations appended after the freeze are intentionally excluded.
    pub async fn pending_through(
        &self,
        freeze: &PublicationFreeze,
    ) -> Result<Vec<Mutation>, RuntimeError> {
        validate_id(freeze.intent_id)?;
        validate_digest(freeze.checkpoint.digest)?;
        let stored = self
            .frozen_intent(freeze.intent_id)
            .await?
            .ok_or(RuntimeError::RecoveryInvalid)?;
        if stored != freeze.checkpoint {
            return Err(RuntimeError::ConflictingPublicationIntent);
        }
        let upper = i64::try_from(freeze.checkpoint.mutation_sequence)
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
        let mut rows = self
            .connection
            .query(
                "SELECT mutation_id, payload, digest FROM pending_mutation WHERE sequence <= ?1 ORDER BY sequence",
                params![upper],
            )
            .await
            ?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            out.push(Mutation {
                id: fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
                payload: row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?,
                digest: fixed(row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
            });
        }
        Ok(out)
    }

    /// Returns the typed table operations covered by a persisted freeze.
    /// Non-table mutations fail closed; newer pending tail mutations remain
    /// outside the returned range.
    pub async fn pending_table_mutations_through(
        &self,
        freeze: &PublicationFreeze,
    ) -> Result<Vec<TableMutation>, RuntimeError> {
        self.pending_through(freeze)
            .await?
            .iter()
            .map(TableMutation::decode)
            .collect()
    }

    /// Returns the typed compact payload retained on a typed freeze.
    pub async fn frozen_pending_mutations(
        &self,
        freeze: &PublicationFreeze,
    ) -> Result<Vec<PublicationFreezeMutation>, RuntimeError> {
        validate_id(freeze.intent_id)?;
        validate_digest(freeze.checkpoint.digest)?;
        let stored = self
            .frozen_intent(freeze.intent_id)
            .await?
            .ok_or(RuntimeError::RecoveryInvalid)?;
        if stored != freeze.checkpoint {
            return Err(RuntimeError::ConflictingPublicationIntent);
        }
        if freeze.mutations.is_empty()
            || freeze.mutations.iter().any(|mutation| {
                mutation.equivalence_witness.candidate_digest != freeze.candidate_digest
            })
        {
            return Err(RuntimeError::RecoveryInvalid);
        }
        Ok(freeze.mutations.clone())
    }

    /// Returns the ordered row operations covered by a persisted freeze.
    ///
    /// The batch preserves insert-only and re-key intent, validates every
    /// operation against its compact freeze witness, and excludes later tails.
    pub async fn frozen_row_mutation_batch(
        &self,
        freeze: &PublicationFreeze,
    ) -> Result<PublicationRowMutationBatch, RuntimeError> {
        let frozen = self.frozen_pending_mutations(freeze).await?;
        let pending = self.pending_table_mutations_through(freeze).await?;
        if frozen.len() != pending.len() {
            return Err(RuntimeError::RecoveryInvalid);
        }

        let mut mutations = Vec::with_capacity(frozen.len());
        let mut previous_sequence = 0;
        for (frozen_mutation, mutation) in frozen.iter().zip(&pending) {
            if frozen_mutation.sequence <= previous_sequence
                || frozen_mutation.sequence > freeze.checkpoint.mutation_sequence
            {
                return Err(RuntimeError::RecoveryInvalid);
            }
            let identity = PublicationTableIdentity {
                table: frozen_mutation.table.clone(),
                table_id: frozen_mutation.table_id,
                schema_fingerprint: frozen_mutation.schema_fingerprint,
                candidate_generation: freeze.checkpoint.generation,
            };
            let expected = publication_freeze_mutation(
                frozen_mutation.sequence,
                mutation,
                freeze.checkpoint.generation,
                freeze.candidate_digest,
                &identity,
            )?;
            if expected != *frozen_mutation {
                return Err(RuntimeError::RecoveryInvalid);
            }
            mutations.push(PublicationRowMutation {
                table_id: frozen_mutation.table_id,
                sequence: frozen_mutation.sequence,
                mutation_id: frozen_mutation.mutation_id,
                table: frozen_mutation.table.clone(),
                logical_key: frozen_mutation.logical_key.clone(),
                operation: publication_row_operation(mutation)?,
                schema_fingerprint: frozen_mutation.schema_fingerprint,
                row_encoding_identity: frozen_mutation.row_encoding_identity,
                value_encoding_identity: frozen_mutation.value_encoding_identity,
                candidate_generation: frozen_mutation.candidate_generation,
                equivalence_witness: frozen_mutation.equivalence_witness,
            });
            previous_sequence = frozen_mutation.sequence;
        }
        if (freeze.checkpoint.mutation_sequence != 0
            && previous_sequence != freeze.checkpoint.mutation_sequence)
            || publication_candidate_digest(
                freeze.checkpoint.generation,
                frozen
                    .iter()
                    .zip(&pending)
                    .map(|(record, mutation)| (record.sequence, mutation)),
            ) != freeze.candidate_digest
        {
            return Err(RuntimeError::RecoveryInvalid);
        }
        Ok(PublicationRowMutationBatch {
            intent_id: freeze.intent_id,
            checkpoint: freeze.checkpoint.clone(),
            candidate_digest: freeze.candidate_digest,
            mutations,
        })
    }

    /// Returns the newest durable checkpoint, if this runtime has committed
    /// one. Checkpoints are never inferred from a pending mutation.
    pub async fn latest_checkpoint(&self) -> Result<Option<Checkpoint>, RuntimeError> {
        let mut rows = self
            .connection
            .query(
                "SELECT generation, mutation_sequence, digest FROM checkpoint ORDER BY generation DESC LIMIT 1",
                (),
            )
            .await
            ?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        Ok(Some(decode_checkpoint_row(&row)?))
    }

    pub(crate) async fn freeze_inner(
        &self,
        intent_id: [u8; 16],
        checkpoint: &Checkpoint,
    ) -> Result<PublicationFreeze, RuntimeError> {
        validate_id(intent_id)?;
        validate_digest(checkpoint.digest)?;
        if let Some(stored) = self.frozen_intent(intent_id).await? {
            if &stored != checkpoint {
                return Err(RuntimeError::ConflictingPublicationIntent);
            }
            let candidate_digest = publication_candidate_digest(
                stored.generation,
                std::iter::empty::<(u64, &TableMutation)>(),
            );
            return Ok(PublicationFreeze {
                intent_id,
                checkpoint: stored,
                candidate_digest,
                mutations: Vec::new(),
            });
        }
        if self.latest_checkpoint().await?.as_ref() != Some(checkpoint) {
            return Err(RuntimeError::RecoveryInvalid);
        }
        self.connection
            .execute(
                "INSERT INTO publication_freeze \
                 (intent_id, checkpoint_generation, checkpoint_mutation_sequence, checkpoint_digest, frozen) \
                 VALUES (?1, ?2, ?3, ?4, 1) ON CONFLICT(intent_id) DO NOTHING",
                params![
                    intent_id.to_vec(),
                    i64::try_from(checkpoint.generation)
                        .map_err(|_| RuntimeError::RecoveryInvalid)?,
                    i64::try_from(checkpoint.mutation_sequence)
                        .map_err(|_| RuntimeError::RecoveryInvalid)?,
                    checkpoint.digest.to_vec(),
                ],
            )
            .await
            ?;
        let stored = self
            .frozen_intent(intent_id)
            .await?
            .ok_or(RuntimeError::RecoveryInvalid)?;
        if &stored != checkpoint {
            return Err(RuntimeError::ConflictingPublicationIntent);
        }
        let candidate_digest = publication_candidate_digest(
            stored.generation,
            std::iter::empty::<(u64, &TableMutation)>(),
        );
        Ok(PublicationFreeze {
            intent_id,
            checkpoint: stored,
            candidate_digest,
            mutations: Vec::new(),
        })
    }

    /// Freezes a compact publication using schema admission's authoritative
    /// table/profile identities. The legacy [`PublicationWriter::freeze`] API
    /// intentionally carries no synthetic identity and therefore cannot
    /// produce compact mutations.
    pub(crate) async fn freeze_compact_inner(
        &self,
        intent_id: [u8; 16],
        checkpoint: &Checkpoint,
        identities: &[PublicationTableIdentity],
    ) -> Result<PublicationFreeze, RuntimeError> {
        validate_compact_identities(checkpoint, identities)?;
        let mut freeze = self.freeze_inner(intent_id, checkpoint).await?;
        self.load_compact_freeze(&mut freeze, identities).await?;
        Ok(freeze)
    }

    /// Fills a compact freeze with the frozen prefix's typed mutations and the
    /// candidate digest they certify.
    async fn load_compact_freeze(
        &self,
        freeze: &mut PublicationFreeze,
        identities: &[PublicationTableIdentity],
    ) -> Result<(), RuntimeError> {
        freeze.mutations = self
            .load_publication_mutations(&freeze.checkpoint, identities)
            .await?;
        freeze.candidate_digest = freeze
            .mutations
            .first()
            .map(|mutation| mutation.equivalence_witness.candidate_digest)
            .ok_or(RuntimeError::EmptyMutationBatch)?;
        Ok(())
    }

    /// Durably binds a freeze to one compact candidate before receipt
    /// completion. Once bound, [`PublicationWriter::complete_publication`]
    /// rejects the freeze; only signed compact receipt completion may consume
    /// its prefix. Repeating the exact binding is safe after restart.
    pub(crate) async fn bind_compact_publication_inner(
        &self,
        pending: &CompactPublicationPending,
        freeze: &PublicationFreeze,
    ) -> Result<(), RuntimeError> {
        validate_id(freeze.intent_id)?;
        validate_digest(freeze.checkpoint.digest)?;
        if pending.runtime_intent_id() != freeze.intent_id
            || pending.cleanup_watermark() != freeze.checkpoint.digest
        {
            return Err(RuntimeError::ConflictingPublicationIntent);
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        let mut rows = transaction
            .query(
                "SELECT checkpoint_generation, checkpoint_mutation_sequence, checkpoint_digest, \
                 compact_watermark, compact_commit_id, compact_journal_verifier \
                 FROM publication_freeze WHERE intent_id = ?1",
                params![freeze.intent_id.to_vec()],
            )
            .await?;
        let row = rows.next().await?.ok_or(RuntimeError::RecoveryInvalid)?;
        if decode_checkpoint_row(&row)? != freeze.checkpoint {
            return Err(RuntimeError::ConflictingPublicationIntent);
        }
        let mut completed_rows = transaction
            .query(
                "SELECT commit_id, compact_receipt FROM publication_commit \
                 WHERE intent_id = ?1",
                params![freeze.intent_id.to_vec()],
            )
            .await?;
        if let Some(completed) = completed_rows.next().await? {
            let completed_commit: Vec<u8> = completed
                .get(0)
                .map_err(|_| RuntimeError::RecoveryInvalid)?;
            let encoded: Option<Vec<u8>> = completed
                .get(1)
                .map_err(|_| RuntimeError::RecoveryInvalid)?;
            let receipt = CompactRuntimeReceipt::decode(
                encoded
                    .as_deref()
                    .ok_or(RuntimeError::ConflictingPublicationCommit)?,
            )
            .map_err(|_| RuntimeError::InvalidCompactReceipt)?;
            self.verify_compact_receipt(&receipt)?;
            if completed_commit != pending.commit().as_str().as_bytes()
                || receipt.commit() != pending.commit()
                || receipt.runtime_intent_id() != pending.runtime_intent_id()
                || receipt.cleanup_watermark() != pending.cleanup_watermark()
                || receipt.journal_verifier() != pending.journal_verifier()
            {
                return Err(RuntimeError::ConflictingPublicationCommit);
            }
            transaction.commit().await?;
            return Ok(());
        }
        let watermark: Option<Vec<u8>> = row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let commit: Option<Vec<u8>> = row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let verifier: Option<Vec<u8>> = row.get(5).map_err(|_| RuntimeError::RecoveryInvalid)?;
        match decode_freeze_binding(watermark, commit, verifier) {
            FreezeBinding::Unbound => {
                transaction
                    .execute(
                        "UPDATE publication_freeze SET compact_watermark = ?1, \
                         compact_commit_id = ?2, compact_journal_verifier = ?3 \
                         WHERE intent_id = ?4",
                        params![
                            pending.cleanup_watermark().to_vec(),
                            pending.commit().as_str().as_bytes().to_vec(),
                            pending.journal_verifier().to_vec(),
                            freeze.intent_id.to_vec(),
                        ],
                    )
                    .await?;
            }
            FreezeBinding::CompactBound {
                watermark,
                commit,
                verifier,
            } => {
                if fixed::<32>(watermark)? != pending.cleanup_watermark()
                    || commit != pending.commit().as_str().as_bytes()
                    || fixed::<32>(verifier)? != pending.journal_verifier()
                {
                    return Err(RuntimeError::ConflictingPublicationIntent);
                }
            }
            FreezeBinding::Inconsistent => return Err(RuntimeError::ConflictingPublicationIntent),
        }
        transaction.commit().await.map_err(RuntimeError::from)
    }

    /// Atomically signs and persists a compact publication receipt with the
    /// candidate commit and deletion of only the frozen pending prefix.
    ///
    /// The returned receipt is stable across restart for one intent and
    /// candidate. The signing key is runtime-owned durable state; callers
    /// cannot supply signing or verification material.
    pub(crate) async fn complete_compact_publication_inner(
        &self,
        pending: &CompactPublicationPending,
        freeze: &PublicationFreeze,
    ) -> Result<CompactRuntimeReceipt, RuntimeError> {
        validate_id(freeze.intent_id)?;
        validate_digest(freeze.checkpoint.digest)?;
        if pending.runtime_intent_id() != freeze.intent_id
            || pending.cleanup_watermark() != freeze.checkpoint.digest
        {
            return Err(RuntimeError::ConflictingPublicationIntent);
        }
        self.bind_compact_publication_inner(pending, freeze).await?;
        let upper = i64::try_from(freeze.checkpoint.mutation_sequence)
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
        let signing_bytes = CompactRuntimeReceipt::signing_bytes(
            pending.runtime_intent_id(),
            pending.cleanup_watermark(),
            pending.commit(),
            pending.journal_verifier(),
        )
        .map_err(|_| RuntimeError::InvalidCompactReceipt)?;
        let signature = self
            .compact_receipt_signing_key
            .sign(&signing_bytes)
            .to_bytes();
        let receipt = CompactRuntimeReceipt::new(
            pending.runtime_intent_id(),
            pending.cleanup_watermark(),
            pending.commit().clone(),
            pending.journal_verifier(),
            signature,
        )
        .map_err(|_| RuntimeError::InvalidCompactReceipt)?;
        let encoded = receipt
            .encode()
            .map_err(|_| RuntimeError::InvalidCompactReceipt)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        let mut rows = tx
            .query(
                "SELECT commit_id, compact_receipt FROM publication_commit WHERE intent_id = ?1",
                params![freeze.intent_id.to_vec()],
            )
            .await?;
        if let Some(row) = rows.next().await? {
            let commit: Vec<u8> = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
            if commit != pending.commit().as_str().as_bytes() {
                return Err(RuntimeError::ConflictingPublicationCommit);
            }
            let encoded: Option<Vec<u8>> = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let existing = CompactRuntimeReceipt::decode(
                encoded
                    .as_deref()
                    .ok_or(RuntimeError::InvalidCompactReceipt)?,
            )
            .map_err(|_| RuntimeError::InvalidCompactReceipt)?;
            self.verify_compact_receipt(&existing)?;
            if existing != receipt {
                return Err(RuntimeError::ConflictingPublicationIntent);
            }
            tx.commit().await?;
            return Ok(existing);
        }
        record_published_prefix_tx(&tx, upper).await?;
        tx.execute(
            "INSERT INTO publication_commit (intent_id, commit_id, compact_receipt)
             VALUES (?1, ?2, ?3)",
            params![
                freeze.intent_id.to_vec(),
                pending.commit().as_str().as_bytes().to_vec(),
                encoded,
            ],
        )
        .await?;
        tx.execute(
            "DELETE FROM pending_mutation WHERE sequence <= ?1",
            params![upper],
        )
        .await?;
        tx.commit().await?;
        Ok(receipt)
    }

    /// Records a verified ordinary publication and consumes only the frozen
    /// pending prefix. Compact publication must use
    /// [`PublicationWriter::complete_compact_publication`] so its signed
    /// receipt is persisted in the same transaction.
    /// prefix. Repeating the same intent and commit is safe after restart;
    /// another commit for that intent is rejected.
    pub(crate) async fn complete_publication_inner(
        &self,
        freeze: &PublicationFreeze,
        commit_id: &PublicationCommitId,
    ) -> Result<(), RuntimeError> {
        validate_id(freeze.intent_id)?;
        validate_digest(freeze.checkpoint.digest)?;
        let stored = self
            .frozen_intent(freeze.intent_id)
            .await?
            .ok_or(RuntimeError::RecoveryInvalid)?;
        if stored != freeze.checkpoint {
            return Err(RuntimeError::ConflictingPublicationIntent);
        }
        let mut binding = self
            .connection
            .query(
                "SELECT compact_watermark, compact_commit_id, compact_journal_verifier \
                 FROM publication_freeze WHERE intent_id = ?1",
                params![freeze.intent_id.to_vec()],
            )
            .await?;
        let binding = binding.next().await?.ok_or(RuntimeError::RecoveryInvalid)?;
        let watermark: Option<Vec<u8>> =
            binding.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let commit: Option<Vec<u8>> = binding.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let verifier: Option<Vec<u8>> =
            binding.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?;
        match decode_freeze_binding(watermark, commit, verifier) {
            FreezeBinding::Unbound => {}
            FreezeBinding::CompactBound { .. } | FreezeBinding::Inconsistent => {
                return Err(RuntimeError::CompactPublicationRequired);
            }
        }
        let upper = i64::try_from(freeze.checkpoint.mutation_sequence)
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        let mut rows = tx
            .query(
                "SELECT commit_id FROM publication_commit WHERE intent_id = ?1",
                params![freeze.intent_id.to_vec()],
            )
            .await?;
        if let Some(row) = rows.next().await? {
            let existing = row
                .get::<Vec<u8>>(0)
                .map_err(|_| RuntimeError::RecoveryInvalid)?;
            if existing != commit_id.as_bytes() {
                return Err(RuntimeError::ConflictingPublicationCommit);
            }
        } else {
            tx.execute(
                "INSERT INTO publication_commit (intent_id, commit_id) VALUES (?1, ?2)",
                params![freeze.intent_id.to_vec(), commit_id.as_bytes().to_vec()],
            )
            .await?;
            record_published_prefix_tx(&tx, upper).await?;
        }
        tx.execute(
            "DELETE FROM pending_mutation WHERE sequence <= ?1",
            params![upper],
        )
        .await?;
        tx.commit().await.map_err(RuntimeError::from)
    }

    async fn load_publication_mutations(
        &self,
        checkpoint: &Checkpoint,
        identities: &[PublicationTableIdentity],
    ) -> Result<Vec<PublicationFreezeMutation>, RuntimeError> {
        let upper = i64::try_from(checkpoint.mutation_sequence)
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
        let mut rows = self
            .connection
            .query(
                "SELECT sequence, mutation_id, payload, digest
                 FROM pending_mutation WHERE sequence <= ?1 ORDER BY sequence",
                params![upper],
            )
            .await?;
        let mut previous = 0_u64;
        let mut decoded = Vec::new();
        while let Some(row) = rows.next().await? {
            let sequence = u64::try_from(
                row.get::<i64>(0)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
            if sequence == 0 || sequence <= previous {
                return Err(RuntimeError::RecoveryInvalid);
            }
            let mutation = Mutation {
                id: fixed(row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
                payload: row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?,
                digest: fixed(row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
            };
            decoded.push((sequence, TableMutation::decode(&mutation)?));
            previous = sequence;
        }
        if checkpoint.mutation_sequence != 0
            && decoded.last().map(|(sequence, _)| *sequence) != Some(checkpoint.mutation_sequence)
        {
            return Err(RuntimeError::RecoveryInvalid);
        }
        let candidate_digest = publication_candidate_digest(
            checkpoint.generation,
            decoded
                .iter()
                .map(|(sequence, mutation)| (*sequence, mutation)),
        );
        decoded
            .iter()
            .map(|(sequence, mutation)| {
                let identity = identities
                    .iter()
                    .find(|identity| identity.table == mutation.table())
                    .ok_or(RuntimeError::RecoveryInvalid)?;
                publication_freeze_mutation(
                    *sequence,
                    mutation,
                    checkpoint.generation,
                    candidate_digest,
                    identity,
                )
            })
            .collect()
    }

    async fn frozen_intent(&self, intent_id: [u8; 16]) -> Result<Option<Checkpoint>, RuntimeError> {
        let mut rows = self.connection.query("SELECT checkpoint_generation, checkpoint_mutation_sequence, checkpoint_digest FROM publication_freeze WHERE intent_id = ?1", params![intent_id.to_vec()]).await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        Ok(Some(decode_checkpoint_row(&row)?))
    }

    /// Reconstructs a persisted publication freeze for restart recovery.
    /// Unknown intents are reported as recovery-invalid rather than inferred
    /// from the current checkpoint or pending mutation tail.
    pub async fn publication_freeze(
        &self,
        intent_id: [u8; 16],
    ) -> Result<PublicationFreeze, RuntimeError> {
        validate_id(intent_id)?;
        let checkpoint = self
            .frozen_intent(intent_id)
            .await?
            .ok_or(RuntimeError::RecoveryInvalid)?;
        Ok(PublicationFreeze {
            intent_id,
            candidate_digest: publication_candidate_digest(
                checkpoint.generation,
                std::iter::empty::<(u64, &TableMutation)>(),
            ),
            mutations: Vec::new(),
            checkpoint,
        })
    }

    /// Reconstructs a typed compact freeze after restart using the same
    /// schema-admission identities supplied to the original producer.
    pub async fn publication_freeze_compact(
        &self,
        intent_id: [u8; 16],
        identities: &[PublicationTableIdentity],
    ) -> Result<PublicationFreeze, RuntimeError> {
        let checkpoint = self
            .frozen_intent(intent_id)
            .await?
            .ok_or(RuntimeError::RecoveryInvalid)?;
        validate_compact_identities(&checkpoint, identities)?;
        let mut freeze = PublicationFreeze {
            intent_id,
            checkpoint,
            candidate_digest: [0; 32],
            mutations: Vec::new(),
        };
        self.load_compact_freeze(&mut freeze, identities).await?;
        Ok(freeze)
    }

    pub(crate) fn verify_compact_receipt(
        &self,
        receipt: &CompactRuntimeReceipt,
    ) -> Result<(), RuntimeError> {
        let payload = CompactRuntimeReceipt::signing_bytes(
            receipt.runtime_intent_id(),
            receipt.cleanup_watermark(),
            receipt.commit(),
            receipt.journal_verifier(),
        )
        .map_err(|_| RuntimeError::InvalidCompactReceipt)?;
        let key =
            VerifyingKey::from_bytes(&self.compact_receipt_signing_key.verifying_key().to_bytes())
                .map_err(|_| RuntimeError::CompactReceiptKeyMismatch)?;
        key.verify(&payload, &Signature::from_bytes(receipt.signature()))
            .map_err(|_| RuntimeError::InvalidCompactReceipt)
    }
}
