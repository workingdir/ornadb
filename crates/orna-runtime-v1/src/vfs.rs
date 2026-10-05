//! FUSE-free state for projected file handles and EDIT-1 replacement drafts.
//!
//! `S` is the repository owner's opaque row-map snapshot (in production,
//! `orna_repository_v1::row_store::RowMapSnapshot`). Keeping that value in a
//! [`SnapshotPin`] binds each open handle and readdir cursor to one immutable
//! row-map version without giving this module a second row authority.

use std::{future::Future, sync::Arc};

use orna_foundation_v1::{OvbRaw, Value};
use orna_repository_v1::{CommittedRowContext, FormatContextError, RepositoryFormatContext};
use tokio::sync::Mutex;

use super::{
    CwdCapture, RuntimeState, SafeDiagnostic, TableActivationError, ValidatedTableActivationCommit,
};
#[cfg(test)]
use super::{DiagnosticClass, DiagnosticCode};

/// A retained owner-issued immutable row-map snapshot.
pub struct SnapshotPin<S>(Arc<S>);

impl<S> Clone for SnapshotPin<S> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<S> SnapshotPin<S> {
    /// Retains an opaque repository snapshot. The snapshot carries its own
    /// immutable version identity and remains the only row lookup authority.
    pub fn capture(snapshot: Arc<S>) -> Self {
        Self(snapshot)
    }

    /// Borrows the same owner-issued snapshot captured by this pin.
    pub fn snapshot(&self) -> &S {
        &self.0
    }

    /// Clones the pin for another handle that must remain on this version.
    pub fn clone_pin(&self) -> Self {
        self.clone()
    }
}

/// Immutable projected file bytes paired with the row-map snapshot that
/// admitted them.
pub struct VfsFileSnapshot<S> {
    pin: SnapshotPin<S>,
    bytes: Arc<[u8]>,
}

impl<S> VfsFileSnapshot<S> {
    pub fn new(pin: SnapshotPin<S>, bytes: impl Into<Arc<[u8]>>) -> Self {
        Self {
            pin,
            bytes: bytes.into(),
        }
    }

    pub fn pin(&self) -> &SnapshotPin<S> {
        &self.pin
    }

    pub fn len(&self) -> u64 {
        self.bytes.len() as u64
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

/// A read-only open file; its bytes and row-map snapshot never advance.
pub struct SnapshotReadHandle<S> {
    image: Arc<VfsFileSnapshot<S>>,
}

impl<S> Clone for SnapshotReadHandle<S> {
    fn clone(&self) -> Self {
        Self {
            image: Arc::clone(&self.image),
        }
    }
}

impl<S> SnapshotReadHandle<S> {
    pub fn open(image: Arc<VfsFileSnapshot<S>>) -> Self {
        Self { image }
    }

    pub fn pin(&self) -> &SnapshotPin<S> {
        self.image.pin()
    }

    pub fn len(&self) -> u64 {
        self.image.len()
    }

    pub fn is_empty(&self) -> bool {
        self.image.is_empty()
    }

    /// Reads at most `limit` bytes from this handle's captured image.
    pub fn read_at(&self, offset: u64, limit: usize) -> Vec<u8> {
        let Ok(start) = usize::try_from(offset) else {
            return Vec::new();
        };
        if start >= self.image.bytes.len() || limit == 0 {
            return Vec::new();
        }
        let end = start.saturating_add(limit).min(self.image.bytes.len());
        self.image.bytes[start..end].to_vec()
    }
}

/// A schema-resolved managed field address. `table` is the runtime relation
/// name, while `relation_id` selects the corresponding committed ORP-1 map.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VfsFieldPath {
    table: String,
    relation_id: [u8; 16],
    canonical_key: Vec<u8>,
    field_index: usize,
}

impl VfsFieldPath {
    pub fn new(
        table: impl Into<String>,
        relation_id: [u8; 16],
        canonical_key: Vec<u8>,
        field_index: usize,
    ) -> Self {
        Self {
            table: table.into(),
            relation_id,
            canonical_key,
            field_index,
        }
    }

