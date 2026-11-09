//! VFS row save admission: complete typed row replacements, candidate
//! validators and one commit through the runtime's validated activation.
//!
//! The validator is supplied by the caller that owns the relation's schema,
//! key/field and assertion authority, so this module never invents a second
//! row authority (ORNA-VFS-002).

use std::collections::BTreeMap;

use orna_repository::{
    ContentIdentity, CwdGeneration, GraphError, OrpBlobBinding, ProtectedContentPin,
    ProtectedContentTransfer, RepositoryCaptureCapability,
};

use super::{ActivationCandidate, CandidateSnapshot, EditDraftError};
use crate::{
    CwdCapture, FaultInjector, ProtectedContentTransferEvidence, RuntimeActivationContext,
    RuntimeError, RuntimeQueryError, RuntimeState, RuntimeTableActivationSnapshot,
    RuntimeTableIdentity, SafeDiagnostic, TableActivationCandidateValidator, TableActivationError,
    TableMutation, TableSchemaIdentity, ValidatedTableActivationCommit, WriterLease,
};

/// One complete VFS row replacement routed into the runtime's validated table
/// activation boundary. The table identity is repository-issued admission
/// metadata; host paths never supply or override it.
pub struct VfsTableRowReplacement {
    mutation: TableMutation,
    table: RuntimeTableIdentity,
    content_pins: Vec<ProtectedContentPin>,
}

impl VfsTableRowReplacement {
    pub fn new(
        mutation_id: [u8; 16],
        table: RuntimeTableIdentity,
        key: Vec<u8>,
        value: Option<Vec<u8>>,
    ) -> Result<Self, RuntimeError> {
        let mutation = TableMutation::new(mutation_id, table.table(), key, value)?
            .with_table_object_id(table.object_id());
        Ok(Self {
            mutation,
            table,
            content_pins: Vec::new(),
        })
    }

    /// Wraps the complete mutation returned by the source producer's
    /// `AdmittedSourceRow::into_mutation_and_bindings` with its owned field
    /// pins. The pins must match the mutation's transfer evidence in order.
    pub fn from_admitted_mutation(
        table: RuntimeTableIdentity,
        mutation: TableMutation,
        content_pins: Vec<ProtectedContentPin>,
    ) -> Result<Self, RuntimeError> {
        if mutation.table() != table.table()
            || mutation.table_object_id() != Some(table.object_id())
            || mutation.value().is_none()
            || mutation.is_insert()
            || mutation.rekey_to().is_some()
        {
            return Err(RuntimeError::InvalidTableMutation);
        }
        let transfers = content_pins
            .iter()
            .map(|pin| ProtectedContentTransferEvidence::from_transfer(&pin.transfer_record()))
            .collect::<Vec<_>>();
        if transfers.as_slice() != mutation.protected_content_transfers() {
            return Err(RuntimeError::InvalidTableMutation);
        }
        Ok(Self {
            mutation,
            table,
            content_pins,
        })
    }

    pub fn from_candidate<S>(
        mutation_id: [u8; 16],
        table: RuntimeTableIdentity,
        key: Vec<u8>,
        candidate: &ActivationCandidate<S>,
    ) -> Result<Self, RuntimeError> {
        let value = (!candidate.is_removal()).then(|| candidate.replacement_bytes().to_vec());
        Self::new(mutation_id, table, key, value)
    }

    pub fn table(&self) -> &RuntimeTableIdentity {
        &self.table
    }

    pub fn key(&self) -> &[u8] {
        self.mutation.key()
    }

    pub fn value(&self) -> Option<&[u8]> {
        self.mutation.value()
    }

    /// Carries graph-issued native content pins with the saved row so the
    /// durable transaction can atomically attach them to its mutation and
    /// accepted-pin manifest (VFS-003, VFS-021).
    pub fn with_protected_content_pin(
        mut self,
        pin: ProtectedContentPin,
    ) -> Result<Self, RuntimeError> {
        self.mutation = self.mutation.with_protected_content_pin(&pin)?;
        self.content_pins.push(pin);
        Ok(self)
    }

