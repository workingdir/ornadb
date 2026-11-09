//! Typed table mutations: their durable encoding and application to the live relation.

use crate::codec::{capture_tx, validate_digest, validate_id};
use crate::error::{FaultInjector, FaultPoint, RuntimeError};
use crate::lease::WriterLease;
use crate::table_snapshot::{PinnedCommittedRows, RuntimeTableRows};
use crate::{
    RuntimeActivationContext, RuntimeState, TableActivationCandidateValidator, TableSchemaIdentity,
};
use libsql::{Connection, TransactionBehavior, params};
use orna_repository::{
    ContentIdentity, GitHashAlgorithm, NativeOid, OrpBlobBinding, ProtectedContentPin,
    ProtectedContentTransfer,
};
use orna_value::CwdCapture;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mutation {
    pub id: [u8; 16],
    pub payload: Vec<u8>,
    pub digest: [u8; 32],
}

/// Durable, opaque description of one repository-issued protected Blob pin.
/// This record carries evidence into the runtime mutation journal; it is not
/// itself a pin or authority to access Git objects.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtectedContentTransferEvidence {
    repository_id: [u8; 32],
    database_id: [u8; 16],
    pin_id: [u8; 16],
    descriptor_oid: NativeOid,
    identity: ContentIdentity,
}

#[derive(Clone, Debug)]
struct PendingContentPin(Arc<ProtectedContentPin>);

impl PartialEq for PendingContentPin {
    fn eq(&self, other: &Self) -> bool {
        self.0.transfer_record() == other.0.transfer_record()
    }
}

impl Eq for PendingContentPin {}

impl ProtectedContentTransferEvidence {
    pub(crate) fn from_transfer(transfer: &ProtectedContentTransfer) -> Self {
        Self {
            repository_id: *transfer.repository_id(),
            database_id: *transfer.database_id(),
            pin_id: *transfer.pin_id(),
            descriptor_oid: transfer.descriptor_oid().clone(),
            identity: transfer.content_identity(),
        }
    }

    pub const fn repository_id(&self) -> &[u8; 32] {
        &self.repository_id
    }

    pub const fn database_id(&self) -> &[u8; 16] {
        &self.database_id
    }

    pub const fn pin_id(&self) -> &[u8; 16] {
        &self.pin_id
    }

    pub fn descriptor_oid(&self) -> &NativeOid {
        &self.descriptor_oid
    }

    pub const fn content_identity(&self) -> ContentIdentity {
        self.identity
    }
}

const MAX_TABLE_MUTATION_BYTES: usize = 16 * 1024 * 1024;

const MAX_TABLE_CONTENT_TRANSFERS_PER_MUTATION: usize = 4_096;

/// Stable semantic identity for a table, supplied by committed source
/// metadata. It is independent of the table's current source name and schema.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TableObjectId([u8; 16]);

impl TableObjectId {
    #[must_use]
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn bytes(self) -> [u8; 16] {
        self.0
    }
}

/// A table name paired with its committed stable identity for one runtime
/// activation. The name selects the requested relation in this runtime API;
/// `object_id` remains its semantic identity across source renames.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeTableIdentity {
    table: String,
    object_id: TableObjectId,
}

impl RuntimeTableIdentity {
    pub fn new(table: impl Into<String>, object_id: TableObjectId) -> Result<Self, RuntimeError> {
        let table = table.into();
        validate_table_name(&table)?;
        Ok(Self { table, object_id })
    }

    #[must_use]
    pub fn table(&self) -> &str {
        &self.table
    }

    #[must_use]
    pub const fn object_id(&self) -> TableObjectId {
        self.object_id
    }
}

/// A schema-independent durable table change.
///
/// This is the first typed boundary between table execution and the durable
/// runtime.  The runtime persists the canonical table/key/value operation in
/// the same transaction as its mutation ledger and CWD checkpoint; schema
/// admission and evaluator integration remain owned by their respective
/// layers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableMutation {
    id: [u8; 16],
    table: String,
    table_object_id: Option<TableObjectId>,
    key: Vec<u8>,
    value: Option<Vec<u8>>,
    rekey_to: Option<Vec<u8>>,
    insert_only: bool,
    protected_content: Vec<ProtectedContentTransferEvidence>,
    protected_content_from_pins: bool,
    protected_pin_holds: Vec<PendingContentPin>,
}

impl TableMutation {
    pub fn new(
        id: [u8; 16],
        table: impl Into<String>,
        key: Vec<u8>,
        value: Option<Vec<u8>>,
    ) -> Result<Self, RuntimeError> {
        let table = table.into();
        validate_table_mutation(id, &table, &key, value.as_deref())?;
        Ok(Self {
            id,
            table,
            table_object_id: None,
            key,
            value,
            rekey_to: None,
            insert_only: false,
            protected_content: Vec::new(),
            protected_content_from_pins: false,
            protected_pin_holds: Vec::new(),
        })
    }

    /// Creates an insert-only operation. Commit rejects an occupied key
    /// instead of replacing the row as the generic replacement constructor does.
    pub fn insert(
        id: [u8; 16],
        table: impl Into<String>,
        key: Vec<u8>,
        value: Vec<u8>,
    ) -> Result<Self, RuntimeError> {
        let table = table.into();
        validate_table_mutation(id, &table, &key, Some(&value))?;
        Ok(Self {
            id,
            table,
            table_object_id: None,
            key,
            value: Some(value),
            rekey_to: None,
            insert_only: true,
            protected_content: Vec::new(),
            protected_content_from_pins: false,
            protected_pin_holds: Vec::new(),
        })
    }

    /// Creates one explicit primary-key move, retaining the semantic intent
    /// through the durable mutation ledger. The replacement row is supplied
    /// because the application layer owns the admitted key-field schema.
    pub fn rekey(
        id: [u8; 16],
        table: impl Into<String>,
        old_key: Vec<u8>,
        new_key: Vec<u8>,
        replacement: Vec<u8>,
    ) -> Result<Self, RuntimeError> {
        let table = table.into();
        validate_table_mutation(id, &table, &old_key, Some(&replacement))?;
        validate_table_identity(&table, &new_key)?;
        if old_key == new_key {
            return Err(RuntimeError::InvalidTableMutation);
        }
        Ok(Self {
            id,
            table,
            table_object_id: None,
            key: old_key,
            value: Some(replacement),
            rekey_to: Some(new_key),
            insert_only: false,
            protected_content: Vec::new(),
            protected_content_from_pins: false,
            protected_pin_holds: Vec::new(),
        })
    }

    pub fn id(&self) -> [u8; 16] {
        self.id
    }

    pub fn table(&self) -> &str {
        &self.table
    }

    /// Returns the committed semantic table identity when source admission
    /// supplied one. Legacy name-only table admission returns `None`.
    #[must_use]
    pub const fn table_object_id(&self) -> Option<TableObjectId> {
        self.table_object_id
    }

    /// Returns the durable `table_row` key for this mutation. Identity-bearing
    /// rows are addressed by their committed identity, so a source rename keeps
    /// reading and writing the same rows.
    fn storage_table(&self) -> String {
        table_storage_key(&self.table, self.table_object_id)
    }

