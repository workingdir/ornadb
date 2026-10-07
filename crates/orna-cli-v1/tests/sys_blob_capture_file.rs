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

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn capture_failure(
    bindings: &mut SysHostBindingRegistry,
    root: &str,
    path: &str,
    max_bytes: u64,
) -> String {
    let source = format!("sys.blob.capture_file({root:?}, {path:?}, {max_bytes})");
    evaluate_expression_ovb2_with_effects(&source, &Default::default(), Limits::default(), bindings)
        .unwrap_err()
        .code()
        .to_owned()
}

fn empty_format3_repository() -> (TempDir, Repository, [u8; 16]) {
    let directory = TempDir::new().unwrap();
    let root = directory.path();
    git(root, &["init", "--quiet"]);
    git(
        root,
        &["config", "user.email", "kieran@drewett.dev"],
    );
    git(root, &["config", "user.name", "kierandrewett"]);
    git(root, &["config", "commit.gpgsign", "false"]);

    let database_id = [0, 0, 0, 0, 0, 0, 0x40, 0, 0x80, 0, 0, 0, 0, 0, 0, 1];
    let relation_id = [0x43; 16];
    let database_uuid = "00000000-0000-4000-8000-000000000001";
    let database =
        format!("{{\n    repository_format: 3,\n    database_id: \"{database_uuid}\",\n}}\n");
    let schema_source = b"capture test schema";
    std::fs::create_dir_all(root.join(".orna/store")).unwrap();
    std::fs::write(root.join(".orna/database.orna"), &database).unwrap();
    std::fs::write(root.join(".orna/store/data"), b"store marker").unwrap();
    std::fs::write(root.join("main.orna"), schema_source).unwrap();
    git(root, &["add", "--all"]);
    git(
        root,
        &["commit", "--quiet", "-m", "format3 capture fixture"],
    );

    let schema_digest: [u8; 32] = Sha256::digest(schema_source).into();
    let chunk_oid = git_object(root, "blob", schema_source);
    let mut byte_index = vec![0x85, 0x01, 0x04, 0x00];
    append_cbor_head(&mut byte_index, 0, schema_source.len());
    byte_index.push(0x81);
    byte_index.push(0x83);
    append_cbor_head(&mut byte_index, 0, schema_source.len());
    append_cbor_bytes(&mut byte_index, &git_oid_bytes(&chunk_oid));
    append_cbor_bytes(&mut byte_index, &schema_digest);
    let byte_index_oid = native_fixture_node(root, &byte_index, &[(chunk_oid.as_str(), "blob")]);

    let mut schema = vec![0x85, 0x01, 0x06];
    append_cbor_head(&mut schema, 0, schema_source.len());
    append_cbor_bytes(&mut schema, &schema_digest);
    append_cbor_bytes(&mut schema, &git_oid_bytes(&byte_index_oid));
    let schema_oid = native_fixture_node(root, &schema, &[(byte_index_oid.as_str(), "tree")]);

    let mut row_domain = vec![0x83, 0x64];
    row_domain.extend_from_slice(b"rows");
    append_cbor_bytes(&mut row_domain, &relation_id);
    append_cbor_bytes(&mut row_domain, &schema_digest);
    let mut empty_rows = vec![0x84, 0x01, 0x01];
    empty_rows.extend_from_slice(&row_domain);
    empty_rows.push(0x80);
    let row_root_oid = native_fixture_node(root, &empty_rows, &[]);

    let mut relation_domain = vec![0x82, 0x69];
    relation_domain.extend_from_slice(b"relations");
    append_cbor_bytes(&mut relation_domain, &database_id);
    let mut relation_map = vec![0x84, 0x01, 0x01];
    relation_map.extend_from_slice(&relation_domain);
    relation_map.push(0x81);
    relation_map.push(0x82);
    append_cbor_bytes(&mut relation_map, &relation_id);
    relation_map.push(0x84);
    append_cbor_bytes(&mut relation_map, &git_oid_bytes(&schema_oid));
    append_cbor_bytes(&mut relation_map, &git_oid_bytes(&row_root_oid));
    relation_map.push(0xf6);
    relation_map.push(0x00);
    let relation_map_oid = native_fixture_node(
        root,
        &relation_map,
        &[
            (schema_oid.as_str(), "tree"),
            (row_root_oid.as_str(), "tree"),
        ],
    );

    let mut store_root = vec![0x83, 0x01, 0x00];
    append_cbor_bytes(&mut store_root, &git_oid_bytes(&relation_map_oid));
    let store_root_oid =
        native_fixture_node(root, &store_root, &[(relation_map_oid.as_str(), "tree")]);
    let database_oid = git_object(root, "blob", database.as_bytes());
    let source_oid = git_object(root, "blob", schema_source);
    let orna_tree = git_tree(
        root,
        &format!(
            "100644 blob {database_oid}\tdatabase.orna\n040000 tree {store_root_oid}\tstore\n"
        ),
    );
    let root_tree = git_tree(
        root,
        &format!("040000 tree {orna_tree}\t.orna\n100644 blob {source_oid}\tmain.orna\n"),
    );
    let parent = String::from_utf8(git_output(root, &["rev-parse", "HEAD"], None))
        .unwrap()
        .trim()
        .to_owned();
    let commit = String::from_utf8(git_output(
        root,
        &[
            "commit-tree",
            &root_tree,
            "-p",
            &parent,
            "-m",
            "format3 graph fixture",
        ],
        None,
    ))
    .unwrap()
    .trim()
    .to_owned();
    let head_ref = String::from_utf8(git_output(root, &["symbolic-ref", "HEAD"], None))
        .unwrap()
        .trim()
        .to_owned();
    git(root, &["update-ref", &head_ref, &commit]);

    let repository = Repository::discover(root).unwrap();
    (directory, repository, relation_id)
}

