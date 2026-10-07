use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
    sync::Arc,
};

use orna_evaluator_v1::{Limits, SysHostBindingRegistry, evaluate_expression_ovb2_with_effects};
use orna_repository_v1::{Repository, RepositoryCaptureCapability};
use orna_runtime_v1::{
    FaultInjector, FaultPoint, RequestIdentity, RequestState, RuntimeError, RuntimeIdentity,
    RuntimeState, TableMutation, TerminalOutcome,
};
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};
use orna_value_v1::{ContextValue, ValueFormat};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

#[tokio::test]
async fn cli_consumer_commits_captured_annotated_blob_with_its_request() {
    let (directory, repository, relation_id) = empty_format3_repository();
    let root = directory.path();
    let payload = b"{\"source\":\"cli\"}";
    std::fs::write(root.join("capture.json"), payload).unwrap();
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

    let format = repository.open_format_context().unwrap();
    let row_map = format.load_row_map(relation_id).unwrap();
    let graph = Arc::new(format.open_native_graph(&row_map).unwrap());
    let scope = graph.open_read_scope().unwrap();
    let capability = RepositoryCaptureCapability::new(graph, scope).unwrap();

    let mut filesystem = FilesystemProvider::with_limits(1024, 16).unwrap();
    filesystem.allow_root(root).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);
    let source = format!(
        "sys.blob.capture_file({root:?}, \"capture.json\", 64)",
        root = root.to_string_lossy().as_ref()
    );
    let value = evaluate_expression_ovb2_with_effects(
        &source,
        &Default::default(),
        Limits::default(),
        &mut bindings,
    )
    .unwrap();

    assert_eq!(value.format(), ValueFormat::Ovb2);
    let blob = value.blob().unwrap();
    assert_eq!(blob.read_to_end().unwrap(), payload);
    assert_eq!(blob.media_type(), "application/json");
    assert_eq!(blob.suffix(), None);

    let round_trip = ContextValue::decode(&value.encode().unwrap(), ValueFormat::Ovb2).unwrap();
    let round_trip_blob = round_trip.blob().unwrap();
    assert_eq!(round_trip_blob.read_to_end().unwrap(), payload);
    assert_eq!(round_trip_blob.media_type(), "application/json");
    assert_eq!(round_trip_blob.suffix(), None);

    // Capture finishes with only owner-scoped scratch state. It has not
    // inserted a runtime row or created a public commit.
    assert_eq!(
        state
            .committed_table_row("captures", b"capture.json")
            .await
            .unwrap(),
        None
    );
    assert!(state.pending().await.unwrap().is_empty());
    assert_eq!(git_output(root, &["rev-parse", "HEAD"], None), head_before_capture);

    let binding = bindings.accept_captured_blob_for_row(&value).unwrap();
    let expected_row_value = binding.encoded_value().to_vec();
    let transfer = binding.transfer_record();
    let pin_ref = format!(
        "refs/orna/pins/{}/pending/{}",
        hex(transfer.owner_id()),
        hex(transfer.pin_id())
    );
    assert_eq!(
        state
            .committed_table_row("captures", b"capture.json")
            .await
            .unwrap(),
        None
    );
    assert_eq!(git_output(root, &["rev-parse", "HEAD"], None), head_before_capture);

    let mutation = TableMutation::insert(
        [0x58; 16],
        "captures",
        b"capture.json".to_vec(),
        Vec::new(),
    )
    .unwrap()
    .with_orp_blob_binding(binding)
    .unwrap();
    std::fs::remove_file(root.join("capture.json")).unwrap();
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
        state
            .committed_table_row("captures", b"capture.json")
            .await
            .unwrap(),
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
    assert_eq!(
        state
            .committed_table_row("captures", b"capture.json")
            .await
            .unwrap(),
        Some(expected_row_value.clone())
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
    assert!(Command::new("git")
        .args(["show-ref", "--verify", "--quiet", &pin_ref])
        .current_dir(root)
        .status()
        .unwrap()
        .success());
    assert_eq!(git_output(root, &["rev-parse", "HEAD"], None), head_before_capture);

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
    let failed_binding = bindings.accept_captured_blob_for_row(&failed_value).unwrap();
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
    let failed_mutation = TableMutation::insert(
        [0x64; 16],
        "captures",
        b"flush.json".to_vec(),
        Vec::new(),
    )
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
    assert!(Command::new("git")
        .args(["show-ref", "--verify", "--quiet", &failed_pin_ref])
        .current_dir(root)
        .status()
        .unwrap()
        .success());

    drop(state);
    let state = RuntimeState::open(&repository, runtime_identity, [0x53; 32])
        .await
        .unwrap();
    assert_eq!(
        state
            .committed_table_row("captures", b"capture.json")
            .await
            .unwrap(),
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
    assert!(Command::new("git")
        .args(["show-ref", "--verify", "--quiet", &failed_pin_ref])
        .current_dir(root)
        .status()
        .unwrap()
        .success());
    assert_eq!(git_output(root, &["rev-parse", "HEAD"], None), head_before_capture);

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