    /// Carries committed source table identity through the runtime mutation
    /// boundary. The caller owns provenance; this runtime never derives an ID.
    #[must_use]
    pub fn with_table_object_id(mut self, object_id: TableObjectId) -> Self {
        self.table_object_id = Some(object_id);
        self
    }
    /// Replaces runtime key encodings without changing the mutation's semantic intent.
    /// Re-key destinations must be supplied when this mutation already carries one.
    pub fn with_reencoded_keys(
        mut self,
        key: Vec<u8>,
        rekey_to: Option<Vec<u8>>,
    ) -> Result<Self, RuntimeError> {
        if self.rekey_to.is_some() != rekey_to.is_some() {
            return Err(RuntimeError::InvalidTableMutation);
        }
        validate_table_mutation(self.id, &self.table, &key, self.value.as_deref())?;
        if let Some(destination) = &rekey_to {
            validate_table_identity(&self.table, destination)?;
            if *destination == key {
                return Err(RuntimeError::InvalidTableMutation);
            }
        }
        self.key = key;
        self.rekey_to = rekey_to;
        Ok(self)
    }
    /// Mutation identity, table identity, re-key intent, and protected-content evidence are retained.
    pub fn with_reencoded_value(mut self, value: Vec<u8>) -> Result<Self, RuntimeError> {
        if self.value.is_none() {
            return Err(RuntimeError::InvalidTableMutation);
        }
        validate_table_mutation(self.id, &self.table, &self.key, Some(&value))?;
        self.value = Some(value);
        Ok(self)
    }
    pub fn key(&self) -> &[u8] {
        &self.key
    }

    pub fn value(&self) -> Option<&[u8]> {
        self.value.as_deref()
    }

    /// Returns the destination key for an explicit semantic re-key.
    pub fn rekey_to(&self) -> Option<&[u8]> {
        self.rekey_to.as_deref()
    }

    /// Whether this mutation requires its destination key to be absent.
    pub fn is_insert(&self) -> bool {
        self.insert_only
    }

    /// Associates an already protected native Blob with this replacement row.
    /// The durable transfer evidence is encoded into the same pending
    /// mutation as the row. Only a pin issued by the repository graph can be
    /// attached through this API.
    pub fn with_protected_content_pin(
        mut self,
        pin: &ProtectedContentPin,
    ) -> Result<Self, RuntimeError> {
        if self.value.is_none()
            || (!self.protected_content_from_pins && !self.protected_content.is_empty())
            || self.protected_content.len() >= MAX_TABLE_CONTENT_TRANSFERS_PER_MUTATION
        {
            return Err(RuntimeError::InvalidTableMutation);
        }
        let transfer = ProtectedContentTransferEvidence::from_transfer(&pin.transfer_record());
        if transfer.pin_id() == &[0; 16]
            || self
                .protected_content
                .iter()
                .any(|existing| existing.pin_id() == transfer.pin_id())
        {
            return Err(RuntimeError::InvalidTableMutation);
        }
        self.protected_content.push(transfer);
        self.protected_content_from_pins = true;
        Ok(self)
    }

    /// Replaces this row value with the accepted canonical ORP Blob and
    /// retains its native pin through the caller's transaction and retries.
    /// The transfer evidence and descriptor-bearing row value are created
    /// from the same graph-issued binding.
    pub fn with_orp_blob_binding(mut self, binding: OrpBlobBinding) -> Result<Self, RuntimeError> {
        if (!self.protected_content_from_pins && !self.protected_content.is_empty())
            || self.protected_content.len() >= MAX_TABLE_CONTENT_TRANSFERS_PER_MUTATION
        {
            return Err(RuntimeError::InvalidTableMutation);
        }
        let value = binding.encoded_value().to_vec();
        let pin = binding.into_pin();
        let transfer = ProtectedContentTransferEvidence::from_transfer(&pin.transfer_record());
        if transfer.pin_id() == &[0; 16]
            || self
                .protected_content
                .iter()
                .any(|existing| existing.pin_id() == transfer.pin_id())
        {
            return Err(RuntimeError::InvalidTableMutation);
        }
        self.value = Some(value);
        self.protected_content.push(transfer);
        self.protected_content_from_pins = true;
        self.protected_pin_holds
            .push(PendingContentPin(Arc::new(pin)));
        Ok(self)
    }

    /// Opaque transfer evidence retained with this table mutation.
    pub fn protected_content_transfers(&self) -> &[ProtectedContentTransferEvidence] {
        &self.protected_content
    }

    /// Decodes a mutation previously produced by this typed boundary.
    /// Generic runtime mutations are rejected unless their payload, digest,
    /// and canonical table operation all validate together.
    pub fn decode(mutation: &Mutation) -> Result<Self, RuntimeError> {
        validate_id(mutation.id)?;
        if mutation.payload.len() > MAX_TABLE_MUTATION_BYTES {
            return Err(RuntimeError::InvalidTableMutation);
        }
        let digest: [u8; 32] = Sha256::digest(&mutation.payload).into();
        if digest != mutation.digest {
            return Err(RuntimeError::InvalidTableMutation);
        }
        let (mut cursor, version) = if mutation.payload.starts_with(b"ORNA-TABLE-MUTATION-4\0") {
            (b"ORNA-TABLE-MUTATION-4\0".len(), 4_u8)
        } else if mutation.payload.starts_with(b"ORNA-TABLE-MUTATION-3\0") {
            (b"ORNA-TABLE-MUTATION-3\0".len(), 3_u8)
        } else if mutation.payload.starts_with(b"ORNA-TABLE-MUTATION-2\0") {
            (b"ORNA-TABLE-MUTATION-2\0".len(), 2_u8)
        } else if mutation.payload.starts_with(b"ORNA-TABLE-MUTATION\0") {
            (b"ORNA-TABLE-MUTATION\0".len(), 1_u8)
        } else {
            return Err(RuntimeError::InvalidTableMutation);
        };
        let table =
            String::from_utf8(read_length_prefixed(&mutation.payload, &mut cursor)?.to_vec())
                .map_err(|_| RuntimeError::InvalidTableMutation)?;
        let table_object_id = if version == 4 {
            let end = cursor
                .checked_add(16)
                .filter(|end| *end <= mutation.payload.len())
                .ok_or(RuntimeError::InvalidTableMutation)?;
            let bytes = mutation.payload[cursor..end]
                .try_into()
                .map_err(|_| RuntimeError::InvalidTableMutation)?;
            cursor = end;
            Some(TableObjectId::new(bytes))
        } else {
            None
        };
        let key = read_length_prefixed(&mutation.payload, &mut cursor)?.to_vec();
        let operation = if version >= 2 {
            Some(
                *mutation
                    .payload
                    .get(cursor)
                    .ok_or(RuntimeError::InvalidTableMutation)?,
            )
        } else {
            None
        };
        if version >= 2 {
            cursor += 1;
        }
        if operation.is_some_and(|operation| operation > 2) {
            return Err(RuntimeError::InvalidTableMutation);
        }
        let rekey_to = if operation == Some(2) {
            Some(read_length_prefixed(&mutation.payload, &mut cursor)?.to_vec())
        } else {
            None
        };
        let value = match mutation.payload.get(cursor).copied() {
            Some(0) => {
                cursor += 1;
                None
            }
            Some(1) => {
                cursor += 1;
                Some(read_length_prefixed(&mutation.payload, &mut cursor)?.to_vec())
            }
            _ => return Err(RuntimeError::InvalidTableMutation),
        };
        let protected_content = match version {
            3 => decode_protected_content_transfers(&mutation.payload, &mut cursor, false)?,
            4 => decode_protected_content_transfers(&mutation.payload, &mut cursor, true)?,
            _ => Vec::new(),
        };
        if cursor != mutation.payload.len() {
            return Err(RuntimeError::InvalidTableMutation);
        }
        let mut decoded = match (operation, rekey_to) {
            (Some(1), None) => Self::insert(
                mutation.id,
                table,
                key,
                value.ok_or(RuntimeError::InvalidTableMutation)?,
            ),
            (Some(2), Some(new_key)) => Self::rekey(
                mutation.id,
                table,
                key,
                new_key,
                value.ok_or(RuntimeError::InvalidTableMutation)?,
            ),
            (Some(0), None) => Self::new(mutation.id, table, key, value),
            (None, None) => Self::new(mutation.id, table, key, value),
            _ => Err(RuntimeError::InvalidTableMutation),
        }?;
        decoded.table_object_id = table_object_id;
        if protected_content.len() > MAX_TABLE_CONTENT_TRANSFERS_PER_MUTATION
            || (!protected_content.is_empty() && decoded.value.is_none())
            || protected_content
                .iter()
                .enumerate()
                .any(|(index, transfer)| {
                    protected_content[..index]
                        .iter()
                        .any(|previous| previous.pin_id() == transfer.pin_id())
                })
        {
            return Err(RuntimeError::InvalidTableMutation);
        }
        decoded.protected_content = protected_content;
        // Decoding a durable record recovers its evidence for readers, but
        // serialized bytes alone do not authorize a new transaction.
        decoded.protected_content_from_pins = false;
        Ok(decoded)
    }