    pub fn mutation(&self) -> Result<TableMutation, RuntimeError> {
        Ok(self.mutation.clone())
    }
}

/// The VFS call boundary into the runtime's single validated table transaction.
/// Callers prepare complete typed row mutations and graph-issued pin transfers
/// in the request; this wrapper adds no journal or publication authority.
pub async fn commit_vfs_table_activation(
    runtime: &RuntimeState,
    request: ValidatedTableActivationCommit<'_>,
) -> Result<CwdCapture, TableActivationError> {
    runtime.commit_validated_table_activation(request).await
}

/// The repository-issued coordinates one durable document save commits under.
///
/// The writer that owns the repository, the admitted relation identity and
/// typed key, and the table schema identity it is validated under are one unit:
/// a save cannot combine a schema identity from one committed state with a row
/// identity from another (`ORNA-STATE-003`, VFS-002).
pub struct VfsDurableSave {
    owner_id: [u8; 16],
    expected_schema: TableSchemaIdentity,
    table: RuntimeTableIdentity,
    key: Vec<u8>,
    pub(super) expected_baseline_row: Option<Option<Vec<u8>>>,
}

impl VfsDurableSave {
    pub fn new(
        owner_id: [u8; 16],
        expected_schema: TableSchemaIdentity,
        table: RuntimeTableIdentity,
        key: Vec<u8>,
    ) -> Self {
        Self {
            owner_id,
            expected_schema,
            table,
            key,
            expected_baseline_row: None,
        }
    }

    /// Binds a save to the exact encoded row observed when its edit began.
    /// `None` means the row was absent; an unconfigured baseline is distinct.
    pub fn with_expected_baseline_row(mut self, row: Option<Vec<u8>>) -> Self {
        self.expected_baseline_row = Some(row);
        self
    }

    /// The admitted committed identity of the saved row's relation.
    pub fn table(&self) -> &RuntimeTableIdentity {
        &self.table
    }

    /// The admitted typed key of the saved row.
    pub fn key(&self) -> &[u8] {
        &self.key
    }
}

/// The admitted coordinates one save hands to its caller: the committed
/// relation identity and typed key the candidate row must address. Owned, so a
/// caller's async candidate builder borrows nothing from the admission.
pub struct VfsSaveCoordinates {
    table: RuntimeTableIdentity,
    key: Vec<u8>,
}

impl VfsSaveCoordinates {
    /// The admitted committed identity of the saved row's relation.
    pub fn table(&self) -> &RuntimeTableIdentity {
        &self.table
    }

    /// The admitted typed key of the saved row.
    pub fn key(&self) -> &[u8] {
        &self.key
    }
}

/// One admitted VFS row save, held open across the activation.
///
/// A save is admitted once: the runtime leases the repository, captures the
/// generation the candidate is based on, and commits under the digest of that
/// capture. `S` is the repository owner's opaque row-map snapshot, so the
/// baseline carried by the draft and this admission's capture travel together.
pub struct VfsSaveAdmission<'a, S> {
    runtime: &'a RuntimeState,
    pub(super) snapshot: RuntimeTableActivationSnapshot,
    context: RuntimeActivationContext,
    save: VfsDurableSave,
    pub(super) writer: WriterLease,
    expected_baseline_row: Option<Option<Vec<u8>>>,
    digest: [u8; 32],
    faults: &'a dyn FaultInjector,
    _baseline: std::marker::PhantomData<fn() -> S>,
}

/// One complete VFS row save: the exact row replacement plus the caller's
/// candidate validator. The validator carries the schema, key/field and
/// assertion authority for the relation, so this module never invents a second
/// row authority and never validates rows it does not own (VFS-002).
pub struct VfsSaveCandidate {
    replacement: VfsTableRowReplacement,
    validator: Box<dyn TableActivationCandidateValidator>,
}