    pub fn table(&self) -> &str {
        &self.table
    }

    pub fn relation_id(&self) -> [u8; 16] {
        self.relation_id
    }

    pub fn canonical_key(&self) -> &[u8] {
        &self.canonical_key
    }

    pub fn field_index(&self) -> usize {
        self.field_index
    }
}

/// The committed graph authority retained by one open VFS field generation.
pub struct VfsCommittedRowPin {
    path: VfsFieldPath,
    committed_row: Arc<CommittedRowContext>,
    runtime_capture: Option<CwdCapture>,
}

impl VfsCommittedRowPin {
    pub fn path(&self) -> &VfsFieldPath {
        &self.path
    }

    pub fn committed_row(&self) -> &CommittedRowContext {
        &self.committed_row
    }

    /// The local runtime generation after an accepted replacement, if any.
    pub fn runtime_capture(&self) -> Option<&CwdCapture> {
        self.runtime_capture.as_ref()
    }

    fn after_activation(&self, capture: CwdCapture) -> SnapshotPin<Self> {
        SnapshotPin::capture(Arc::new(Self {
            path: self.path.clone(),
            committed_row: Arc::clone(&self.committed_row),
            runtime_capture: Some(capture),
        }))
    }
}

/// A field handle initialized from one committed ORP-1 lookup.
pub struct VfsFieldHandle {
    image: Arc<VfsFileSnapshot<VfsCommittedRowPin>>,
}

impl VfsFieldHandle {
    pub fn path(&self) -> &VfsFieldPath {
        self.image.pin().snapshot().path()
    }

    pub fn open_read(&self) -> SnapshotReadHandle<VfsCommittedRowPin> {
        SnapshotReadHandle::open(Arc::clone(&self.image))
    }

    /// Starts a private replacement from this exact committed field image.
    pub fn open_temp_replacement(&self, max_file_bytes: usize) -> EditDraft<VfsCommittedRowPin> {
        EditDraft::open(Arc::clone(&self.image), max_file_bytes)
    }
}

/// Errors opening a graph-backed VFS field projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VfsOpenError {
    Repository(FormatContextError),
    FieldNotPresent,
}

impl From<FormatContextError> for VfsOpenError {
    fn from(error: FormatContextError) -> Self {
        Self::Repository(error)
    }
}

/// Errors before or during validated VFS replacement.
#[derive(Debug)]
pub enum VfsSaveError {
    Runtime(super::RuntimeError),
    DraftChanged,
    TargetMismatch,
    TargetMutationMissing,
    AmbiguousTargetMutation,
    InvalidReplacement,
    StaleRuntimeCapture,
}

/// FUSE-free production bridge from a committed repository field to the
/// runtime's validated EDIT-1 table activation boundary.
pub struct RepositoryVfs;

impl RepositoryVfs {
    /// Opens one schema-resolved field through the committed ORP-1 row map.
    /// The returned handle retains its OGS-1 graph context and admitted row.
    pub fn open_committed_field(
        format: &RepositoryFormatContext,
        path: VfsFieldPath,
    ) -> Result<Option<VfsFieldHandle>, VfsOpenError> {
        let Some(committed_row) = format.open_committed_row(
            path.relation_id,
            &path.canonical_key,
        )? else {
            return Ok(None);
        };
        let bytes = committed_row
            .canonical_field_bytes(path.field_index)?
            .ok_or(VfsOpenError::FieldNotPresent)?;
        let pin = SnapshotPin::capture(Arc::new(VfsCommittedRowPin {
            path,
            committed_row: Arc::new(committed_row),
            runtime_capture: None,
        }));
        let image = Arc::new(VfsFileSnapshot::new(pin, bytes));
        Ok(Some(VfsFieldHandle { image }))
    }