    pub(crate) fn runtime_mutation(&self) -> Result<Mutation, RuntimeError> {
        if !self.protected_content.is_empty() {
            return Err(RuntimeError::InvalidTableMutation);
        }
        let payload = if self.table_object_id.is_some() {
            encode_v4_table_mutation(self)?
        } else if self.rekey_to.is_some() || self.insert_only {
            encode_v2_table_mutation(self)?
        } else {
            encode_table_mutation(self)?
        };
        Ok(Mutation {
            id: self.id,
            digest: Sha256::digest(&payload).into(),
            payload,
        })
    }

    pub(crate) fn runtime_mutation_with_protected_content(
        &self,
        database_id: &[u8; 16],
    ) -> Result<Mutation, RuntimeError> {
        if self.protected_content.is_empty() {
            return self.runtime_mutation();
        }
        if !self.protected_content_from_pins
            || self.value.is_none()
            || self.protected_content.len() > MAX_TABLE_CONTENT_TRANSFERS_PER_MUTATION
            || self.protected_content.iter().any(|transfer| {
                transfer.database_id() != database_id || transfer.pin_id() == &[0; 16]
            })
            || self
                .protected_content
                .iter()
                .enumerate()
                .any(|(index, transfer)| {
                    self.protected_content[..index]
                        .iter()
                        .any(|previous| previous.pin_id() == transfer.pin_id())
                })
        {
            return Err(RuntimeError::InvalidTableMutation);
        }
        let payload = if self.table_object_id.is_some() {
            encode_v4_table_mutation(self)?
        } else {
            encode_v3_table_mutation(self)?
        };
        Ok(Mutation {
            id: self.id,
            digest: Sha256::digest(&payload).into(),
            payload,
        })
    }
}

/// An accepted content pin the runtime recorded for a descriptor. It names
/// candidate content only; admission through the graph is what verifies it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptedContentPin {
    pub(crate) pin_id: [u8; 16],
    pub(crate) content_length: u64,
    pub(crate) content_sha256: [u8; 32],
}

impl AcceptedContentPin {
    pub const fn pin_id(&self) -> &[u8; 16] {
        &self.pin_id
    }

    pub const fn content_length(&self) -> u64 {
        self.content_length
    }

    pub const fn content_sha256(&self) -> &[u8; 32] {
        &self.content_sha256
    }
}

/// Tables written by a validated boundary; dependency-scoped validators skip
/// propositions whose dependencies none of these tables touch (ORNA-ASSERT-026).
pub(crate) fn mutated_tables(mutations: &[TableMutation]) -> BTreeSet<String> {
    mutations
        .iter()
        .map(|mutation| mutation.table().to_owned())
        .collect()
}

pub(crate) fn validate_mutations(
    mutations: &[Mutation],
    next_digest: [u8; 32],
) -> Result<(), RuntimeError> {
    if mutations.is_empty() {
        return Err(RuntimeError::EmptyMutationBatch);
    }
    validate_stream_mutations(mutations, next_digest)
}

pub(crate) fn validate_stream_mutations(
    mutations: &[Mutation],
    next_digest: [u8; 32],
) -> Result<(), RuntimeError> {
    for mutation in mutations {
        validate_id(mutation.id)?;
        validate_digest(mutation.digest)?;
    }
    validate_digest(next_digest)
}

/// Empty keys are admitted: the only producer of an empty stored key is the
/// text key `""` (the VFS-1 `~empty` projection). Canonical keys of every other
/// key type are never empty; the project layer validates key shape before it
/// reaches the runtime.
fn validate_table_identity(table: &str, key: &[u8]) -> Result<(), RuntimeError> {
    validate_table_name(table)?;
    if key.len() > MAX_TABLE_MUTATION_BYTES {
        return Err(RuntimeError::InvalidTableMutation);
    }
    Ok(())
}

/// Maps a table name plus its optional committed identity to the `table_row`
/// key. Name-only tables keep their legacy key; identity-bearing tables use a
/// prefix that no source name can produce, because names never contain `:`.
pub(crate) fn table_storage_key(name: &str, object_id: Option<TableObjectId>) -> String {
    match object_id {
        Some(object_id) => {
            let hex: String = object_id
                .bytes()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            format!("oid:{hex}")
        }
        None => name.to_owned(),
    }
}

/// Rejects a mutation whose identity disagrees with the identity its
/// activation snapshot or candidate validator was admitted under. Without this
/// check a write could land under a key the admitted reader never observes.
pub(crate) fn validate_mutation_identities(
    mutations: &[TableMutation],
    admitted: &BTreeMap<String, TableObjectId>,
) -> Result<(), RuntimeError> {
    if mutations
        .iter()
        .any(|mutation| mutation.table_object_id() != admitted.get(mutation.table()).copied())
    {
        return Err(RuntimeError::InvalidTableMutation);
    }
    Ok(())
}

pub(crate) fn validate_table_name(table: &str) -> Result<(), RuntimeError> {
    if table.is_empty() || table.len() > MAX_TABLE_MUTATION_BYTES || table.contains('\0') {
        return Err(RuntimeError::InvalidTableMutation);
    }
    Ok(())
}

fn validate_table_mutation(
    id: [u8; 16],
    table: &str,
    key: &[u8],
    value: Option<&[u8]>,
) -> Result<(), RuntimeError> {
    validate_id(id)?;
    validate_table_identity(table, key)?;
    if value.is_some_and(|value| value.len() > MAX_TABLE_MUTATION_BYTES) {
        return Err(RuntimeError::InvalidTableMutation);
    }
    Ok(())
}

fn encode_table_mutation(mutation: &TableMutation) -> Result<Vec<u8>, RuntimeError> {
    let mut payload = Vec::new();
    payload.extend_from_slice(b"ORNA-TABLE-MUTATION\0");
    append_length_prefixed(&mut payload, mutation.table.as_bytes())?;
    append_length_prefixed(&mut payload, &mutation.key)?;
    match &mutation.value {
        Some(value) => {
            payload.push(1);
            append_length_prefixed(&mut payload, value)?;
        }
        None => payload.push(0),
    }
    if payload.len() > MAX_TABLE_MUTATION_BYTES {
        return Err(RuntimeError::InvalidTableMutation);
    }
    Ok(payload)
}

