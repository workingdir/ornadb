use std::{
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use orna_evaluator_v1::{Limits, SysHostBindingRegistry, evaluate_expression_ovb2_with_effects};
use orna_project_v1::ProjectLoader;
use orna_runtime_v1::{
    FaultInjector, FaultPoint, RequestIdentity, RequestState, RuntimeError, RuntimeIdentity,
    RuntimeState, TableMutation, TableObjectId, TerminalOutcome,
};
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};
use orna_value_v1::{
    decode_rov3_blob_metadata, AvailabilityState, Blob, BlobResolver, ContentIdentity,
    ContentReference, ContextValue, Error as BlobError, Format3Context, Format3Quota, GitHash,
    NativeOid, OwnerLifetime, Snapshot, ValueFormat,
};
use tempfile::TempDir;

#[tokio::test]
/// Exercises capture through the runtime into a format-3 row reference: raw
/// identity survives annotated OVB-2/ROV-3 conversion and metadata conversion
/// performs no payload reads. ORNA-BLOB-004, ORNA-BLOB-007, ORNA-BLOB-009,
/// ORNA-OGB-001.
async fn cli_consumer_commits_captured_annotated_blob_with_its_request() {
    let (directory, repository, relation_id) = empty_format3_repository();
    let commit = repository.head().unwrap().unwrap();
    let source_table = ProjectLoader::default()
        .resolve_committed_source_table_schema(
            &repository,
            &commit,
            TableObjectId::new(relation_id),
        )
        .unwrap();
    assert_eq!(source_table.binding().module_name(), Some(""));
    assert_eq!(source_table.binding().source_path(), Some("main.orna"));
    assert_eq!(source_table.binding().declaration_name(), Some("media"));
    let media_table_id = *source_table.binding().table_object_id();
    assert_eq!(media_table_id, relation_id);
    let root = directory.path();
    let payload = include_bytes!("fixtures/media/tone.wav");
    std::fs::write(root.join("tone.wav"), payload).unwrap();
    let head_before_capture = git_output(root, &["rev-parse", "HEAD"], None);

    let runtime_identity = RuntimeIdentity {
        database_id: [0, 0, 0, 0, 0, 0, 0x40, 0, 0x80, 0, 0, 0, 0, 0, 0, 1],
        repository_id: [0x52; 16],
    };
    let state = RuntimeState::open(&repository, runtime_identity, [0x53; 32])
        .await
        .unwrap();
    let writer = state.acquire_lease([0x54; 16]).await.unwrap();
    let request_identity = RequestIdentity {
        session_id: [0x55; 16],
        request_id: [0x56; 16],
    };
    let fingerprint = [0x57; 32];
    let (_, admission) = state
        .reserve_request_with_admission(request_identity, fingerprint)
        .await
        .unwrap();
    state
        .start_request_with_owner_and_admission(
            request_identity,
            fingerprint,
            writer,
            admission.expect("new request returns its admission capability"),
        )
        .await
        .unwrap();
    let context = state.begin_activation().await.unwrap();

    let capability = repository.capture_capability(media_table_id).unwrap();

    let mut filesystem = FilesystemProvider::with_limits(65_536, 16).unwrap();
    filesystem.allow_root(root).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);
    let media_root = format!("{:?}", root.to_string_lossy().as_ref());
    let source = include_str!("fixtures/media/import-song-annotation.orna")
        .replace("__MEDIA_ROOT__", &media_root);
    let value = evaluate_expression_ovb2_with_effects(
        &source,
        &Default::default(),
        Limits::default(),
        &mut bindings,
    )
    .unwrap_or_else(|error| panic!("authored capture annotation failed: {}", error.code()));

    assert_eq!(value.format(), ValueFormat::Ovb2);
    assert!(
        value.context().is_none(),
        "portable OVB-2 results carry no owner context"
    );
    let blob = value.blob().unwrap();
    assert_eq!(blob.read_to_end().unwrap(), payload);
    assert_eq!(blob.media_type(), "audio/mp4");
    assert_eq!(blob.suffix(), Some("m4b"));

    let round_trip = ContextValue::decode(&value.encode().unwrap(), ValueFormat::Ovb2).unwrap();
    assert_eq!(round_trip.format(), ValueFormat::Ovb2);
    assert!(round_trip.context().is_none());
    let round_trip_blob = round_trip.blob().unwrap();
    assert_eq!(round_trip_blob.content_identity(), blob.content_identity());
    assert_eq!(round_trip_blob.read_to_end().unwrap(), payload);
    assert_eq!(round_trip_blob.media_type(), "audio/mp4");
    assert_eq!(round_trip_blob.suffix(), Some("m4b"));

    // Capture finishes with only owner-scoped scratch state. It has not
    // inserted a runtime row or created a public commit.
    assert_eq!(
        state.committed_table_row("media", b"song").await.unwrap(),
        None
    );
    assert!(state.pending().await.unwrap().is_empty());
    assert_eq!(
        git_output(root, &["rev-parse", "HEAD"], None),
        head_before_capture
    );

    // The authored annotation survives OVB-2 evaluation and row conversion.
    let annotated = value;
    let annotated_blob = annotated.blob().unwrap();
    assert_eq!(annotated_blob.media_type(), "audio/mp4");
    assert_eq!(annotated_blob.suffix(), Some("m4b"));
    assert_eq!(annotated_blob.annotation().selected_suffix(), "m4b");
    // A row-bound Format 3 reference carries identity and annotation without
    // loading captured bytes; row conversion must not hydrate it.
    let (format3_context, resolver_bytes_read) = resolver_context(payload);
    let reference = format3_context
        .reference(
            annotated_blob.content_identity(),
            NativeOid::from_bytes(&[0x66; 20]).unwrap(),
        )
        .unwrap();
    let context_backed_blob =
        Blob::from_reference_with_annotation(reference, "application/octet-stream", None).unwrap();
    assert!(!context_backed_blob.is_hydrated());
    let contextual_value =
        ContextValue::from_blob(&context_backed_blob, ValueFormat::Rov3).unwrap();
    assert!(contextual_value.context().is_some());
    let contextual_blob = contextual_value.blob().unwrap();
    assert_eq!(
        contextual_blob.content_identity(),
        annotated_blob.content_identity()
    );
    assert_eq!(contextual_blob.media_type(), "application/octet-stream");
    assert_eq!(contextual_blob.suffix(), None);
    assert_eq!(resolver_bytes_read.load(Ordering::Relaxed), 0);

    let row_value = contextual_value
        .with_blob_annotation(annotated_blob.media_type(), annotated_blob.suffix())
        .unwrap();
    assert_eq!(row_value.format(), ValueFormat::Rov3);
    assert!(row_value.context().is_some());
    let row_blob = row_value.blob().unwrap();
    assert_eq!(row_blob.media_type(), "audio/mp4");
    assert_eq!(row_blob.suffix(), Some("m4b"));
    assert_eq!(
        row_blob.content_identity(),
        annotated_blob.content_identity()
    );
    assert!(!row_blob.is_hydrated());
    assert_eq!(resolver_bytes_read.load(Ordering::Relaxed), 0);

    let binding = bindings.accept_captured_blob_for_row(&row_value).unwrap();
    assert_eq!(resolver_bytes_read.load(Ordering::Relaxed), 0);
    // ROV-3 stores the descriptor, not a read-capable reference. The original
    // authorized capture reference remains the explicit byte-read path.
    assert_eq!(context_backed_blob.read_to_end().unwrap(), payload);
    assert_eq!(
        resolver_bytes_read.load(Ordering::Relaxed),
        payload.len() as u64
    );
    let expected_row_value = binding.encoded_value().to_vec();
    assert!(
        expected_row_value
            .windows(b"audio/mp4".len())
            .any(|window| window == b"audio/mp4")
    );
    assert!(
        expected_row_value
            .windows(b"m4b".len())
            .any(|window| window == b"m4b")
    );
    let transfer = binding.transfer_record();
    let captured_identity = transfer.content_identity();
    assert_eq!(
        annotated_blob.content_identity().length(),
        captured_identity.length()
    );
    assert_eq!(
        annotated_blob.content_identity().sha256(),
        captured_identity.sha256()
    );
    let pin_ref = format!(
        "refs/orna/pins/{}/pending/{}",
        hex(transfer.owner_id()),
        hex(transfer.pin_id())
    );
    assert_eq!(
        state.committed_table_row("media", b"song").await.unwrap(),
        None
    );
    assert_eq!(
        git_output(root, &["rev-parse", "HEAD"], None),
        head_before_capture
    );

    let mutation = TableMutation::insert(media_table_id, "media", b"song".to_vec(), Vec::new())
        .unwrap()
        .with_orp_blob_binding(binding)
        .unwrap();
    std::fs::remove_file(root.join("tone.wav")).unwrap();
    let competing_context = state.begin_activation().await.unwrap();
    state
        .commit_table_activation(
            writer,
            &competing_context,
            &[TableMutation::insert([0x5b; 16], "other", vec![1], vec![2]).unwrap()],
            [0x5c; 32],
            &orna_runtime_v1::NoFault,
        )
        .await
        .unwrap();
    assert!(matches!(
        state
            .commit_table_request_activation(
                writer,
                request_identity,
                fingerprint,
                &context,
                std::slice::from_ref(&mutation),
                [0x5d; 32],
                TerminalOutcome::new(vec![0x5e]).unwrap(),
                &orna_runtime_v1::NoFault,
            )
            .await,
        Err(RuntimeError::StaleCapture { .. })
    ));
    assert_eq!(
        state.committed_table_row("media", b"song").await.unwrap(),
        None
    );
    assert_eq!(
        state
            .request_status(request_identity, fingerprint)
            .await
            .unwrap()
            .unwrap()
            .state,
        RequestState::Running
    );
    let retry_context = state.begin_activation().await.unwrap();
    let committed = state
        .commit_table_request_activation(
            writer,
            request_identity,
            fingerprint,
            &retry_context,
            &[mutation],
            [0x59; 32],
            TerminalOutcome::new(vec![0x5a]).unwrap(),
            &orna_runtime_v1::NoFault,
        )
        .await
        .unwrap();
    assert_eq!(committed.request.state, RequestState::Completed);
    let committed_value = state
        .committed_table_row("media", b"song")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(committed_value, expected_row_value);
    // Acceptance stores the Blob as a canonical ROV-3 reference, not its OVB-2
    // capture envelope; inspect the native descriptor profile and annotation.
    let committed_blob = decode_rov3_blob_metadata(&committed_value).unwrap();
    assert_eq!(committed_blob.media_type(), "audio/mp4");
    assert_eq!(committed_blob.suffix(), Some("m4b"));
    assert_eq!(committed_blob.length(), transfer.content_identity().length());
    assert_eq!(committed_blob.sha256(), transfer.content_identity().sha256());
    assert_eq!(
        committed_blob.descriptor_oid().map(|oid| oid.as_bytes()),
        Some(transfer.descriptor_oid().as_bytes())
    );
    assert_eq!(
        state.latest_checkpoint().await.unwrap().unwrap().digest,
        [0x59; 32]
    );
    let stored_mutation = state.pending().await.unwrap().pop().unwrap();
    let decoded_mutation = TableMutation::decode(&stored_mutation).unwrap();
    assert_eq!(decoded_mutation.protected_content_transfers().len(), 1);
    assert_eq!(
        decoded_mutation.protected_content_transfers()[0].content_identity(),
        transfer.content_identity()
    );
    assert!(
        Command::new("git")
            .args(["show-ref", "--verify", "--quiet", &pin_ref])
            .current_dir(root)
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        git_output(root, &["rev-parse", "HEAD"], None),
        head_before_capture
    );

    let failed_payload = b"must not be acknowledged before flush";
    std::fs::write(root.join("flush.json"), failed_payload).unwrap();
    let failed_value = evaluate_expression_ovb2_with_effects(
        &format!(
            "sys.blob.capture_file({root:?}, \"flush.json\", 64)",
            root = root.to_string_lossy().as_ref()
        ),
        &Default::default(),
        Limits::default(),
        &mut bindings,
    )
    .unwrap();
    let failed_binding = bindings
        .accept_captured_blob_for_row(&failed_value)
        .unwrap();
    let failed_transfer = failed_binding.transfer_record();
    let failed_pin_ref = format!(
        "refs/orna/pins/{}/pending/{}",
        hex(failed_transfer.owner_id()),
        hex(failed_transfer.pin_id())
    );
    let failed_request = RequestIdentity {
        session_id: [0x61; 16],
        request_id: [0x62; 16],
    };
    let failed_fingerprint = [0x63; 32];
    let (_, failed_admission) = state
        .reserve_request_with_admission(failed_request, failed_fingerprint)
        .await
        .unwrap();
    state
        .start_request_with_owner_and_admission(
            failed_request,
            failed_fingerprint,
            writer,
            failed_admission.expect("new request returns its admission capability"),
        )
        .await
        .unwrap();
    let failed_context = state.begin_activation().await.unwrap();
    let failed_mutation =
        TableMutation::insert([0x64; 16], "captures", b"flush.json".to_vec(), Vec::new())
            .unwrap()
            .with_orp_blob_binding(failed_binding)
            .unwrap();
    assert_eq!(
        state
            .commit_table_request_activation(
                writer,
                failed_request,
                failed_fingerprint,
                &failed_context,
                &[failed_mutation],
                [0x65; 32],
                TerminalOutcome::new(vec![0x66]).unwrap(),
                &FailAt(FaultPoint::AfterMutation),
            )
            .await,
        Err(RuntimeError::FaultInjected(FaultPoint::AfterMutation))
    );
    assert_eq!(
        state
            .committed_table_row("captures", b"flush.json")
            .await
            .unwrap(),
        None
    );
    assert_eq!(state.pending().await.unwrap().len(), 2);
    assert_eq!(
        state.latest_checkpoint().await.unwrap().unwrap().digest,
        [0x59; 32]
    );
    assert_eq!(
        state
            .request_status(failed_request, failed_fingerprint)
            .await
            .unwrap()
            .unwrap()
            .state,
        RequestState::Running
    );
    assert!(
        Command::new("git")
            .args(["show-ref", "--verify", "--quiet", &failed_pin_ref])
            .current_dir(root)
            .status()
            .unwrap()
            .success()
    );

    drop(state);
    let state = RuntimeState::open(&repository, runtime_identity, [0x53; 32])
        .await
        .unwrap();
    assert_eq!(
        state.committed_table_row("media", b"song").await.unwrap(),
        Some(expected_row_value)
    );
    assert_eq!(
        state
            .committed_table_row("captures", b"flush.json")
            .await
            .unwrap(),
        None
    );
    assert_eq!(state.pending().await.unwrap().len(), 2);
    assert!(
        Command::new("git")
            .args(["show-ref", "--verify", "--quiet", &failed_pin_ref])
            .current_dir(root)
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        git_output(root, &["rev-parse", "HEAD"], None),
        head_before_capture
    );

    std::fs::write(root.join("capture.json"), payload).unwrap();
    assert_eq!(
        capture_failure(&mut bindings, "/not/authorized", "capture.json", 64),
        "ORNA-INGEST-001"
    );
    assert_eq!(
        capture_failure(&mut bindings, root.to_str().unwrap(), "../capture.json", 64),
        "ORNA-INGEST-002"
    );
    assert_eq!(
        capture_failure(&mut bindings, root.to_str().unwrap(), "missing", 64),
        "ORNA-INGEST-003"
    );
    std::fs::create_dir(root.join("directory")).unwrap();
    assert_eq!(
        capture_failure(&mut bindings, root.to_str().unwrap(), "directory", 64),
        "ORNA-INGEST-003"
    );
    assert_eq!(
        capture_failure(&mut bindings, root.to_str().unwrap(), "capture.json", 4),
        "ORNA-INGEST-004"
    );
}