impl VfsSaveCandidate {
    pub fn new(
        replacement: VfsTableRowReplacement,
        validator: Box<dyn TableActivationCandidateValidator>,
    ) -> Self {
        Self {
            replacement,
            validator,
        }
    }
    /// Requires every protected content transfer to match the row value
    /// generated from those transfers in the accepted candidate snapshot.
    pub fn with_protected_content_transfers(
        self,
        row_value: Vec<u8>,
        expected_transfers: Vec<ProtectedContentTransfer>,
    ) -> Self {
        if expected_transfers.is_empty() {
            return self;
        }
        let table = self.replacement.table().table().to_owned();
        let key = self.replacement.key().to_vec();
        Self {
            replacement: self.replacement,
            validator: Box::new(ContentPinCandidateValidator {
                inner: self.validator,
                table,
                key,
                row_value,
                expected_transfers,
            }),
        }
    }

    /// The row replacement the activation will commit.
    pub fn replacement(&self) -> &VfsTableRowReplacement {
        &self.replacement
    }

    /// Makes the commit read the table schema identity inside the writer
    /// transaction and refuse when it differs from the one this save was
    /// admitted under.
    pub fn with_live_schema_identity(
        self,
        read: impl FnMut() -> Option<TableSchemaIdentity> + Send + 'static,
    ) -> Self {
        Self {
            replacement: self.replacement,
            validator: Box::new(LiveSchemaCandidateValidator {
                inner: self.validator,
                read: Box::new(read),
            }),
        }
    }
}
struct ContentPinCandidateValidator {
    inner: Box<dyn TableActivationCandidateValidator>,
    table: String,
    key: Vec<u8>,
    row_value: Vec<u8>,
    expected_transfers: Vec<ProtectedContentTransfer>,
}

impl TableActivationCandidateValidator for ContentPinCandidateValidator {
    fn tables(&self) -> &[String] {
        self.inner.tables()
    }

    fn table_object_ids(&self) -> BTreeMap<String, crate::TableObjectId> {
        self.inner.table_object_ids()
    }

    fn cwd_generation(&self) -> Option<&CwdGeneration> {
        self.inner.cwd_generation()
    }

    fn schema_identity(&mut self) -> Option<TableSchemaIdentity> {
        self.inner.schema_identity()
    }

    fn validate(&mut self, rows: &crate::RuntimeTableRows) -> Result<(), SafeDiagnostic> {
        self.inner.validate(rows)
    }

    fn validate_content_pins(
        &mut self,
        rows: &crate::RuntimeTableRows,
        transfers: &[ProtectedContentTransfer],
    ) -> bool {
        self.inner.validate_content_pins(rows, &[])
            && transfers == self.expected_transfers.as_slice()
            && rows.get(&self.table).is_some_and(|table_rows| {
                table_rows
                    .iter()
                    .any(|(key, value)| key == &self.key && value == &self.row_value)
            })
    }
}

/// Reads the table schema identity inside the writer transaction, so a save
/// admitted under one schema cannot commit under another (ORNA-VFS-019).
struct LiveSchemaCandidateValidator {
    inner: Box<dyn TableActivationCandidateValidator>,
    read: Box<dyn FnMut() -> Option<TableSchemaIdentity> + Send>,
}

impl TableActivationCandidateValidator for LiveSchemaCandidateValidator {
    fn tables(&self) -> &[String] {
        self.inner.tables()
    }

    fn table_object_ids(&self) -> BTreeMap<String, crate::TableObjectId> {
        self.inner.table_object_ids()
    }

    fn committed_rows(&self) -> Option<&crate::PinnedCommittedRows> {
        self.inner.committed_rows()
    }

    fn cwd_generation(&self) -> Option<&CwdGeneration> {
        self.inner.cwd_generation()
    }

    fn schema_identity(&mut self) -> Option<TableSchemaIdentity> {
        (self.read)()
    }