fn encode_v2_table_mutation(mutation: &TableMutation) -> Result<Vec<u8>, RuntimeError> {
    let mut payload = Vec::new();
    payload.extend_from_slice(b"ORNA-TABLE-MUTATION-2\0");
    append_length_prefixed(&mut payload, mutation.table().as_bytes())?;
    append_length_prefixed(&mut payload, mutation.key())?;
    if let Some(new_key) = mutation.rekey_to() {
        payload.push(2);
        append_length_prefixed(&mut payload, new_key)?;
    } else if mutation.is_insert() {
        payload.push(1);
    } else {
        return Err(RuntimeError::InvalidTableMutation);
    }
    payload.push(1);
    append_length_prefixed(
        &mut payload,
        mutation.value().ok_or(RuntimeError::InvalidTableMutation)?,
    )?;
    if payload.len() > MAX_TABLE_MUTATION_BYTES {
        return Err(RuntimeError::InvalidTableMutation);
    }
    Ok(payload)
}

fn encode_v3_table_mutation(mutation: &TableMutation) -> Result<Vec<u8>, RuntimeError> {
    if mutation.value.is_none()
        || mutation.protected_content.is_empty()
        || mutation.protected_content.len() > MAX_TABLE_CONTENT_TRANSFERS_PER_MUTATION
    {
        return Err(RuntimeError::InvalidTableMutation);
    }
    let mut payload = Vec::new();
    payload.extend_from_slice(b"ORNA-TABLE-MUTATION-3\0");
    append_length_prefixed(&mut payload, mutation.table().as_bytes())?;
    append_length_prefixed(&mut payload, mutation.key())?;
    if let Some(new_key) = mutation.rekey_to() {
        payload.push(2);
        append_length_prefixed(&mut payload, new_key)?;
    } else if mutation.is_insert() {
        payload.push(1);
    } else {
        payload.push(0);
    }
    payload.push(1);
    append_length_prefixed(
        &mut payload,
        mutation.value().ok_or(RuntimeError::InvalidTableMutation)?,
    )?;
    payload.extend_from_slice(
        &u32::try_from(mutation.protected_content.len())
            .map_err(|_| RuntimeError::InvalidTableMutation)?
            .to_be_bytes(),
    );
    for transfer in &mutation.protected_content {
        payload.extend_from_slice(transfer.repository_id());
        payload.extend_from_slice(transfer.database_id());
        payload.extend_from_slice(transfer.pin_id());
        payload.push(match transfer.descriptor_oid().algorithm() {
            GitHashAlgorithm::Sha1 => 1,
            GitHashAlgorithm::Sha256 => 2,
        });
        let identity = transfer.content_identity();
        payload.extend_from_slice(&identity.length().to_be_bytes());
        payload.extend_from_slice(&identity.sha256());
        payload.extend_from_slice(transfer.descriptor_oid().as_bytes());
    }
    if payload.len() > MAX_TABLE_MUTATION_BYTES {
        return Err(RuntimeError::InvalidTableMutation);
    }
    Ok(payload)
}

fn encode_v4_table_mutation(mutation: &TableMutation) -> Result<Vec<u8>, RuntimeError> {
    if mutation.protected_content.len() > MAX_TABLE_CONTENT_TRANSFERS_PER_MUTATION
        || (!mutation.protected_content.is_empty() && mutation.value.is_none())
    {
        return Err(RuntimeError::InvalidTableMutation);
    }
    let mut payload = Vec::new();
    payload.extend_from_slice(b"ORNA-TABLE-MUTATION-4\0");
    append_length_prefixed(&mut payload, mutation.table().as_bytes())?;
    payload.extend_from_slice(
        &mutation
            .table_object_id
            .ok_or(RuntimeError::InvalidTableMutation)?
            .bytes(),
    );
    append_length_prefixed(&mut payload, mutation.key())?;
    if let Some(new_key) = mutation.rekey_to() {
        payload.push(2);
        append_length_prefixed(&mut payload, new_key)?;
    } else if mutation.is_insert() {
        payload.push(1);
    } else {
        payload.push(0);
    }
    match mutation.value() {
        Some(value) => {
            payload.push(1);
            append_length_prefixed(&mut payload, value)?;
        }
        None => payload.push(0),
    }
    payload.extend_from_slice(
        &u32::try_from(mutation.protected_content.len())
            .map_err(|_| RuntimeError::InvalidTableMutation)?
            .to_be_bytes(),
    );
    for transfer in &mutation.protected_content {
        payload.extend_from_slice(transfer.repository_id());
        payload.extend_from_slice(transfer.database_id());
        payload.extend_from_slice(transfer.pin_id());
        payload.push(match transfer.descriptor_oid().algorithm() {
            GitHashAlgorithm::Sha1 => 1,
            GitHashAlgorithm::Sha256 => 2,
        });
        let identity = transfer.content_identity();
        payload.extend_from_slice(&identity.length().to_be_bytes());
        payload.extend_from_slice(&identity.sha256());
        payload.extend_from_slice(transfer.descriptor_oid().as_bytes());
    }
    if payload.len() > MAX_TABLE_MUTATION_BYTES {
        return Err(RuntimeError::InvalidTableMutation);
    }
    Ok(payload)
}

fn decode_protected_content_transfers(
    payload: &[u8],
    cursor: &mut usize,
    allow_empty: bool,
) -> Result<Vec<ProtectedContentTransferEvidence>, RuntimeError> {
    let count_bytes = take_fixed::<4>(payload, cursor)?;
    let count = usize::try_from(u32::from_be_bytes(count_bytes))
        .map_err(|_| RuntimeError::InvalidTableMutation)?;
    if (!allow_empty && count == 0) || count > MAX_TABLE_CONTENT_TRANSFERS_PER_MUTATION {
        return Err(RuntimeError::InvalidTableMutation);
    }
    let mut transfers = Vec::with_capacity(count);
    for _ in 0..count {
        let repository_id = take_fixed::<32>(payload, cursor)?;
        let database_id = take_fixed::<16>(payload, cursor)?;
        let pin_id = take_fixed::<16>(payload, cursor)?;
        if pin_id == [0; 16] {
            return Err(RuntimeError::InvalidTableMutation);
        }
        let algorithm = match take_fixed::<1>(payload, cursor)?[0] {
            1 => GitHashAlgorithm::Sha1,
            2 => GitHashAlgorithm::Sha256,
            _ => return Err(RuntimeError::InvalidTableMutation),
        };
        let length = u64::from_be_bytes(take_fixed::<8>(payload, cursor)?);
        let identity_sha256 = take_fixed::<32>(payload, cursor)?;
        let oid_bytes = match algorithm {
            GitHashAlgorithm::Sha1 => take_fixed::<20>(payload, cursor)?.to_vec(),
            GitHashAlgorithm::Sha256 => take_fixed::<32>(payload, cursor)?.to_vec(),
        };
        let descriptor_oid =
            NativeOid::new(algorithm, oid_bytes).map_err(|_| RuntimeError::InvalidTableMutation)?;
        let identity = ContentIdentity::new(length, identity_sha256)
            .map_err(|_| RuntimeError::InvalidTableMutation)?;
        let transfer = ProtectedContentTransferEvidence {
            repository_id,
            database_id,
            pin_id,
            descriptor_oid,
            identity,
        };
        if transfers
            .iter()
            .any(|previous: &ProtectedContentTransferEvidence| {
                previous.pin_id() == transfer.pin_id()
            })
        {
            return Err(RuntimeError::InvalidTableMutation);
        }
        transfers.push(transfer);
    }
    Ok(transfers)
}