    /// Renames a completed temporary field onto its managed path. The exact
    /// temp bytes must be the selected field in one complete row mutation;
    /// acceptance runs the runtime's existing validated atomic transaction.
    pub async fn rename_temp_onto_field(
        runtime: &RuntimeState,
        target: &VfsFieldPath,
        draft: &EditDraft<VfsCommittedRowPin>,
        expected_temp_bytes: &[u8],
        request: ValidatedTableActivationCommit<'_>,
    ) -> Result<FsyncOutcome<VfsCommittedRowPin>, VfsSaveError> {
        let baseline = draft.baseline().await;
        if baseline.pin().snapshot().path() != target {
            return Err(VfsSaveError::TargetMismatch);
        }

        let target = target.clone();
        let expected_temp_bytes: Arc<[u8]> = Arc::from(expected_temp_bytes);
        draft
            .fsync_with(move |candidate| async move {
                if candidate.replacement_bytes() != expected_temp_bytes.as_ref() {
                    return Err(VfsSaveError::DraftChanged);
                }
                let baseline = candidate.baseline().clone_pin();
                if baseline
                    .snapshot()
                    .runtime_capture()
                    .is_some_and(|capture| request.context.capture() != capture)
                {
                    return Err(VfsSaveError::StaleRuntimeCapture);
                }
                let mut matching = request.mutations.iter().filter(|mutation| {
                    mutation.table() == target.table()
                        && mutation.key() == target.canonical_key()
                        && mutation.value().is_some()
                        && mutation.rekey_to().is_none()
                });
                let target_mutation = matching
                    .next()
                    .ok_or(VfsSaveError::TargetMutationMissing)?;
                if matching.next().is_some() {
                    return Err(VfsSaveError::AmbiguousTargetMutation);
                }
                let encoded_row = target_mutation
                    .value()
                    .ok_or(VfsSaveError::TargetMutationMissing)?;
                let projected = canonical_field_from_row(encoded_row, target.field_index())?
                    .ok_or(VfsSaveError::InvalidReplacement)?;
                if projected.as_slice() != expected_temp_bytes.as_ref() {
                    return Err(VfsSaveError::InvalidReplacement);
                }

                match commit_vfs_table_activation(runtime, request).await {
                    Ok(capture) => Ok(ActivationDecision::Accepted(
                        baseline.snapshot().after_activation(capture),
                    )),
                    Err(TableActivationError::ValidationFailed(diagnostic)) => {
                        Ok(ActivationDecision::Rejected(diagnostic))
                    }
                    Err(TableActivationError::Runtime(error)) => {
                        Err(VfsSaveError::Runtime(error))
                    }
                }
            })
            .await
    }
}

fn canonical_field_from_row(
    encoded_row: &[u8],
    field_index: usize,
) -> Result<Option<Vec<u8>>, VfsSaveError> {
    let value = Value::decode(encoded_row).map_err(|_| VfsSaveError::InvalidReplacement)?;
    let OvbRaw::Array(fields) = value.raw() else {
        return Err(VfsSaveError::InvalidReplacement);
    };
    let Some(field) = fields.get(field_index) else {
        return Ok(None);
    };
    Value::new(field.clone())
        .and_then(|value| value.encode())
        .map(Some)
        .map_err(|_| VfsSaveError::InvalidReplacement)
}

/// A readdir continuation paired with the exact snapshot used for its scan.
/// The owner-provided continuation value can never be detached from that pin.
pub struct SnapshotReaddirCursor<S, C> {
    pin: SnapshotPin<S>,
    continuation: C,
}

impl<S, C> SnapshotReaddirCursor<S, C> {
    pub fn new(pin: SnapshotPin<S>, continuation: C) -> Self {
        Self { pin, continuation }
    }

    pub fn pin(&self) -> &SnapshotPin<S> {
        &self.pin
    }

    pub fn continuation(&self) -> &C {
        &self.continuation
    }

    pub fn continuation_mut(&mut self) -> &mut C {
        &mut self.continuation
    }
}