struct FixtureResolver {
    identity: ContentIdentity,
    payload: Arc<[u8]>,
    bytes_read: Arc<AtomicU64>,
}

impl BlobResolver for FixtureResolver {
    fn read_range(
        &self,
        reference: &ContentReference,
        offset: u64,
        length: u64,
    ) -> orna_value_v1::Result<Vec<u8>> {
        if reference.identity() != self.identity {
            return Err(BlobError::ContentUnavailable);
        }
        let start = usize::try_from(offset).map_err(|_| BlobError::InvalidRange)?;
        let length = usize::try_from(length).map_err(|_| BlobError::InvalidRange)?;
        let end = start.checked_add(length).ok_or(BlobError::InvalidRange)?;
        let bytes = self
            .payload
            .get(start..end)
            .ok_or(BlobError::InvalidRange)?;
        self.bytes_read.fetch_add(length as u64, Ordering::Relaxed);
        Ok(bytes.to_vec())
    }
}

fn resolver_context(payload: &[u8]) -> (Format3Context, Arc<AtomicU64>) {
    let database = [0x67; 16];
    let bytes_read = Arc::new(AtomicU64::new(0));
    let context = Format3Context::from_persisted(
        database,
        Snapshot::cwd(database, [0x68; 16], 0.into()).unwrap(),
        GitHash::Sha1,
        NativeOid::from_bytes(&[0x69; 20]).unwrap(),
        NativeOid::from_bytes(&[0x6a; 20]).unwrap(),
        AvailabilityState::Available,
        OwnerLifetime::new([0x6b; 16]),
        Format3Quota::new(65_536, 1_u64 << 32).unwrap(),
        Arc::new(FixtureResolver {
            identity: ContentIdentity::from_bytes(payload),
            payload: Arc::from(payload),
            bytes_read: Arc::clone(&bytes_read),
        }),
    )
    .unwrap();
    (context, bytes_read)
}

struct FailAt(FaultPoint);

impl FaultInjector for FailAt {
    fn check(&self, point: FaultPoint) -> Result<(), RuntimeError> {
        if point == self.0 {
            Err(RuntimeError::FaultInjected(point))
        } else {
            Ok(())
        }
    }
}

#[path = "support/format3.rs"]
mod format3;
use format3::*;