fn take_fixed<const N: usize>(payload: &[u8], cursor: &mut usize) -> Result<[u8; N], RuntimeError> {
    let end = cursor
        .checked_add(N)
        .ok_or(RuntimeError::InvalidTableMutation)?;
    let bytes: [u8; N] = payload
        .get(*cursor..end)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(RuntimeError::InvalidTableMutation)?;
    *cursor = end;
    Ok(bytes)
}

fn append_length_prefixed(target: &mut Vec<u8>, value: &[u8]) -> Result<(), RuntimeError> {
    target.extend_from_slice(
        &u32::try_from(value.len())
            .map_err(|_| RuntimeError::InvalidTableMutation)?
            .to_be_bytes(),
    );
    target.extend_from_slice(value);
    Ok(())
}

fn read_length_prefixed<'a>(
    payload: &'a [u8],
    cursor: &mut usize,
) -> Result<&'a [u8], RuntimeError> {
    let length = payload
        .get(*cursor..(*cursor).saturating_add(4))
        .and_then(|bytes| bytes.try_into().ok())
        .map(u32::from_be_bytes)
        .ok_or(RuntimeError::InvalidTableMutation)?;
    *cursor = cursor
        .checked_add(4)
        .ok_or(RuntimeError::InvalidTableMutation)?;
    let end = (*cursor)
        .checked_add(usize::try_from(length).map_err(|_| RuntimeError::InvalidTableMutation)?)
        .ok_or(RuntimeError::InvalidTableMutation)?;
    let bytes = payload
        .get(*cursor..end)
        .ok_or(RuntimeError::InvalidTableMutation)?;
    *cursor = end;
    Ok(bytes)
}

/// Committed `HEAD` rows of one storage table, borrowed from the pinned base,
/// and the keys whose live `table_row` state now governs.
struct CommittedTable<'a> {
    /// Committed value of each key at the published pin.
    rows: &'a BTreeMap<Vec<u8>, Vec<u8>>,
    /// Keys mutated after the pin, by the unpublished tail or by the batch in
    /// progress. The committed value of such a key no longer applies.
    overridden: BTreeSet<Vec<u8>>,
}

/// Committed `HEAD` rows an activation or commit reads beneath the live
/// `table_row` state, keyed by storage table. `None` binds no committed base:
/// the live `table_row` state is the whole relation.
#[derive(Default)]
pub(crate) struct CommittedTableBase<'a> {
    tables: Option<BTreeMap<String, CommittedTable<'a>>>,
}

/// The committed rows of a relation that has none at the pin.
static NO_COMMITTED_ROWS: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();

impl<'a> CommittedTableBase<'a> {
    /// Binds the pinned committed rows of each target and overrides every key
    /// mutated after the published pin. Fails closed with `StaleCapture` when the
    /// pin no longer matches the published checkpoint.
    pub(crate) async fn load_tx<'t>(
        connection: &Connection,
        committed: Option<&'a PinnedCommittedRows>,
        targets: impl IntoIterator<Item = (&'t str, Option<TableObjectId>)>,
    ) -> Result<Self, RuntimeError> {
        let Some(committed) = committed else {
            return Ok(Self::default());
        };
        let sequence = published_mutation_sequence_tx(connection).await?;
        if sequence != committed.pin.mutation_sequence {
            return Err(RuntimeError::StaleCapture {
                current: Box::new(capture_tx(connection).await?),
            });
        }
        let mut tables: BTreeMap<String, CommittedTable<'a>> = BTreeMap::new();
        for (table, object_id) in targets {
            let storage = table_storage_key(table, object_id);
            if tables.contains_key(&storage) {
                continue;
            }
            let mut overridden = BTreeSet::new();
            let mut tail = connection
                .query(
                    "SELECT DISTINCT row_key FROM runtime_table_history
                     WHERE table_id = ?1 AND mutation_sequence > ?2",
                    params![storage.clone(), sequence],
                )
                .await?;
            while let Some(row) = tail.next().await? {
                let key: Vec<u8> = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
                overridden.insert(key);
            }
            let rows = committed.tables.get(table).unwrap_or(&NO_COMMITTED_ROWS);
            tables.insert(storage, CommittedTable { rows, overridden });
        }
        Ok(Self {
            tables: Some(tables),
        })
    }

    /// Marks `key` of `storage` as governed by `table_row` from now on.
    fn override_key(&mut self, storage: &str, key: &[u8]) {
        if let Some(table) = self
            .tables
            .as_mut()
            .and_then(|tables| tables.get_mut(storage))
        {
            table.overridden.insert(key.to_vec());
        }
    }

    /// Whether a live `table_row` key of `storage` belongs to the relation. With
    /// no base bound every live row does. Beneath a base only keys the tail
    /// overrode do, so a live row never shadows an untouched committed key.
    fn includes_live_row(&self, storage: &str, key: &[u8]) -> bool {
        match &self.tables {
            None => true,
            Some(tables) => tables
                .get(storage)
                .is_some_and(|table| table.overridden.contains(key)),
        }
    }

    /// Whether a committed row `key` of `storage` is untouched since the pin.
    fn committed_visible(&self, storage: &str, key: &[u8]) -> bool {
        self.tables
            .as_ref()
            .and_then(|tables| tables.get(storage))
            .is_some_and(|table| table.rows.contains_key(key) && !table.overridden.contains(key))
    }
}

/// Mutation sequence of the newest published checkpoint; zero before any
/// publication has committed.
pub(crate) async fn published_mutation_sequence_tx(
    connection: &Connection,
) -> Result<i64, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT COALESCE(MAX(publication_freeze.checkpoint_mutation_sequence), 0)
             FROM publication_freeze
             JOIN publication_commit
               ON publication_commit.intent_id = publication_freeze.intent_id",
            (),
        )
        .await?;
    let row = rows.next().await?.ok_or(RuntimeError::RecoveryInvalid)?;
    row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)
}

/// Whether `key` is live in `storage`: present in `table_row`, or a committed
/// base row that no mutation has touched since the pin.
async fn row_present_tx(
    connection: &Connection,
    storage: &str,
    key: &[u8],
    base: &CommittedTableBase<'_>,
) -> Result<bool, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT 1 FROM table_row WHERE table_id = ?1 AND row_key = ?2 LIMIT 1",
            params![storage.to_owned(), key.to_vec()],
        )
        .await?;
    if rows.next().await?.is_some() {
        return Ok(true);
    }
    Ok(base.committed_visible(storage, key))
}

/// Applies one table mutation to the live rows and records the keys it
/// overrides beneath the committed base.
pub(crate) async fn apply_table_mutation_tx(
    connection: &Connection,
    mutation: &TableMutation,
    base: &mut CommittedTableBase<'_>,
) -> Result<(), RuntimeError> {
    if let Some(identity) = mutation.table_object_id() {
        // Rows written under a committed identity live under `oid:<hex>`, so
        // the name is recorded here for readers that address the table by name
        // (bead ornadb-jm0d).
        connection
            .execute(
                "INSERT OR IGNORE INTO table_identity (table_name, object_id) VALUES (?1, ?2)",
                params![mutation.table().to_owned(), identity.bytes().to_vec()],
            )
            .await?;
    }
    apply_table_row_tx(connection, mutation, base).await?;
    let storage_table = mutation.storage_table();
    base.override_key(&storage_table, mutation.key());
    if let Some(new_key) = mutation.rekey_to() {
        base.override_key(&storage_table, new_key);
    }
    Ok(())
}