/// An owned copy-on-write candidate passed to the table activation adapter.
pub struct ActivationCandidate<S> {
    baseline: SnapshotPin<S>,
    revision: u64,
    replacement: Arc<[u8]>,
}

impl<S> ActivationCandidate<S> {
    pub fn baseline(&self) -> &SnapshotPin<S> {
        &self.baseline
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn replacement_bytes(&self) -> &[u8] {
        &self.replacement
    }
}

/// The accepted replacement's new repository snapshot, or its stable safe
/// validation diagnostic. Rejection never discards the candidate.
pub enum ActivationDecision<S> {
    Accepted(SnapshotPin<S>),
    Rejected(SafeDiagnostic),
}

/// The result of fsync. Repeated syncs of a rejected revision return the same
/// diagnostic without calling the activation boundary again.
pub enum FsyncOutcome<S> {
    Accepted(SnapshotReadHandle<S>),
    Rejected(SafeDiagnostic),
    Unchanged(SnapshotReadHandle<S>),
}

/// The latest rejected replacement remains separately readable even after a
/// later write starts a new candidate from its bytes.
pub struct RetainedInvalidDraft<S> {
    baseline: SnapshotPin<S>,
    revision: u64,
    replacement: Arc<[u8]>,
    diagnostic: SafeDiagnostic,
}

impl<S> Clone for RetainedInvalidDraft<S> {
    fn clone(&self) -> Self {
        Self {
            baseline: self.baseline.clone(),
            revision: self.revision,
            replacement: Arc::clone(&self.replacement),
            diagnostic: self.diagnostic,
        }
    }
}

impl<S> RetainedInvalidDraft<S> {
    pub fn baseline(&self) -> &SnapshotPin<S> {
        &self.baseline
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn replacement_bytes(&self) -> &[u8] {
        &self.replacement
    }

    pub fn diagnostic(&self) -> SafeDiagnostic {
        self.diagnostic
    }
}

struct DraftState<S> {
    baseline: Arc<VfsFileSnapshot<S>>,
    candidate: Option<Vec<u8>>,
    revision: u64,
    last_rejection: Option<(u64, SafeDiagnostic)>,
    retained_invalid: Option<RetainedInvalidDraft<S>>,
}

/// A private EDIT-1 replacement draft. Writes use a copy-on-write buffer;
/// fsync serializes against writes and invokes the caller's validated table
/// activation path. Dropping or releasing the draft never accepts it.
pub struct EditDraft<S> {
    state: Mutex<DraftState<S>>,
    max_file_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditDraftError {
    FileTooLarge,
    OffsetOverflow,
    RevisionExhausted,
}

impl<S> EditDraft<S> {
    pub fn open(image: Arc<VfsFileSnapshot<S>>, max_file_bytes: usize) -> Self {
        Self {
            state: Mutex::new(DraftState {
                baseline: image,
                candidate: None,
                revision: 0,
                last_rejection: None,
                retained_invalid: None,
            }),
            max_file_bytes,
        }
    }

    pub async fn baseline(&self) -> SnapshotReadHandle<S> {
        SnapshotReadHandle::open(Arc::clone(&self.state.lock().await.baseline))
    }

    /// Applies a positional write, zero-filling any gap in the private copy.
    pub async fn write_at(&self, offset: u64, bytes: &[u8]) -> Result<(), EditDraftError> {
        if bytes.is_empty() {
            return Ok(());
        }
        let start = usize::try_from(offset).map_err(|_| EditDraftError::OffsetOverflow)?;
        let end = start
            .checked_add(bytes.len())
            .ok_or(EditDraftError::OffsetOverflow)?;
        if end > self.max_file_bytes {
            return Err(EditDraftError::FileTooLarge);
        }

        let mut state = self.state.lock().await;
        let current_len = state
            .candidate
            .as_ref()
            .map_or(state.baseline.bytes.len(), Vec::len);
        if current_len.max(end) > self.max_file_bytes {
            return Err(EditDraftError::FileTooLarge);
        }
        let next_revision = state
            .revision
            .checked_add(1)
            .ok_or(EditDraftError::RevisionExhausted)?;
        let baseline_bytes = Arc::clone(&state.baseline.bytes);
        let candidate = state
            .candidate
            .get_or_insert_with(|| baseline_bytes.to_vec());
        candidate.resize(candidate.len().max(end), 0);
        candidate[start..end].copy_from_slice(bytes);
        state.revision = next_revision;
        Ok(())
    }