    fn validate(&mut self, rows: &crate::RuntimeTableRows) -> Result<(), SafeDiagnostic> {
        self.inner.validate(rows)
    }

    fn validate_changed(
        &mut self,
        rows: &crate::RuntimeTableRows,
        changed_tables: &std::collections::BTreeSet<String>,
    ) -> Result<(), SafeDiagnostic> {
        self.inner.validate_changed(rows, changed_tables)
    }

    fn failed_module_assertion(&self) -> Option<String> {
        self.inner.failed_module_assertion()
    }

    fn validate_content_pins(
        &mut self,
        rows: &crate::RuntimeTableRows,
        transfers: &[ProtectedContentTransfer],
    ) -> bool {
        self.inner.validate_content_pins(rows, transfers)
    }
}

impl<'a, S: Send + Sync + 'static> VfsSaveAdmission<'a, S> {
    /// Leases the repository and captures the generation this save is based on.
    ///
    /// The capture is the runtime's admitted-table activation, so the row's
    /// committed relation identity is verified against committed metadata in the
    /// same read transaction that captures the generation (VFS-002).
    pub async fn begin(
        runtime: &'a RuntimeState,
        save: VfsDurableSave,
        faults: &'a dyn FaultInjector,
    ) -> Result<Self, TableActivationError> {
        if crate::activation::runtime_activation_active(runtime) {
            return Err(TableActivationError::Runtime(RuntimeError::AdminBusy));
        }
        let writer = runtime
            .acquire_lease(save.owner_id)
            .await
            .map_err(TableActivationError::Runtime)?;
        let snapshot = runtime
            .begin_admitted_table_activation(std::slice::from_ref(&save.table))
            .await
            .map_err(TableActivationError::Runtime)?;
        let context = snapshot.context().clone();
        let digest = context.capture().generation_digest();
        let expected_baseline_row = save.expected_baseline_row.clone();
        Ok(Self {
            runtime,
            snapshot,
            context,
            save,
            expected_baseline_row,
            writer,
            digest,
            faults,
            _baseline: std::marker::PhantomData,
        })
    }

    /// The admitted coordinates the candidate row must address. Owned so the
    /// caller's candidate builder can own them across its own await points.
    pub fn coordinates(&self) -> VfsSaveCoordinates {
        VfsSaveCoordinates {
            table: self.save.table.clone(),
            key: self.save.key.clone(),
        }
    }

    /// The digest the activation commits under.
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }

    /// The admitted relation row this save is replacing, as read inside the
    /// admission's own committed capture. It is checked against the captured
    /// baseline before the replacement can be committed.
    pub fn admitted_row(&self) -> Result<Option<&[u8]>, RuntimeQueryError> {
        self.snapshot
            .query_exact(self.save.table.table(), &self.save.key)
    }
    pub(super) fn expected_baseline_matches(&self, current: Option<&[u8]>) -> Option<bool> {
        self.expected_baseline_row
            .as_ref()
            .map(|expected| current == expected.as_deref())
    }
    pub(super) fn validate_replacement(
        &self,
        replacement: &VfsTableRowReplacement,
    ) -> Result<(), TableActivationError> {
        if replacement.table() != &self.save.table || replacement.key() != self.save.key.as_slice()
        {
            return Err(TableActivationError::Runtime(
                RuntimeError::InvalidTableMutation,
            ));
        }
        Ok(())
    }

    /// Commits one complete row replacement through the runtime's single
    /// validated transaction. The candidate relation is validated before any
    /// row, mutation record, or checkpoint becomes visible, so a rejected save
    /// leaves the row store byte-identical and publishes no checkpoint. The
    /// returned capture is a reopenable durable acknowledgement (VFS-021).
    pub async fn commit(
        &self,
        candidate: VfsSaveCandidate,
    ) -> Result<CwdCapture, TableActivationError> {
        if crate::activation::runtime_activation_active(self.runtime) {
            return Err(TableActivationError::Runtime(RuntimeError::AdminBusy));
        }
        let VfsSaveCandidate {
            replacement,
            mut validator,
        } = candidate;
        self.validate_replacement(&replacement)?;
        let VfsTableRowReplacement {
            mutation,
            content_pins,
            ..
        } = replacement;
        let mut content_pins = content_pins;
        commit_vfs_table_activation(
            self.runtime,
            ValidatedTableActivationCommit {
                writer: self.writer,
                context: &self.context,
                cwd_generation: None,
                expected_schema: Some(self.save.expected_schema.clone()),
                mutations: std::slice::from_ref(&mutation),
                content_pins: &mut content_pins,
                next_digest: self.digest,
                validator: validator.as_mut(),
                faults: self.faults,
            },
        )
        .await
    }
}