async fn apply_table_row_tx(
    connection: &Connection,
    mutation: &TableMutation,
    base: &CommittedTableBase<'_>,
) -> Result<(), RuntimeError> {
    let storage_table = mutation.storage_table();
    if let Some(new_key) = mutation.rekey_to() {
        if !row_present_tx(connection, &storage_table, mutation.key(), base).await? {
            return Err(RuntimeError::InvalidTableMutation);
        }
        if row_present_tx(connection, &storage_table, new_key, base).await? {
            return Err(RuntimeError::InvalidTableMutation);
        }
        let value = mutation.value().ok_or(RuntimeError::InvalidTableMutation)?;
        let digest: [u8; 32] = Sha256::digest(value).into();
        connection
            .execute(
                "DELETE FROM table_row WHERE table_id = ?1 AND row_key = ?2",
                params![storage_table.clone(), mutation.key().to_vec()],
            )
            .await?;
        connection
            .execute(
                "INSERT INTO table_row (table_id, row_key, row_value, row_digest)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    storage_table.clone(),
                    new_key.to_vec(),
                    value.to_vec(),
                    digest.to_vec()
                ],
            )
            .await?;
        return Ok(());
    }
    if mutation.is_insert() {
        let value = mutation.value().ok_or(RuntimeError::InvalidTableMutation)?;
        if row_present_tx(connection, &storage_table, mutation.key(), base).await? {
            return Err(RuntimeError::InvalidTableMutation);
        }
        let digest: [u8; 32] = Sha256::digest(value).into();
        connection
            .execute(
                "INSERT INTO table_row (table_id, row_key, row_value, row_digest)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    storage_table.clone(),
                    mutation.key().to_vec(),
                    value.to_vec(),
                    digest.to_vec()
                ],
            )
            .await?;
        return Ok(());
    }
    match &mutation.value {
        Some(value) => {
            let digest: [u8; 32] = Sha256::digest(value).into();
            connection
                .execute(
                    "INSERT INTO table_row (table_id, row_key, row_value, row_digest)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(table_id, row_key) DO UPDATE SET
                       row_value = excluded.row_value,
                       row_digest = excluded.row_digest",
                    params![
                        storage_table.clone(),
                        mutation.key.clone(),
                        value.clone(),
                        digest.to_vec()
                    ],
                )
                .await?;
        }
        None => {
            // Rows are values; mutations are operations on tables
            // (ORNA-MUT-001). A delete therefore records that the row is
            // absent at this generation rather than asserting that it was
            // present: deleting an already-absent row removes nothing and is a
            // tolerated no-op here. The durable mutation, its pending evidence
            // and the generation advance are recorded by the caller, so an
            // `as_of` read observes exactly the delete this activation made.
            // The language surface still rejects `T.delete` on a missing key
            // before it reaches this boundary.
            connection
                .execute(
                    "DELETE FROM table_row WHERE table_id = ?1 AND row_key = ?2",
                    params![storage_table.clone(), mutation.key.clone()],
                )
                .await?;
        }
    }
    Ok(())
}

/// Loads the committed base beneath one candidate: the relations its mutations
/// address and the relations its validator reads.
pub(crate) async fn load_candidate_base_tx<'a>(
    connection: &Connection,
    committed: Option<&'a PinnedCommittedRows>,
    mutations: &[TableMutation],
    validator: Option<&dyn TableActivationCandidateValidator>,
) -> Result<CommittedTableBase<'a>, RuntimeError> {
    let validator_ids = validator
        .map(|validator| validator.table_object_ids())
        .unwrap_or_default();
    let validator_tables: &[String] = match validator {
        Some(validator) => validator.tables(),
        None => &[],
    };
    CommittedTableBase::load_tx(
        connection,
        committed,
        mutations
            .iter()
            .map(|mutation| (mutation.table(), mutation.table_object_id()))
            .chain(
                validator_tables
                    .iter()
                    .map(|table| (table.as_str(), validator_ids.get(table).copied())),
            ),
    )
    .await
}

pub(crate) async fn table_rows_tx(
    connection: &Connection,
    tables: &[String],
    table_object_ids: &BTreeMap<String, TableObjectId>,
    base: &CommittedTableBase<'_>,
) -> Result<RuntimeTableRows, RuntimeError> {
    let mut result = BTreeMap::new();
    for table in tables {
        validate_table_name(table)?;
        if result.contains_key(table) {
            continue;
        }
        let storage = table_storage_key(table, table_object_ids.get(table).copied());
        // Beneath a committed base the relation is the committed `HEAD` rows no
        // mutation touched plus the live rows the tail overrode. With no base the
        // live `table_row` state is the whole relation.
        let mut rows_by_key: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
        if let Some(committed) = base.tables.as_ref().and_then(|tables| tables.get(&storage)) {
            for (key, value) in committed.rows {
                if !committed.overridden.contains(key) {
                    rows_by_key.insert(key.clone(), value.clone());
                }
            }
        }
        let mut query = connection
            .query(
                "SELECT row_key, row_value FROM table_row
                 WHERE table_id = ?1 ORDER BY row_key",
                params![storage.clone()],
            )
            .await?;
        while let Some(row) = query.next().await? {
            let key: Vec<u8> = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let value: Vec<u8> = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
            if base.includes_live_row(&storage, &key) {
                rows_by_key.insert(key, value);
            }
        }
        result.insert(table.clone(), rows_by_key.into_iter().collect());
    }
    Ok(result)
}

/// Returns the distinct declared names of a set of admitted table identities.
///
/// Duplicate names or duplicate committed identities fail closed: two
/// relations differing only by identity or only by name would otherwise alias
/// one storage key or one lookup entry.
pub(crate) fn admitted_table_names(
    tables: &[RuntimeTableIdentity],
) -> Result<Vec<String>, RuntimeError> {
    let mut names = BTreeSet::new();
    let mut object_ids = BTreeSet::new();
    for table in tables {
        if !names.insert(table.table.clone()) || !object_ids.insert(table.object_id) {
            return Err(RuntimeError::InvalidTableMutation);
        }
    }
    Ok(names.into_iter().collect())
}

/// Maps admitted table names to their committed identity, validating every
/// name and rejecting a repeated name.
pub(crate) fn admitted_table_object_ids(
    tables: &[RuntimeTableIdentity],
) -> Result<BTreeMap<String, TableObjectId>, RuntimeError> {
    let mut table_object_ids = BTreeMap::new();
    for table in tables {
        validate_table_name(&table.table)?;
        if table_object_ids
            .insert(table.table.clone(), table.object_id)
            .is_some()
        {
            return Err(RuntimeError::InvalidTableMutation);
        }
    }
    Ok(table_object_ids)
}

pub(crate) fn validate_table_candidate_scope(
    mutations: &[TableMutation],
    tables: &[String],
    table_object_ids: &BTreeMap<String, TableObjectId>,
) -> Result<(), RuntimeError> {
    if mutations
        .iter()
        .any(|mutation| !tables.iter().any(|table| table == mutation.table()))
    {
        return Err(RuntimeError::RecoveryInvalid);
    }
    validate_mutation_identities(mutations, table_object_ids)
}