    /// Changes the candidate length. Extending creates zero-filled holes.
    pub async fn truncate(&self, length: u64) -> Result<(), EditDraftError> {
        let length = usize::try_from(length).map_err(|_| EditDraftError::OffsetOverflow)?;
        if length > self.max_file_bytes {
            return Err(EditDraftError::FileTooLarge);
        }
        let mut state = self.state.lock().await;
        let current_len = state
            .candidate
            .as_ref()
            .map_or(state.baseline.bytes.len(), Vec::len);
        if current_len == length {
            return Ok(());
        }
        let next_revision = state
            .revision
            .checked_add(1)
            .ok_or(EditDraftError::RevisionExhausted)?;
        let baseline_bytes = Arc::clone(&state.baseline.bytes);
        let candidate = state
            .candidate
            .get_or_insert_with(|| baseline_bytes.to_vec());
        candidate.resize(length, 0);
        state.revision = next_revision;
        Ok(())
    }

    pub async fn candidate_bytes(&self) -> Arc<[u8]> {
        let state = self.state.lock().await;
        Arc::from(
            state
                .candidate
                .as_deref()
                .unwrap_or(state.baseline.bytes.as_ref()),
        )
    }

    pub async fn retained_invalid_draft(&self) -> Option<RetainedInvalidDraft<S>> {
        self.state.lock().await.retained_invalid.clone()
    }

    /// Calls the validated activation adapter exactly once for a changed
    /// revision. A failed callback leaves the draft retryable; a validation
    /// rejection is cached and retained until the draft changes or is dropped.
    pub async fn fsync_with<F, Fut, E>(&self, activate: F) -> Result<FsyncOutcome<S>, E>
    where
        S: Send + Sync + 'static,
        F: FnOnce(ActivationCandidate<S>) -> Fut,
        Fut: Future<Output = Result<ActivationDecision<S>, E>>,
    {
        let mut state = self.state.lock().await;
        let Some(bytes) = state.candidate.as_ref() else {
            return Ok(FsyncOutcome::Unchanged(SnapshotReadHandle::open(
                Arc::clone(&state.baseline),
            )));
        };
        if let Some((revision, diagnostic)) = state.last_rejection
            && revision == state.revision
        {
            return Ok(FsyncOutcome::Rejected(diagnostic));
        }

        let revision = state.revision;
        let candidate = ActivationCandidate {
            baseline: state.baseline.pin.clone(),
            revision,
            replacement: Arc::from(bytes.as_slice()),
        };
        match activate(candidate).await? {
            ActivationDecision::Accepted(pin) => {
                let bytes: Arc<[u8]> =
                    Arc::from(state.candidate.take().expect("candidate held by lock"));
                let image = Arc::new(VfsFileSnapshot::new(pin, bytes));
                state.baseline = Arc::clone(&image);
                state.last_rejection = None;
                Ok(FsyncOutcome::Accepted(SnapshotReadHandle::open(image)))
            }
            ActivationDecision::Rejected(diagnostic) => {
                let replacement: Arc<[u8]> =
                    Arc::from(state.candidate.as_deref().expect("candidate held by lock"));
                state.last_rejection = Some((revision, diagnostic));
                state.retained_invalid = Some(RetainedInvalidDraft {
                    baseline: state.baseline.pin.clone(),
                    revision,
                    replacement,
                    diagnostic,
                });
                Ok(FsyncOutcome::Rejected(diagnostic))
            }
        }
    }