/// Failure while streaming and protecting a replacement Blob.
#[derive(Debug)]
pub enum VfsBlobReplacementError {
    Capture(GraphError),
    IdentityMismatch {
        expected: ContentIdentity,
        actual: ContentIdentity,
    },
}

impl std::fmt::Display for VfsBlobReplacementError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Capture(error) => write!(formatter, "Blob capture failed: {error}"),
            Self::IdentityMismatch { expected, actual } => write!(
                formatter,
                "Blob identity mismatch: expected {expected:?}, captured {actual:?}"
            ),
        }
    }
}

impl std::error::Error for VfsBlobReplacementError {}

pub(super) fn capture_vfs_blob_replacement(
    capture: &RepositoryCaptureCapability,
    bytes: &CandidateSnapshot,
    expected: ContentIdentity,
    media_type: &str,
    suffix: Option<&str>,
) -> Result<OrpBlobBinding, VfsBlobReplacementError> {
    let candidate = capture
        .capture_blob_candidate(bytes.reader(), expected.length())
        .map_err(VfsBlobReplacementError::Capture)?;
    let actual = candidate.content_identity();
    if actual != expected {
        return Err(VfsBlobReplacementError::IdentityMismatch { expected, actual });
    }
    capture
        .accept_captured_blob(candidate, media_type, suffix)
        .map_err(VfsBlobReplacementError::Capture)
}
/// Why a durable save was not admitted. Every variant is decided before the
/// row store is touched except `Activation`, which the runtime refuses inside
/// its own transaction.
#[derive(Debug)]
pub enum VfsSaveError<E> {
    /// The activated destination already accepted this draft.
    AlreadyApplied {
        /// The cache generation the accepted image was published at.
        generation: u64,
    },
    /// The captured baseline is no longer the destination's image, so the
    /// candidate is stale and the destination keeps its accepted row (VFS-011).
    Stale {
        /// The repository's cache generation observed at admission.
        generation: u64,
        diagnostic: SafeDiagnostic,
    },
    /// The runtime refused the saved row. The row store keeps the accepted row
    /// and this is the stable diagnostic association in local state (VFS-009).
    Rejected {
        /// The repository's cache generation observed at admission.
        generation: u64,
        diagnostic: SafeDiagnostic,
    },
    /// The file holds no candidate bytes to admit (VFS-001).
    NoCandidate,
    /// The temporary rename route requires an admitted durable baseline.
    MissingBaseline,
    /// The temporary file belongs to a different managed destination.
    WrongDestination,
    /// The target belongs to another runtime's projection/cache scope.
    WrongRepositoryScope,
    /// The admitted row lookup failed before any candidate commit.
    AdmissionLookup(RuntimeQueryError),
    /// Repository capture failed before the shared row-transaction boundary.
    BlobCapture(VfsBlobReplacementError),
    /// The private candidate spool could not be read; the draft is kept.
    Backing(EditDraftError),
    Activation(E),
}

#[derive(Debug)]
pub(super) enum VfsSaveAdmissionError {
    BlobCapture(VfsBlobReplacementError),
    Activation(TableActivationError),
    Backing(EditDraftError),
}