/// Compares the validator's live table schema identity with the identity a
/// save was admitted under (ORNA-VFS-019). Callers run it inside the writer
/// transaction; a mismatch is `StaleTableSchema`, and the caller's early return
/// drops the transaction so no row or checkpoint is published.
pub(crate) fn require_live_table_schema(
    validator: &mut dyn TableActivationCandidateValidator,
    expected_schema: Option<&TableSchemaIdentity>,
) -> Result<(), RuntimeError> {
    if validator.schema_identity().as_ref() != expected_schema {
        return Err(RuntimeError::StaleTableSchema);
    }
    Ok(())
}

/// Requires every mutation for an identity-admitted table to address that
/// exact committed table identity. Name-only mutations for such a table, and
/// mutations carrying a different identity, fail before any row is applied.
/// Tables that were not admitted with an identity keep the name-addressed
/// behavior of the activation they began under.
pub(crate) fn validate_admitted_table_identities(
    context: &RuntimeActivationContext,
    mutations: &[TableMutation],
) -> Result<(), RuntimeError> {
    for mutation in mutations {
        if let Some(admitted) = context.table_object_ids().get(mutation.table())
            && mutation.table_object_id() != Some(*admitted)
        {
            return Err(RuntimeError::InvalidTableMutation);
        }
    }
    Ok(())
}

/// Reports whether `mutations` are an exact replay of records the relation
/// already holds. A mutation's record is written in the same transaction that
/// applies its row effect, so an identical retained record proves that effect
/// is present. A batch that is only partly retained, or retained with different
/// bytes, reuses a mutation identity and is invalid.
pub(crate) async fn table_mutations_already_durable_tx(
    connection: &Connection,
    mutations: &[Mutation],
) -> Result<bool, RuntimeError> {
    let mut retained = 0_usize;
    for mutation in mutations {
        let mut rows = connection
            .query(
                "SELECT digest FROM admitted_mutation WHERE mutation_id = ?1",
                params![mutation.id.to_vec()],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            continue;
        };
        // The retained digest must commit to the payload being replayed: an
        // identical digest over different bytes is a reuse, not a replay.
        let payload_digest: [u8; 32] = Sha256::digest(&mutation.payload).into();
        if payload_digest != mutation.digest {
            return Err(RuntimeError::InvalidTableMutation);
        }
        let digest: Vec<u8> = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
        if digest != mutation.digest {
            return Err(RuntimeError::InvalidTableMutation);
        }
        retained += 1;
    }
    match retained {
        0 => Ok(false),
        count if count == mutations.len() => Ok(true),
        _ => Err(RuntimeError::InvalidTableMutation),
    }
}

pub(crate) async fn append_mutations_tx(
    connection: &Connection,
    expected: &CwdCapture,
    mutations: &[Mutation],
    next_digest: [u8; 32],
    faults: &dyn FaultInjector,
) -> Result<CwdCapture, RuntimeError> {
    append_mutations_with_catalogue_tx(connection, expected, mutations, next_digest, faults, true)
        .await
}

pub(crate) async fn append_mutations_with_catalogue_tx(
    connection: &Connection,
    expected: &CwdCapture,
    mutations: &[Mutation],
    next_digest: [u8; 32],
    faults: &dyn FaultInjector,
    carry_catalogue_forward: bool,
) -> Result<CwdCapture, RuntimeError> {
    let current = capture_tx(connection).await?;
    if &current != expected {
        return Err(RuntimeError::StaleCapture {
            current: Box::new(current),
        });
    }
    let mut sequence: Option<i64> = None;
    let generation = current
        .generation()
        .to_u64_digits()
        .1
        .first()
        .copied()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or(RuntimeError::RecoveryInvalid)?;
    // Scan the whole batch before the first write: a mixed batch that carries a
    // retained identity must leave no pending or admitted row behind. A retained
    // id here is id reuse (an exact replay was short-circuited by the caller),
    // reported as a caller error, not as storage failure (ORNA-VFS-008).
    for mutation in mutations {
        let mut retained_rows = connection
            .query(
                "SELECT 1 FROM admitted_mutation WHERE mutation_id = ?1",
                params![mutation.id.to_vec()],
            )
            .await?;
        if retained_rows.next().await?.is_some() {
            return Err(RuntimeError::InvalidTableMutation);
        }
    }
    for mutation in mutations {
        connection
            .execute(
                "INSERT INTO pending_mutation (mutation_id, payload, digest) VALUES (?1, ?2, ?3)",
                params![
                    mutation.id.to_vec(),
                    mutation.payload.clone(),
                    mutation.digest.to_vec()
                ],
            )
            .await?;
        connection
            .execute(
                "INSERT INTO admitted_mutation (mutation_id, digest) VALUES (?1, ?2)",
                params![mutation.id.to_vec(), mutation.digest.to_vec()],
            )
            .await?;
        let mut rows = connection.query("SELECT last_insert_rowid()", ()).await?;
        sequence = Some(
            rows.next()
                .await?
                .ok_or(RuntimeError::RecoveryInvalid)?
                .get(0)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        );
        // Every table-mutation encoding, including identity-bearing v4, must
        // enter history so `as_of` reads the same rows the live relation holds.
        if mutation.payload.starts_with(b"ORNA-TABLE-MUTATION\0")
            || mutation.payload.starts_with(b"ORNA-TABLE-MUTATION-2\0")
            || mutation.payload.starts_with(b"ORNA-TABLE-MUTATION-3\0")
            || mutation.payload.starts_with(b"ORNA-TABLE-MUTATION-4\0")
        {
            let table_mutation = TableMutation::decode(mutation)?;
            let sequence = sequence.ok_or(RuntimeError::RecoveryInvalid)?;
            if let Some(new_key) = table_mutation.rekey_to() {
                connection
                    .execute(
                        "INSERT INTO runtime_table_history
                         (mutation_sequence, table_id, row_key, row_value, row_digest, deleted)
                         VALUES (?1, ?2, ?3, NULL, NULL, 1)",
                        params![
                            sequence,
                            table_mutation.storage_table(),
                            table_mutation.key()
                        ],
                    )
                    .await?;
                let value = table_mutation
                    .value()
                    .ok_or(RuntimeError::RecoveryInvalid)?;
                connection
                    .execute(
                        "INSERT INTO runtime_table_history
                         (mutation_sequence, table_id, row_key, row_value, row_digest, deleted)
                         VALUES (?1, ?2, ?3, ?4, ?5, 0)",
                        params![
                            sequence,
                            table_mutation.storage_table(),
                            new_key,
                            value,
                            Sha256::digest(value).to_vec()
                        ],
                    )
                    .await?;
            } else {
                let (value, row_digest, deleted) = match table_mutation.value() {
                    Some(value) => (
                        Some(value.to_vec()),
                        Some(Sha256::digest(value).to_vec()),
                        0_i64,
                    ),
                    None => (None, None, 1_i64),
                };
                connection
                    .execute(
                        "INSERT INTO runtime_table_history
                         (mutation_sequence, table_id, row_key, row_value, row_digest, deleted)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![
                            sequence,
                            table_mutation.storage_table(),
                            table_mutation.key().to_vec(),
                            value,
                            row_digest,
                            deleted,
                        ],
                    )
                    .await?;
            }
        }
    }
    faults.check(FaultPoint::AfterMutation)?;
    let sequence = sequence.ok_or(RuntimeError::RecoveryInvalid)?;
    connection
        .execute(
            "INSERT INTO checkpoint (generation, digest, mutation_sequence) VALUES (?1, ?2, ?3)",
            params![
                i64::try_from(generation).map_err(|_| RuntimeError::RecoveryInvalid)?,
                next_digest.to_vec(),
                sequence
            ],
        )
        .await?;
    connection
        .execute(
            "INSERT INTO runtime_history_governance_boundary
             (generation, admin_audit_sequence, checkpoint_reset_audit_sequence)
             SELECT ?1,
                    COALESCE((SELECT MAX(sequence) FROM admin_invocation_audit), 0),
                    COALESCE((SELECT MAX(sequence) FROM stream_checkpoint_reset_audit), 0)",
            params![i64::try_from(generation).map_err(|_| RuntimeError::RecoveryInvalid)?],
        )
        .await?;
    faults.check(FaultPoint::AfterCheckpoint)?;
    connection
        .execute(
            "UPDATE runtime_meta SET generation = ?1, generation_digest = ?2 WHERE singleton = 1",
            params![
                i64::try_from(generation).map_err(|_| RuntimeError::RecoveryInvalid)?,
                next_digest.to_vec()
            ],
        )
        .await?;
    faults.check(FaultPoint::AfterCapture)?;
    let capture = capture_tx(connection).await?;
    if carry_catalogue_forward {
        crate::catalogue::persist_capture_for_runtime_tx(connection, &current, &capture).await?;
    }
    Ok(capture)
}