fn git(path: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .unwrap()
            .success()
    );
}

fn git_output(path: &Path, args: &[&str], input: Option<&[u8]>) -> Vec<u8> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(path)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = input {
        child.stdin.take().unwrap().write_all(input).unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn git_object(path: &Path, kind: &str, bytes: &[u8]) -> String {
    String::from_utf8(git_output(
        path,
        &["hash-object", "-w", "-t", kind, "--stdin"],
        Some(bytes),
    ))
    .unwrap()
    .trim()
    .to_owned()
}

fn git_tree(path: &Path, entries: &str) -> String {
    String::from_utf8(git_output(path, &["mktree"], Some(entries.as_bytes())))
        .unwrap()
        .trim()
        .to_owned()
}

fn native_fixture_node(path: &Path, data: &[u8], dependencies: &[(&str, &str)]) -> String {
    let data_oid = git_object(path, "blob", data);
    let mut dependencies = dependencies.to_vec();
    dependencies.sort_by_key(|(oid, _)| *oid);
    let refs_oid = if dependencies.is_empty() {
        None
    } else {
        let refs = dependencies
            .iter()
            .map(|(oid, kind)| {
                let mode = if *kind == "tree" { "040000" } else { "100644" };
                format!("{mode} {kind} {oid}\t{oid}\n")
            })
            .collect::<String>();
        Some(git_tree(path, &refs))
    };
    let mut envelope = format!("100644 blob {data_oid}\tdata\n");
    if let Some(refs_oid) = refs_oid {
        envelope.push_str(&format!("040000 tree {refs_oid}\trefs\n"));
    }
    git_tree(path, &envelope)
}

fn append_cbor_head(output: &mut Vec<u8>, major: u8, value: usize) {
    let prefix = major << 5;
    match value {
        0..=23 => output.push(prefix | value as u8),
        24..=255 => output.extend_from_slice(&[prefix | 24, value as u8]),
        _ => panic!("fixture CBOR value is unexpectedly large"),
    }
}

fn append_cbor_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
    append_cbor_head(output, 2, bytes.len());
    output.extend_from_slice(bytes);
}

fn git_oid_bytes(oid: &str) -> Vec<u8> {
    oid.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let hex = std::str::from_utf8(pair).unwrap();
            u8::from_str_radix(hex, 16).unwrap()
        })
        .collect()
}