    /// Explicit close path. It drops private state and never invokes activation.
    pub fn release(self) {}
}

/// The VFS call boundary into the runtime's single validated table transaction.
/// Callers prepare complete typed row mutations and graph-issued pin transfers
/// in the request; this wrapper adds no journal or publication authority.
async fn commit_vfs_table_activation(
    runtime: &RuntimeState,
    request: ValidatedTableActivationCommit<'_>,
) -> Result<CwdCapture, TableActivationError> {
    runtime.commit_validated_table_activation(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn fixture_image() -> Arc<VfsFileSnapshot<u64>> {
        let contents = include_str!("../tests/fixtures/publication-repository-main.orna");
        Arc::new(VfsFileSnapshot::new(
            SnapshotPin::capture(Arc::new(7)),
            Arc::<[u8]>::from(contents.as_bytes()),
        ))
    }

    fn rejected() -> SafeDiagnostic {
        SafeDiagnostic {
            code: DiagnosticCode::TableAssertionFalse,
            class: DiagnosticClass::Permanent,
        }
    }

    #[tokio::test]
    async fn open_read_keeps_its_captured_image_and_directory_pin() {
        let image = fixture_image();
        let handle = SnapshotReadHandle::open(Arc::clone(&image));
        let cursor = SnapshotReaddirCursor::new(image.pin().clone_pin(), 23_u64);
        let original = handle.read_at(0, handle.len() as usize);

        let draft = EditDraft::open(image, 1 << 20);
        draft.write_at(0, &[0x58]).await.unwrap();

        assert_eq!(handle.read_at(0, original.len()), original);
        assert_eq!(*cursor.pin().snapshot(), 7);
        assert_ne!(draft.candidate_bytes().await.as_ref(), original.as_slice());
    }

    #[tokio::test]
    async fn rejected_draft_is_retained_and_repeated_fsync_is_idempotent() {
        let draft = EditDraft::open(fixture_image(), 1 << 20);
        draft.write_at(0, &[0x58]).await.unwrap();
        let calls = AtomicUsize::new(0);
        let first = draft
            .fsync_with(|candidate| {
                calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    assert!(!candidate.replacement_bytes().is_empty());
                    Ok::<_, ()>(ActivationDecision::Rejected(rejected()))
                }
            })
            .await
            .unwrap();
        assert!(matches!(first, FsyncOutcome::Rejected(_)));

        let retained = draft.retained_invalid_draft().await.unwrap();
        assert_eq!(retained.diagnostic(), rejected());
        assert_ne!(retained.replacement_bytes(), fixture_image().bytes.as_ref());

        let second = draft
            .fsync_with(|_| {
                calls.fetch_add(1, Ordering::SeqCst);
                async {
                    Ok::<_, ()>(ActivationDecision::Accepted(SnapshotPin::capture(
                        Arc::new(8),
                    )))
                }
            })
            .await
            .unwrap();
        assert!(matches!(second, FsyncOutcome::Rejected(_)));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn accepted_fsync_advances_only_future_opens_and_release_never_commits() {
        let image = fixture_image();
        let old_handle = SnapshotReadHandle::open(Arc::clone(&image));
        let draft = EditDraft::open(image, 1 << 20);
        draft.write_at(0, &[0x58]).await.unwrap();
        let accepted = draft
            .fsync_with(|candidate| async move {
                assert_eq!(*candidate.baseline().snapshot(), 7);
                Ok::<_, ()>(ActivationDecision::Accepted(SnapshotPin::capture(
                    Arc::new(8),
                )))
            })
            .await
            .unwrap();
        let FsyncOutcome::Accepted(new_handle) = accepted else {
            panic!("activation should accept the candidate");
        };

        assert_eq!(*new_handle.pin().snapshot(), 8);
        assert_eq!(*old_handle.pin().snapshot(), 7);
        assert_ne!(old_handle.read_at(0, 1), new_handle.read_at(0, 1));

        let calls = AtomicUsize::new(0);
        let draft = EditDraft::open(fixture_image(), 1 << 20);
        draft.write_at(1, &[0x59]).await.unwrap();
        draft.release();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