#[derive(Clone)]
pub(crate) struct AcceptedContentPinManifestEntry {
    pub(crate) pin_id: [u8; 16],
    pub(crate) repository_id: [u8; 32],
    pub(crate) database_id: [u8; 16],
    pub(crate) descriptor_oid_algorithm: i64,
    pub(crate) descriptor_oid: Vec<u8>,
    pub(crate) content_length: i64,
    pub(crate) content_sha256: [u8; 32],
    pub(crate) owner_id: [u8; 16],
    pub(crate) owner_epoch: i64,
    pub(crate) checkpoint_generation: i64,
}

pub(crate) fn accepted_content_pin_manifest_digest(
    entries: &[AcceptedContentPinManifestEntry],
) -> [u8; 32] {
    let mut ordered = entries.to_vec();
    ordered.sort_by_key(|entry| entry.pin_id);
    let mut digest = Sha256::new();
    digest.update(b"orna.runtime.accepted-content-pin-manifest.v1\0");
    digest.update(
        u64::try_from(ordered.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    for entry in ordered {
        digest.update(entry.pin_id);
        digest.update(entry.repository_id);
        digest.update(entry.database_id);
        digest.update(entry.descriptor_oid_algorithm.to_be_bytes());
        digest.update(
            u64::try_from(entry.descriptor_oid.len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        digest.update(entry.descriptor_oid);
        digest.update(entry.content_length.to_be_bytes());
        digest.update(entry.content_sha256);
        digest.update(entry.owner_id);
        digest.update(entry.owner_epoch.to_be_bytes());
        digest.update(entry.checkpoint_generation.to_be_bytes());
    }
    digest.finalize().into()
}

impl RuntimeState {
    /// Reads a committed table row after reopening the durable runtime.
    pub async fn committed_table_row(
        &self,
        table: &str,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, RuntimeError> {
        validate_table_identity(table, key)?;
        let mut rows = self
            .connection
            .query(
                "SELECT row_value FROM table_row WHERE table_id = ?1 AND row_key = ?2",
                params![table, key.to_vec()],
            )
            .await?;
        rows.next()
            .await?
            .map(|row| row.get(0).map_err(|_| RuntimeError::RecoveryInvalid))
            .transpose()
    }

    /// Reads the live row `key` of one admitted table identity: the row an
    /// activation admitted under `table` observes with no committed base. A
    /// point lookup, so it never loads the rest of the relation.
    pub async fn admitted_table_row(
        &self,
        table: &RuntimeTableIdentity,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, RuntimeError> {
        validate_table_identity(table.table(), key)?;
        let storage = table_storage_key(table.table(), Some(table.object_id()));
        let mut rows = self
            .connection
            .query(
                "SELECT row_value FROM table_row WHERE table_id = ?1 AND row_key = ?2",
                params![storage, key.to_vec()],
            )
            .await?;
        rows.next()
            .await?
            .map(|row| row.get(0).map_err(|_| RuntimeError::RecoveryInvalid))
            .transpose()
    }

    /// Whether `table_row` holds any row under the storage name `table`. The
    /// CLI uses this to refuse a store whose module tables still carry rows
    /// under their bare names, which the qualified module table would not see.
    pub async fn has_table_rows(&self, table: &str) -> Result<bool, RuntimeError> {
        validate_table_name(table)?;
        let mut rows = self
            .connection
            .query(
                "SELECT 1 FROM table_row WHERE table_id = ?1 LIMIT 1",
                params![table],
            )
            .await?;
        Ok(rows.next().await?.is_some())
    }

    /// Reads one complete committed relation in canonical key order.
    pub async fn committed_table_rows(
        &self,
        table: &str,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, RuntimeError> {
        validate_table_name(table)?;
        // A table written under its committed identity stores rows under
        // `oid:<hex>`, so the name is resolved through the durable identity
        // mapping first (bead ornadb-jm0d).
        let mut storage = vec![table.to_owned()];
        let mut identities = self
            .connection
            .query(
                "SELECT object_id FROM table_identity WHERE table_name = ?1",
                params![table],
            )
            .await?;
        while let Some(row) = identities.next().await? {
            let bytes: Vec<u8> = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
            if let Ok(id) = <[u8; 16]>::try_from(bytes.as_slice()) {
                storage.push(table_storage_key(table, Some(TableObjectId::new(id))));
            }
        }
        let mut rows = self
            .connection
            .query(
                "SELECT row_key, row_value FROM table_row
                 WHERE table_id = ?1 OR table_id = ?2 OR table_id = ?3
                 ORDER BY row_key",
                params![
                    storage[0].clone(),
                    storage.get(1).cloned().unwrap_or_default(),
                    storage.get(2).cloned().unwrap_or_default()
                ],
            )
            .await?;
        let mut result = Vec::new();
        while let Some(row) = rows.next().await? {
            result.push((
                row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?,
                row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?,
            ));
        }
        Ok(result)
    }

    pub async fn commit(
        &self,
        lease: WriterLease,
        expected: &CwdCapture,
        mutation: &Mutation,
        next_digest: [u8; 32],
        faults: &dyn FaultInjector,
    ) -> Result<CwdCapture, RuntimeError> {
        self.commit_batch(
            lease,
            expected,
            std::slice::from_ref(mutation),
            next_digest,
            faults,
        )
        .await
    }

    /// Atomically append a non-empty activation batch, its one corresponding
    /// checkpoint, and the next CWD capture. A fault or validation failure
    /// rolls back the complete batch rather than exposing a partial prefix.
    pub async fn commit_batch(
        &self,
        lease: WriterLease,
        expected: &CwdCapture,
        mutations: &[Mutation],
        next_digest: [u8; 32],
        faults: &dyn FaultInjector,
    ) -> Result<CwdCapture, RuntimeError> {
        validate_id(lease.owner_id)?;
        validate_mutations(mutations, next_digest)?;
        let current = self.capture().await?;
        if &current != expected {
            return Err(RuntimeError::StaleCapture {
                current: Box::new(current),
            });
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        self.require_owner(&tx, lease).await?;
        let next = append_mutations_tx(&tx, expected, mutations, next_digest, faults).await?;
        tx.commit().await?;
        faults.check(FaultPoint::AfterCommit)?;
        Ok(next)
    }
}
