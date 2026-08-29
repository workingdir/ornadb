use super::inspect::*;
use super::*;
use orna_client::{
    ClientResourceInvocationContext, ClientResourceKey, ClientResourceRequest, ClientStateStore,
};
use orna_core::{
    CallSiteId, CatalogueRevisionId, FunctionId, InvocationId, ParameterId, PrincipalId, SchemaId,
    SecurityAuditEventId, SourceBundleId, SourceRevisionId,
    canonical_hash::{catalogue_digest, source_bundle_digest, source_revision_record_digest},
    catalogue::{
        CatalogueSnapshot, FunctionDomain, FunctionReturn, FunctionSecurity, FunctionVolatility,
        ParameterDefinition, SchemaDefinition, ValueTypeDefinition, ValueTypeMutability,
        ValueTypePersistence,
    },
    inspect::{InspectSnapshotEpoch, InspectSnapshotOptions, InspectSnapshotSummary},
    invocation::{
        InvocationFailure, InvocationFailurePhase, InvocationParameterSelector,
        InvocationRetryability, InvokeEvent, InvokeValue,
    },
    revision::{
        ActiveDatabaseRevisionInput, ActiveRevisionContent, CatalogueHashContext, RevisionPair,
        Sha256Digest, StoredSourceRevision,
    },
    security::InvocationTarget as ClientInvocationTarget,
    types::{ResolvedType, StandardScalar},
    value::{FunctionArgument, RuntimeValue},
};
use orna_protocol::{
    EventRecord, InvocationEventBatch, InvocationEventRecord, ResourceAccepted, ResourceCancelled,
    ResourceCompleted, ResourceFailed, ResourceValues, decode_resource_client_frame,
    encode_constructed_server_frame, encode_resource_server_frame, encode_session_server_frame,
};
use orna_standard::{
    STD_UI_TYPE_ID, retained_standard_library_snapshot, verify_standard_library_snapshot,
};
use std::io::{Read, Write};

#[cfg(unix)]
use std::{fs, os::unix::net::UnixListener, thread};

const ENCODED_VALUE: &[u8] = b"ORV5-encoded-value";

#[test]
fn session_bridge_rejects_crossed_response_identity() {
    let root = InvocationId::from_bytes([0x41; 16]);
    let bridge = SessionBridge::new(root, 7).expect("session bridge creates");
    let waiting_bridge = Arc::clone(&bridge);
    let waiter = std::thread::spawn(move || waiting_bridge.request_input(root));
    let request = loop {
        if let Some(SessionServerFrame::InputRequested(request)) = bridge.try_take_outbound() {
            break request;
        }
        std::thread::yield_now();
    };
    let crossed = SessionClientFrame::InputLine {
        root_invocation_id: InvocationId::from_bytes([0x42; 16]),
        call_stream: 7,
        request_invocation_id: request.request_invocation_id,
        line: "wrong root".to_owned(),
    };
    assert_eq!(
        bridge.accept_response(crossed),
        Err(SessionStateError::MismatchedIdentity)
    );
    bridge
        .accept_response(SessionClientFrame::InputLine {
            root_invocation_id: root,
            call_stream: 7,
            request_invocation_id: request.request_invocation_id,
            line: "accepted".to_owned(),
        })
        .expect("matching response accepted");
    assert_eq!(
        waiter
            .join()
            .expect("input waiter joins")
            .expect("input succeeds"),
        "accepted"
    );
}

#[test]
fn session_bridge_close_before_request_does_not_publish_fake_response() {
    let bridge = SessionBridge::new(InvocationId::from_bytes([0x43; 16]), 7)
        .expect("session bridge creates");
    bridge.close();
    assert!(bridge.try_take_outbound().is_none());
    let error = bridge
        .request_input(InvocationId::from_bytes([0x43; 16]))
        .expect_err("closed bridge rejects input");
    assert_eq!(error, "client.input_unavailable");
}
#[cfg(unix)]
#[test]
fn local_socket_connector_attaches_to_a_listener() {
    let socket_path =
        std::env::temp_dir().join(format!("orna-invoke-connector-{}.sock", std::process::id()));
    let _ = fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path).expect("test Unix listener");
    let server = thread::spawn(move || listener.accept().expect("test Unix connection"));

    let client = connect_local_socket(&socket_path).expect("connect to local Orna socket");
    drop(client);
    server.join().expect("local socket listener");
    fs::remove_file(socket_path).expect("remove test Unix socket");
}

#[test]
fn endpoint_transport_accepts_only_the_current_managed_socket() {
    assert!(matches!(
        endpoint_transport(&DatabaseEndpoint::managed_local()),
        Ok(InvokeTransport::UnixSocket(_)),
    ));

    let custom = DatabaseEndpoint::UnixSocket {
        path: PathBuf::from("/tmp/another-orna.sock"),
    };
    let custom_error = endpoint_transport(&custom).expect_err("custom socket must fail closed");
    assert_eq!(
        custom_error.to_string(),
        "orna: invoke: this Unix socket is not the current managed Orna instance",
    );

    let remote = DatabaseEndpoint::RemoteTls {
        host: "db.example.test".to_owned(),
        port: 7443,
        database: "default".to_owned(),
    };
    let remote_error = endpoint_transport(&remote).expect_err("remote transport is not wired");
    assert_eq!(
        remote_error.to_string(),
        "orna: invoke: remote Orna URIs need TLS session bootstrap and are not available yet",
    );
}

fn encoded_record() -> Vec<u8> {
    [ENCODED_VALUE, b"\n"].concat()
}

fn encoder(value: &RuntimeValue) -> Result<Vec<u8>, InstalledInvokeError> {
    let _ = value;
    Ok(ENCODED_VALUE.to_vec())
}

fn echo_events() -> InvocationEventBatch {
    let invocation = InvocationId::new();
    let started = InvokeEvent::new(
        invocation,
        0,
        InvocationEventBody::Started {
            visible_principal: None,
        },
    )
    .expect("started event");
    let values = InvokeEvent::new(
        invocation,
        1,
        InvocationEventBody::value_batch(
            None,
            [InvokeValue::new(RuntimeValue::Integer(41)).expect("integer value")],
        )
        .expect("value batch body"),
    )
    .expect("values event");
    let completed = InvokeEvent::new(
        invocation,
        2,
        InvocationEventBody::Completed {
            duration_nanoseconds: 7,
        },
    )
    .expect("completed event");
    InvocationEventBatch::new(vec![
        InvocationEventRecord::new(1, started),
        InvocationEventRecord::new(2, values),
        InvocationEventRecord::new(3, completed),
    ])
    .expect("event batch")
}

#[tokio::test]
async fn shared_broker_reader_preserves_session_tag_and_payload_boundary() {
    let (mut client, mut server) = tokio::io::duplex(256);
    let request = SessionServerFrame::InputRequested(InputRequested {
        root_invocation_id: InvocationId::from_bytes([0x51; 16]),
        call_stream: 7,
        request_invocation_id: InvocationId::from_bytes([0x52; 16]),
        prompt: "orna> ".to_owned(),
    });
    let encoded = encode_session_server_frame(&request).expect("session request encodes");
    client
        .write_all(&encoded)
        .await
        .expect("session request writes");
    let decoded = read_shared_broker_frame(&mut server)
        .await
        .expect("frame reads");
    assert!(!decoded.resource);
    assert_eq!(
        decode_session_server_frame(&decoded.bytes).expect("session request decodes"),
        request
    );
}

#[tokio::test]
async fn shared_broker_reader_retains_partial_frames_across_polling() {
    let (mut client, server) = tokio::io::duplex(64);
    let (sender, mut receiver) = mpsc::channel(1);
    let reader = tokio::spawn(read_shared_broker_frames(server, sender));
    let frame = [0_u8; 18];
    client
        .write_all(&frame[..5])
        .await
        .expect("partial frame write");
    assert!(
        tokio::time::timeout(Duration::from_millis(10), receiver.recv())
            .await
            .is_err()
    );
    client
        .write_all(&frame[5..])
        .await
        .expect("frame remainder write");
    let decoded = tokio::time::timeout(Duration::from_secs(1), receiver.recv())
        .await
        .expect("frame arrives")
        .expect("reader remains connected")
        .expect("valid frame");
    assert_eq!(decoded.bytes, frame);
    reader.abort();
    let _ = reader.await;
}

#[tokio::test]
async fn broker_cleanup_completion_signal_does_not_wait_for_full_queue() {
    let (sender, mut receiver) = mpsc::channel::<
        Result<ResourceTransportOutcome, ResourceTransportFailure>,
    >(BROKER_RESOURCE_COMPLETION_CAPACITY);
    for _ in 0..BROKER_RESOURCE_COMPLETION_CAPACITY {
        sender
            .send(Ok(ResourceTransportOutcome::StreamCompleted {
                nested_invocation_id: InvocationId::from_bytes([0x40; 16]),
            }))
            .await
            .expect("queue accepts its configured capacity");
    }

    tokio::time::timeout(Duration::from_millis(100), async {
        signal_broker_resource_cleanup(sender)
    })
    .await
    .expect("cleanup signal must not wait for a receiver");

    for _ in 0..BROKER_RESOURCE_COMPLETION_CAPACITY {
        assert!(matches!(
            receiver.recv().await,
            Some(Ok(ResourceTransportOutcome::StreamCompleted {
                nested_invocation_id,
            })) if nested_invocation_id == InvocationId::from_bytes([0x40; 16])
        ));
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(100), receiver.recv())
            .await
            .expect("full cleanup queue must close after its backlog drains")
            .is_none()
    );

    let (sender, mut receiver) = mpsc::channel(BROKER_RESOURCE_COMPLETION_CAPACITY);
    sender
        .send(Ok(ResourceTransportOutcome::StreamCompleted {
            nested_invocation_id: InvocationId::from_bytes([0x41; 16]),
        }))
        .await
        .expect("queue accepts a buffered terminal outcome");
    signal_broker_resource_cleanup(sender);
    assert!(matches!(
        receiver.recv().await,
        Some(Ok(ResourceTransportOutcome::StreamCompleted {
            nested_invocation_id,
        })) if nested_invocation_id == InvocationId::from_bytes([0x41; 16])
    ));
    assert!(matches!(
        receiver.recv().await,
        Some(Err(ResourceTransportFailure::Transport))
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), receiver.recv())
            .await
            .expect("cleanup queue must close after publishing transport failure")
            .is_none()
    );
}

#[tokio::test]
async fn broker_stream_completion_queue_is_finite() {
    let (sender, mut receiver) = mpsc::channel::<
        Result<ResourceTransportOutcome, ResourceTransportFailure>,
    >(BROKER_RESOURCE_COMPLETION_CAPACITY);
    for _ in 0..BROKER_RESOURCE_COMPLETION_CAPACITY {
        sender
            .send(Ok(ResourceTransportOutcome::StreamCompleted {
                nested_invocation_id: InvocationId::from_bytes([0x40; 16]),
            }))
            .await
            .expect("queue accepts its configured capacity");
    }
    assert!(
        tokio::time::timeout(
            Duration::from_millis(10),
            sender.send(Ok(ResourceTransportOutcome::StreamCompleted {
                nested_invocation_id: InvocationId::from_bytes([0x40; 16]),
            })),
        )
        .await
        .is_err()
    );
    let _ = receiver.recv().await;
}

#[tokio::test]
async fn shared_broker_drops_known_terminal_frames_and_rejects_unknown_streams() {
    let (active, registry) = transport_test_context();
    let request = transport_test_request(active.pair(), 1);
    let accepted = ResourceAccepted {
        stream_id: request.stream_id,
        request_id: request.request_id,
        nested_invocation_id: InvocationId::from_bytes([0x40; 16]),
        target_revision: request.target_revision,
        resource_kind: request.resource_kind,
    };
    let value = RuntimeValue::Integer(7);
    let byte_count = encode_constructed_value(&active, &registry, &value)
        .expect("encoded resource value")
        .len() as u32;
    let values = ResourceValues {
        stream_id: request.stream_id,
        request_id: request.request_id,
        target_revision: active.pair(),
        batch_sequence: 0,
        item_count: 1,
        byte_count,
        values: vec![value],
    };
    let completed = ResourceCompleted {
        stream_id: request.stream_id,
        request_id: request.request_id,
        target_revision: active.pair(),
        final_batch_sequence: 0,
        total_items: 1,
    };
    let protocol = {
        let mut protocol = ResourceProtocolConnection::new();
        protocol
            .open(request.clone())
            .expect("resource request opens");
        protocol
    };
    let (completion, mut completions) = mpsc::channel(2);
    let mut resources = BTreeMap::from([(
        request.stream_id,
        BrokerResourceState {
            request: request.clone(),
            expected_type: ResolvedType::Scalar(StandardScalar::Integer),
            resource_kind: ProtocolResourceKind::Single,
            protocol,
            completion,
            accepted: false,
            accepted_nested_invocation_id: None,
            scalar_value: None,
            cancellation_requested: false,
            stream_values_seen: false,
            terminal_provenance: ResourceTerminalProvenance::Uncommitted,
            scalar_value_after_cancellation: false,
        },
    )]);
    let mut tombstones = BrokerResourceTombstones::new();
    let mut root = None;
    let (_reader, mut writer) = tokio::io::duplex(128);

    for frame in [
        ResourceServerFrame::Accepted(accepted),
        ResourceServerFrame::Values(values),
        ResourceServerFrame::Completed(completed),
    ] {
        let bytes = encode_resource_server_frame(&active, &registry, &frame)
            .expect("encoded resource response");
        handle_shared_broker_frame(
            BrokerWireFrame {
                resource: true,
                bytes,
            },
            &mut writer,
            &active,
            &registry,
            &mut root,
            &mut resources,
            Some(request.stream_id),
            &mut tombstones,
            &Arc::new(Mutex::new(BTreeMap::new())),
        )
        .await
        .expect("valid resource response");
    }
    assert!(resources.is_empty());
    assert_eq!(tombstones.len(), 1);
    assert!(matches!(
        completions.recv().await,
        Some(Ok(ResourceTransportOutcome::Ready {
            value: RuntimeValue::Integer(7),
            nested_invocation_id,
        })) if nested_invocation_id == InvocationId::from_bytes([0x40; 16])
    ));

    let late_bytes = encode_resource_server_frame(
        &active,
        &registry,
        &ResourceServerFrame::Completed(completed),
    )
    .expect("encoded late resource response");
    handle_shared_broker_frame(
        BrokerWireFrame {
            resource: true,
            bytes: late_bytes,
        },
        &mut writer,
        &active,
        &registry,
        &mut root,
        &mut resources,
        Some(request.stream_id),
        &mut tombstones,
        &Arc::new(Mutex::new(BTreeMap::new())),
    )
    .await
    .expect("late terminal response is dropped");
    assert!(completions.try_recv().is_err());
    assert!(resources.is_empty());
    assert_eq!(tombstones.len(), 1);

    let mismatched_bytes = encode_resource_server_frame(
        &active,
        &registry,
        &ResourceServerFrame::Completed(ResourceCompleted {
            request_id: InvocationId::from_bytes([0xaa; 16]),
            target_revision: active.pair(),
            ..completed
        }),
    )
    .expect("encoded mismatched late resource response");
    assert!(matches!(
        handle_shared_broker_frame(
            BrokerWireFrame {
                resource: true,
                bytes: mismatched_bytes,
            },
            &mut writer,
            &active,
            &registry,
            &mut root,
            &mut resources,
            Some(request.stream_id),
            &mut tombstones,
            &Arc::new(Mutex::new(BTreeMap::new())),
        )
        .await,
        Err(ResourceTransportFailure::Shape)
    ));

    let unknown_request = transport_test_request(active.pair(), 2);
    let unknown_bytes = encode_resource_server_frame(
        &active,
        &registry,
        &ResourceServerFrame::Failed(ResourceFailed {
            stream_id: unknown_request.stream_id,
            request_id: unknown_request.request_id,
            target_revision: active.pair(),
            failure: CallFailure::ExecuteDenied,
        }),
    )
    .expect("encoded unknown resource response");
    assert!(matches!(
        handle_shared_broker_frame(
            BrokerWireFrame {
                resource: true,
                bytes: unknown_bytes,
            },
            &mut writer,
            &active,
            &registry,
            &mut root,
            &mut resources,
            Some(request.stream_id),
            &mut tombstones,
            &Arc::new(Mutex::new(BTreeMap::new())),
        )
        .await,
        Err(ResourceTransportFailure::Shape)
    ));
}

#[tokio::test]
async fn shared_broker_local_resource_protocol_failure_preserves_other_streams() {
    let (active, registry) = transport_test_context();
    let request_a = transport_test_request(active.pair(), 1);
    let request_b = transport_test_request(active.pair(), 2);
    let make_state =
        |request: ResourceRequest,
         completion: Sender<Result<ResourceTransportOutcome, ResourceTransportFailure>>| {
            let mut protocol = ResourceProtocolConnection::new();
            protocol
                .open(request.clone())
                .expect("resource request opens");
            BrokerResourceState {
                request,
                expected_type: ResolvedType::Scalar(StandardScalar::Integer),
                resource_kind: ProtocolResourceKind::Single,
                protocol,
                completion,
                accepted: false,
                accepted_nested_invocation_id: None,
                scalar_value: None,
                cancellation_requested: false,
                stream_values_seen: false,
                terminal_provenance: ResourceTerminalProvenance::Uncommitted,
                scalar_value_after_cancellation: false,
            }
        };
    let (completion_a, mut completions_a) = mpsc::channel(2);
    let (completion_b, mut completions_b) = mpsc::channel(2);
    let mut resources = BTreeMap::from([
        (
            request_a.stream_id,
            make_state(request_a.clone(), completion_a),
        ),
        (
            request_b.stream_id,
            make_state(request_b.clone(), completion_b),
        ),
    ]);
    let (root_response, _root_receiver) = tokio::sync::oneshot::channel();
    let mut root = Some(BrokerRootState {
        invocation: Some(InvocationId::from_bytes([0x55; 16])),
        records: Vec::new(),
        response: root_response,
    });
    let mut tombstones = BrokerResourceTombstones::new();
    let (_reader, mut writer) = tokio::io::duplex(256);
    let wrong_revision = RevisionPair::new(
        SourceRevisionId::from_bytes([0x91; 16]),
        CatalogueRevisionId::from_bytes([0x92; 16]),
    );
    let bytes = encode_resource_server_frame(
        &active,
        &registry,
        &ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id: request_a.stream_id,
            request_id: request_a.request_id,
            nested_invocation_id: InvocationId::from_bytes([0x40; 16]),
            target_revision: wrong_revision,
            resource_kind: request_a.resource_kind,
        }),
    )
    .expect("encoded mismatched resource acceptance");

    handle_shared_broker_frame(
        BrokerWireFrame {
            resource: true,
            bytes,
        },
        &mut writer,
        &active,
        &registry,
        &mut root,
        &mut resources,
        Some(request_b.stream_id),
        &mut tombstones,
        &Arc::new(Mutex::new(BTreeMap::new())),
    )
    .await
    .expect("request-local resource failure keeps broker alive");

    assert!(!resources.contains_key(&request_a.stream_id));
    assert!(resources.contains_key(&request_b.stream_id));
    assert!(root.is_some());
    assert_eq!(
        tombstones.get(&request_a.stream_id),
        Some(&request_a.request_id)
    );
    assert!(matches!(
        completions_a.recv().await,
        Some(Err(ResourceTransportFailure::Shape))
    ));
    assert!(completions_b.try_recv().is_err());
    let credit = resources
        .get(&request_b.stream_id)
        .expect("unaffected resource remains")
        .protocol
        .resource_credit(request_b.stream_id, request_b.request_id)
        .expect("unaffected resource credit remains available");
    assert_eq!(credit.item_available, request_b.item_window);
    assert_eq!(credit.byte_available, request_b.byte_window);
}

#[test]
fn shared_broker_resource_expectations_require_exact_request_identity() {
    let (active, _) = transport_test_context();
    let (broker, _receiver) = SharedInvokeBroker::pending();
    let request = transport_test_request(active.pair(), 1);
    assert!(broker.register_expected_resource_request(&request));
    assert!(!broker.register_expected_resource_request(&request));

    let mut mismatched = request.clone();
    mismatched.generation += 1;
    assert!(!broker.take_expected_resource_request(&mismatched));
    assert!(broker.take_expected_resource_request(&request));
    assert!(!broker.take_expected_resource_request(&request));
}

#[test]
fn shared_broker_terminal_tombstones_are_bounded() {
    let mut tombstones = BrokerResourceTombstones::new();
    for stream_id in 1..=(BROKER_RESOURCE_TOMBSTONE_CAPACITY as u64 + 1) {
        remember_broker_resource_terminal(
            &mut tombstones,
            stream_id,
            InvocationId::from_bytes([stream_id as u8; 16]),
        );
    }
    assert_eq!(tombstones.len(), BROKER_RESOURCE_TOMBSTONE_CAPACITY);
    assert!(!tombstones.contains_key(&1));
    assert!(tombstones.contains_key(&(BROKER_RESOURCE_TOMBSTONE_CAPACITY as u64 + 1)));
}

#[tokio::test]
async fn shared_broker_drops_evicted_terminal_frames_by_high_water_mark() {
    let (active, registry) = transport_test_context();
    let (_reader, mut writer) = tokio::io::duplex(128);
    let mut resources = BTreeMap::new();
    let mut tombstones = BrokerResourceTombstones::new();
    let mut root = None;
    let mut terminal_completions = Vec::new();
    let make_state =
        |request: ResourceRequest,
         completion: Sender<Result<ResourceTransportOutcome, ResourceTransportFailure>>| {
            let mut protocol = ResourceProtocolConnection::new();
            protocol
                .open(request.clone())
                .expect("resource request opens");
            BrokerResourceState {
                request,
                expected_type: ResolvedType::Scalar(StandardScalar::Integer),
                resource_kind: ProtocolResourceKind::Single,
                protocol,
                completion,
                accepted: false,
                accepted_nested_invocation_id: None,
                scalar_value: None,
                cancellation_requested: false,
                stream_values_seen: false,
                terminal_provenance: ResourceTerminalProvenance::Uncommitted,
                scalar_value_after_cancellation: false,
            }
        };

    for stream_id in 1..=(BROKER_RESOURCE_TOMBSTONE_CAPACITY as u64 + 1) {
        let request = transport_test_request(active.pair(), stream_id);
        let (completion, receiver) = mpsc::channel(BROKER_RESOURCE_COMPLETION_CAPACITY);
        terminal_completions.push(receiver);
        resources.insert(stream_id, make_state(request.clone(), completion));
        let bytes = encode_resource_server_frame(
            &active,
            &registry,
            &ResourceServerFrame::Failed(ResourceFailed {
                stream_id,
                request_id: request.request_id,
                target_revision: active.pair(),
                failure: CallFailure::ExecuteDenied,
            }),
        )
        .expect("encoded terminal resource response");
        handle_shared_broker_frame(
            BrokerWireFrame {
                resource: true,
                bytes,
            },
            &mut writer,
            &active,
            &registry,
            &mut root,
            &mut resources,
            Some(stream_id),
            &mut tombstones,
            &Arc::new(Mutex::new(BTreeMap::new())),
        )
        .await
        .expect("terminal resource response");
    }
    assert_eq!(tombstones.len(), BROKER_RESOURCE_TOMBSTONE_CAPACITY);
    assert!(!tombstones.contains_key(&1));

    let current_stream_id = BROKER_RESOURCE_TOMBSTONE_CAPACITY as u64 + 2;
    let current_request = transport_test_request(active.pair(), current_stream_id);
    let (current_completion, mut current_completions) = mpsc::channel(2);
    resources.insert(
        current_stream_id,
        make_state(current_request.clone(), current_completion),
    );
    let high_water_mark = Some(current_stream_id);

    let late_bytes = encode_resource_server_frame(
        &active,
        &registry,
        &ResourceServerFrame::Failed(ResourceFailed {
            stream_id: 1,
            request_id: InvocationId::from_bytes([1; 16]),
            target_revision: active.pair(),
            failure: CallFailure::ExecuteDenied,
        }),
    )
    .expect("encoded evicted late resource response");
    handle_shared_broker_frame(
        BrokerWireFrame {
            resource: true,
            bytes: late_bytes,
        },
        &mut writer,
        &active,
        &registry,
        &mut root,
        &mut resources,
        high_water_mark,
        &mut tombstones,
        &Arc::new(Mutex::new(BTreeMap::new())),
    )
    .await
    .expect("evicted late resource response is dropped");
    assert!(resources.contains_key(&current_stream_id));

    let accepted = ResourceAccepted {
        stream_id: current_stream_id,
        request_id: current_request.request_id,
        nested_invocation_id: InvocationId::from_bytes([0x40; 16]),
        target_revision: current_request.target_revision,
        resource_kind: current_request.resource_kind,
    };
    let value = RuntimeValue::Integer(7);
    let byte_count = encode_constructed_value(&active, &registry, &value)
        .expect("encoded current resource value")
        .len() as u32;
    let values = ResourceValues {
        stream_id: current_stream_id,
        request_id: current_request.request_id,
        target_revision: active.pair(),
        batch_sequence: 0,
        item_count: 1,
        byte_count,
        values: vec![value],
    };
    let completed = ResourceCompleted {
        stream_id: current_stream_id,
        request_id: current_request.request_id,
        target_revision: active.pair(),
        final_batch_sequence: 0,
        total_items: 1,
    };
    for frame in [
        ResourceServerFrame::Accepted(accepted),
        ResourceServerFrame::Values(values),
        ResourceServerFrame::Completed(completed),
    ] {
        let bytes = encode_resource_server_frame(&active, &registry, &frame)
            .expect("encoded current resource response");
        handle_shared_broker_frame(
            BrokerWireFrame {
                resource: true,
                bytes,
            },
            &mut writer,
            &active,
            &registry,
            &mut root,
            &mut resources,
            high_water_mark,
            &mut tombstones,
            &Arc::new(Mutex::new(BTreeMap::new())),
        )
        .await
        .expect("current resource remains usable");
    }
    assert!(!resources.contains_key(&current_stream_id));
    assert!(matches!(
        current_completions.recv().await,
        Some(Ok(ResourceTransportOutcome::Ready {
            value: RuntimeValue::Integer(7),
            nested_invocation_id,
        })) if nested_invocation_id == InvocationId::from_bytes([0x40; 16])
    ));

    let future_stream_id = current_stream_id + 1;
    let future_request = transport_test_request(active.pair(), future_stream_id);
    let future_bytes = encode_resource_server_frame(
        &active,
        &registry,
        &ResourceServerFrame::Failed(ResourceFailed {
            stream_id: future_stream_id,
            request_id: future_request.request_id,
            target_revision: active.pair(),
            failure: CallFailure::ExecuteDenied,
        }),
    )
    .expect("encoded future resource response");
    assert!(matches!(
        handle_shared_broker_frame(
            BrokerWireFrame {
                resource: true,
                bytes: future_bytes,
            },
            &mut writer,
            &active,
            &registry,
            &mut root,
            &mut resources,
            high_water_mark,
            &mut tombstones,
            &Arc::new(Mutex::new(BTreeMap::new())),
        )
        .await,
        Err(ResourceTransportFailure::Shape)
    ));
}

#[test]
fn shared_broker_reconstructs_completed_root_events() {
    let events = echo_events();
    let invocation = events.records()[0].event().invocation_id();
    let result = reconstruct_shared_root_result(invocation, events.records().to_vec())
        .expect("completed root result");
    assert!(matches!(result, SealedInvocationResult::Completed { .. }));
}

async fn assert_accepted_event_batch_is_shape(frame: ServerFrame, invocation: InvocationId) {
    let (active, registry) = transport_test_context();
    let bytes = encode_constructed_server_frame(&active, &registry, &frame)
        .expect("encoded invalid accepted EventBatch");
    let (response, _receiver) = tokio::sync::oneshot::channel();
    let mut root = Some(BrokerRootState {
        invocation: Some(invocation),
        records: Vec::new(),
        response,
    });
    let mut resources = BTreeMap::new();
    let mut tombstones = BrokerResourceTombstones::new();
    let (_reader, mut writer) = tokio::io::duplex(128);

    assert!(matches!(
        handle_shared_broker_frame(
            BrokerWireFrame {
                resource: false,
                bytes,
            },
            &mut writer,
            &active,
            &registry,
            &mut root,
            &mut resources,
            None,
            &mut tombstones,
            &Arc::new(Mutex::new(BTreeMap::new())),
        )
        .await,
        Err(ResourceTransportFailure::Shape)
    ));
    assert!(root.is_some());
}

#[tokio::test]
async fn shared_broker_rejects_first_non_started_event() {
    let invocation = InvocationId::new();
    let value = InvokeEvent::new(
        invocation,
        0,
        InvocationEventBody::value_batch(
            None,
            [InvokeValue::new(RuntimeValue::Integer(7)).expect("integer value")],
        )
        .expect("value batch body"),
    )
    .expect("value event");
    assert_accepted_event_batch_is_shape(
        ServerFrame::EventBatch {
            stream: 1,
            channel: Channel::ResultValues,
            events: vec![EventRecord {
                sequence: 1,
                event: Event::Value(RuntimeValue::InvokeEvent(value)),
            }],
        },
        invocation,
    )
    .await;
}

#[tokio::test]
async fn shared_broker_rejects_duplicate_started_event_batch() {
    let invocation = InvocationId::new();
    let started = |sequence| {
        InvokeEvent::new(
            invocation,
            sequence,
            InvocationEventBody::Started {
                visible_principal: None,
            },
        )
        .expect("started event")
    };
    assert_accepted_event_batch_is_shape(
        ServerFrame::EventBatch {
            stream: 1,
            channel: Channel::ResultValues,
            events: vec![
                EventRecord {
                    sequence: 1,
                    event: Event::Value(RuntimeValue::InvokeEvent(started(0))),
                },
                EventRecord {
                    sequence: 2,
                    event: Event::Value(RuntimeValue::InvokeEvent(started(1))),
                },
            ],
        },
        invocation,
    )
    .await;
}

#[tokio::test]
async fn shared_broker_rejects_terminal_followup_in_same_event_batch() {
    let (active, registry) = transport_test_context();
    let invocation = InvocationId::new();
    let started = InvokeEvent::new(
        invocation,
        0,
        InvocationEventBody::Started {
            visible_principal: None,
        },
    )
    .expect("started event");
    let completed = InvokeEvent::new(
        invocation,
        1,
        InvocationEventBody::Completed {
            duration_nanoseconds: 0,
        },
    )
    .expect("completed event");
    let value = InvokeEvent::new(
        invocation,
        2,
        InvocationEventBody::value_batch(
            None,
            [InvokeValue::new(RuntimeValue::Integer(7)).expect("integer value")],
        )
        .expect("value batch body"),
    )
    .expect("value event");
    let frame = ServerFrame::EventBatch {
        stream: 1,
        channel: Channel::ResultValues,
        events: vec![
            EventRecord {
                sequence: 1,
                event: Event::Value(RuntimeValue::InvokeEvent(started)),
            },
            EventRecord {
                sequence: 2,
                event: Event::Value(RuntimeValue::InvokeEvent(completed)),
            },
            EventRecord {
                sequence: 3,
                event: Event::Value(RuntimeValue::InvokeEvent(value)),
            },
        ],
    };
    let bytes = encode_constructed_server_frame(&active, &registry, &frame)
        .expect("encoded terminal followup batch");
    let (response, _receiver) = tokio::sync::oneshot::channel();
    let mut root = Some(BrokerRootState {
        invocation: Some(invocation),
        records: Vec::new(),
        response,
    });
    let mut resources = BTreeMap::new();
    let mut tombstones = BrokerResourceTombstones::new();
    let (_reader, mut writer) = tokio::io::duplex(128);

    assert!(matches!(
        handle_shared_broker_frame(
            BrokerWireFrame {
                resource: false,
                bytes,
            },
            &mut writer,
            &active,
            &registry,
            &mut root,
            &mut resources,
            None,
            &mut tombstones,
            &Arc::new(Mutex::new(BTreeMap::new())),
        )
        .await,
        Err(ResourceTransportFailure::Shape)
    ));
    assert!(root.is_some());
}

#[tokio::test]
async fn shared_broker_rejects_root_events_before_acceptance() {
    let (active, registry) = transport_test_context();
    let events = echo_events();
    let frame = ServerFrame::EventBatch {
        stream: 1,
        channel: Channel::ResultValues,
        events: events
            .records()
            .iter()
            .map(|record| EventRecord {
                sequence: record.outer_sequence(),
                event: Event::Value(RuntimeValue::InvokeEvent(record.event().clone())),
            })
            .collect(),
    };
    let bytes = encode_constructed_server_frame(&active, &registry, &frame)
        .expect("encoded root event batch");
    let (response, _receiver) = tokio::sync::oneshot::channel();
    let mut root = Some(BrokerRootState {
        invocation: None,
        records: Vec::new(),
        response,
    });
    let mut resources = BTreeMap::new();
    let mut tombstones = BrokerResourceTombstones::new();
    let (_reader, mut writer) = tokio::io::duplex(128);

    assert!(matches!(
        handle_shared_broker_frame(
            BrokerWireFrame {
                resource: false,
                bytes,
            },
            &mut writer,
            &active,
            &registry,
            &mut root,
            &mut resources,
            None,
            &mut tombstones,
            &Arc::new(Mutex::new(BTreeMap::new())),
        )
        .await,
        Err(ResourceTransportFailure::Shape)
    ));
}

#[tokio::test]
async fn shared_broker_maps_preflight_denial_without_invocation() {
    let (active, registry) = transport_test_context();
    let frame = ServerFrame::CallFailed {
        stream: 1,
        failure: CallFailure::ExecuteDenied,
    };
    let bytes = encode_constructed_server_frame(&active, &registry, &frame)
        .expect("encoded preflight denial");
    let (response, receiver) = tokio::sync::oneshot::channel();
    let mut root = Some(BrokerRootState {
        invocation: None,
        records: Vec::new(),
        response,
    });
    let mut resources = BTreeMap::new();
    let mut tombstones = BrokerResourceTombstones::new();
    let (_reader, mut writer) = tokio::io::duplex(128);

    handle_shared_broker_frame(
        BrokerWireFrame {
            resource: false,
            bytes,
        },
        &mut writer,
        &active,
        &registry,
        &mut root,
        &mut resources,
        None,
        &mut tombstones,
        &Arc::new(Mutex::new(BTreeMap::new())),
    )
    .await
    .expect("preflight denial is a valid terminal root frame");

    assert!(root.is_none());
    assert!(matches!(
        receiver.await,
        Ok(Err(ResourceTransportFailure::RootPreflightDenied))
    ));
}

#[tokio::test]
async fn shared_broker_maps_preflight_internal_failure_without_invocation() {
    let (active, registry) = transport_test_context();
    let frame = ServerFrame::CallFailed {
        stream: 1,
        failure: CallFailure::InternalFailure,
    };
    let bytes = encode_constructed_server_frame(&active, &registry, &frame)
        .expect("encoded preflight internal failure");
    let (response, receiver) = tokio::sync::oneshot::channel();
    let mut root = Some(BrokerRootState {
        invocation: None,
        records: Vec::new(),
        response,
    });
    let mut resources = BTreeMap::new();
    let mut tombstones = BrokerResourceTombstones::new();
    let (_reader, mut writer) = tokio::io::duplex(128);

    handle_shared_broker_frame(
        BrokerWireFrame {
            resource: false,
            bytes,
        },
        &mut writer,
        &active,
        &registry,
        &mut root,
        &mut resources,
        None,
        &mut tombstones,
        &Arc::new(Mutex::new(BTreeMap::new())),
    )
    .await
    .expect("preflight internal failure is a valid terminal root frame");

    assert!(root.is_none());
    assert!(matches!(
        receiver.await,
        Ok(Err(ResourceTransportFailure::RootSealedDispatchInternal))
    ));
}

#[tokio::test]
async fn shared_broker_maps_preaccept_cancellation_without_invocation() {
    let (active, registry) = transport_test_context();
    let frame = ServerFrame::CallCancelled { stream: 1 };
    let bytes = encode_constructed_server_frame(&active, &registry, &frame)
        .expect("encoded pre-accept cancellation");
    let (response, receiver) = tokio::sync::oneshot::channel();
    let mut root = Some(BrokerRootState {
        invocation: None,
        records: Vec::new(),
        response,
    });
    let mut resources = BTreeMap::new();
    let mut tombstones = BrokerResourceTombstones::new();
    let (_reader, mut writer) = tokio::io::duplex(128);

    handle_shared_broker_frame(
        BrokerWireFrame {
            resource: false,
            bytes,
        },
        &mut writer,
        &active,
        &registry,
        &mut root,
        &mut resources,
        None,
        &mut tombstones,
        &Arc::new(Mutex::new(BTreeMap::new())),
    )
    .await
    .expect("pre-accept cancellation is a valid terminal root frame");

    assert!(root.is_none());
    assert!(matches!(
        receiver.await,
        Ok(Err(ResourceTransportFailure::Cancelled))
    ));
}

async fn assert_accepted_root_frame_is_shape(frame: ServerFrame) {
    let (active, registry) = transport_test_context();
    let bytes = encode_constructed_server_frame(&active, &registry, &frame)
        .expect("encoded accepted root frame");
    let invocation = InvocationId::from_bytes([0x52; 16]);
    let (response, receiver) = tokio::sync::oneshot::channel();
    let mut root = Some(BrokerRootState {
        invocation: Some(invocation),
        records: Vec::new(),
        response,
    });
    let mut resources = BTreeMap::new();
    let mut tombstones = BrokerResourceTombstones::new();
    let (_reader, mut writer) = tokio::io::duplex(128);

    assert!(matches!(
        handle_shared_broker_frame(
            BrokerWireFrame {
                resource: false,
                bytes,
            },
            &mut writer,
            &active,
            &registry,
            &mut root,
            &mut resources,
            None,
            &mut tombstones,
            &Arc::new(Mutex::new(BTreeMap::new())),
        )
        .await,
        Err(ResourceTransportFailure::Shape)
    ));
    assert!(root.is_none());
    assert!(matches!(
        receiver.await,
        Ok(Err(ResourceTransportFailure::Shape))
    ));
}
async fn assert_invalid_preaccept_root_frame_is_shape(
    frame: ServerFrame,
    records: Vec<InvocationEventRecord>,
) {
    let (active, registry) = transport_test_context();
    let bytes = encode_constructed_server_frame(&active, &registry, &frame)
        .expect("encoded invalid pre-accept root frame");
    let (response, receiver) = tokio::sync::oneshot::channel();
    let mut root = Some(BrokerRootState {
        invocation: None,
        records,
        response,
    });
    let mut resources = BTreeMap::new();
    let mut tombstones = BrokerResourceTombstones::new();
    let (_reader, mut writer) = tokio::io::duplex(128);

    assert!(matches!(
        handle_shared_broker_frame(
            BrokerWireFrame {
                resource: false,
                bytes,
            },
            &mut writer,
            &active,
            &registry,
            &mut root,
            &mut resources,
            None,
            &mut tombstones,
            &Arc::new(Mutex::new(BTreeMap::new())),
        )
        .await,
        Err(ResourceTransportFailure::Shape)
    ));
    assert!(root.is_none());
    assert!(matches!(
        receiver.await,
        Ok(Err(ResourceTransportFailure::Shape))
    ));
}

#[tokio::test]
async fn shared_broker_rejects_accepted_call_failed_as_shape() {
    assert_accepted_root_frame_is_shape(ServerFrame::CallFailed {
        stream: 1,
        failure: CallFailure::ExecuteDenied,
    })
    .await;
}

#[tokio::test]
async fn shared_broker_rejects_accepted_call_cancelled_as_shape() {
    assert_accepted_root_frame_is_shape(ServerFrame::CallCancelled { stream: 1 }).await;
}

#[tokio::test]
async fn shared_broker_rejects_accepted_completion_without_events_as_shape() {
    assert_accepted_root_frame_is_shape(ServerFrame::CallCompleted { stream: 1 }).await;
}
#[tokio::test]
async fn shared_broker_notifies_invalid_preaccept_call_failed() {
    assert_invalid_preaccept_root_frame_is_shape(
        ServerFrame::CallFailed {
            stream: 1,
            failure: CallFailure::TargetUnavailable,
        },
        Vec::new(),
    )
    .await;
}

#[tokio::test]
async fn shared_broker_notifies_invalid_preaccept_call_cancelled() {
    assert_invalid_preaccept_root_frame_is_shape(
        ServerFrame::CallCancelled { stream: 1 },
        echo_events().records().to_vec(),
    )
    .await;
}

#[test]
fn shared_broker_rejects_root_result_without_started_event() {
    let invocation = InvocationId::new();
    let value = InvokeEvent::new(
        invocation,
        0,
        InvocationEventBody::value_batch(
            None,
            [InvokeValue::new(RuntimeValue::Integer(1)).expect("integer value")],
        )
        .expect("value batch body"),
    )
    .expect("value event");
    let completed = InvokeEvent::new(
        invocation,
        1,
        InvocationEventBody::Completed {
            duration_nanoseconds: 0,
        },
    )
    .expect("completed event");

    assert!(matches!(
        reconstruct_shared_root_result(
            invocation,
            vec![
                InvocationEventRecord::new(1, value),
                InvocationEventRecord::new(2, completed),
            ],
        ),
        Err(ResourceTransportFailure::Shape)
    ));
}

#[test]
fn shared_broker_rejects_repeated_started_event() {
    let invocation = InvocationId::new();
    let started = |sequence| {
        InvokeEvent::new(
            invocation,
            sequence,
            InvocationEventBody::Started {
                visible_principal: None,
            },
        )
        .expect("started event")
    };
    let completed = InvokeEvent::new(
        invocation,
        2,
        InvocationEventBody::Completed {
            duration_nanoseconds: 0,
        },
    )
    .expect("completed event");

    assert!(matches!(
        reconstruct_shared_root_result(
            invocation,
            vec![
                InvocationEventRecord::new(1, started(0)),
                InvocationEventRecord::new(2, started(1)),
                InvocationEventRecord::new(3, completed),
            ],
        ),
        Err(ResourceTransportFailure::Shape)
    ));
}

#[test]
fn shared_broker_rejects_root_events_after_terminal_event() {
    let events = echo_events();
    let invocation = events.records()[0].event().invocation_id();
    let late_value = InvokeEvent::new(
        invocation,
        3,
        InvocationEventBody::value_batch(
            None,
            [InvokeValue::new(RuntimeValue::Integer(99)).expect("integer value")],
        )
        .expect("value batch body"),
    )
    .expect("late value event");
    let mut records = events.records().to_vec();
    records.push(InvocationEventRecord::new(4, late_value));

    assert!(matches!(
        reconstruct_shared_root_result(invocation, records),
        Err(ResourceTransportFailure::Shape)
    ));
}

#[test]
fn shared_broker_reconstructs_failed_root_events() {
    let invocation = InvocationId::new();
    let started = InvokeEvent::new(
        invocation,
        0,
        InvocationEventBody::Started {
            visible_principal: None,
        },
    )
    .expect("started event");
    let failure = orna_core::invocation::InvocationFailure::new(
        orna_core::invocation::InvocationFailurePhase::Target,
        "TARGET_FAILED",
        "invocation failed",
        None,
        orna_core::invocation::InvocationRetryability::No,
    )
    .expect("failure event");
    let failed = InvokeEvent::new(invocation, 1, InvocationEventBody::Failed(failure))
        .expect("failed event");
    let result = reconstruct_shared_root_result(
        invocation,
        vec![
            InvocationEventRecord::new(1, started),
            InvocationEventRecord::new(2, failed),
        ],
    )
    .expect("failed root result");
    assert!(matches!(result, SealedInvocationResult::Failed { .. }));
}

#[test]
fn shared_broker_maps_redacted_root_failure_classes() {
    let redacted_result = |code: &str| {
        let invocation = InvocationId::new();
        let started = InvokeEvent::new(
            invocation,
            0,
            InvocationEventBody::Started {
                visible_principal: None,
            },
        )
        .expect("started event");
        let failure = orna_core::invocation::InvocationFailure::new(
            orna_core::invocation::InvocationFailurePhase::Internal,
            code,
            "redacted failure",
            None,
            orna_core::invocation::InvocationRetryability::No,
        )
        .expect("failure event");
        let failed = InvokeEvent::new(invocation, 1, InvocationEventBody::Failed(failure))
            .expect("failed event");
        reconstruct_shared_root_result(
            invocation,
            vec![
                InvocationEventRecord::new(1, started),
                InvocationEventRecord::new(2, failed),
            ],
        )
    };
    assert!(matches!(
        redacted_result("INVOKE_DENIED"),
        Ok(SealedInvocationResult::Denied { .. })
    ));
    assert!(matches!(
        redacted_result("INVOKE_INTERNAL_FAILURE"),
        Err(ResourceTransportFailure::RootSealedDispatchInternal)
    ));
}

#[test]
fn shared_broker_maps_cancelled_root_terminal_to_cancelled_transport() {
    let invocation = InvocationId::new();
    let started = InvokeEvent::new(
        invocation,
        0,
        InvocationEventBody::Started {
            visible_principal: None,
        },
    )
    .expect("started event");
    let cancelled = InvokeEvent::new(
        invocation,
        1,
        InvocationEventBody::Cancelled { reason: None },
    )
    .expect("cancelled event");
    let result = reconstruct_shared_root_result(
        invocation,
        vec![
            InvocationEventRecord::new(1, started),
            InvocationEventRecord::new(2, cancelled),
        ],
    );
    assert!(matches!(result, Err(ResourceTransportFailure::Cancelled)));
}

#[tokio::test]
async fn broker_rejects_zero_nested_invocation_identity() {
    let (active, registry) = transport_test_context();
    let request = transport_test_request(active.pair(), 1);
    let mut protocol = ResourceProtocolConnection::new();
    protocol
        .open(request.clone())
        .expect("resource request opens");
    let (completion, _completions) = mpsc::channel(1);
    let mut state = BrokerResourceState {
        request: request.clone(),
        expected_type: ResolvedType::Scalar(StandardScalar::Integer),
        resource_kind: ProtocolResourceKind::Single,
        protocol,
        completion,
        accepted: false,
        accepted_nested_invocation_id: None,
        scalar_value: None,
        cancellation_requested: false,
        stream_values_seen: false,
        terminal_provenance: ResourceTerminalProvenance::Uncommitted,
        scalar_value_after_cancellation: false,
    };
    let (_reader, mut writer) = tokio::io::duplex(128);
    let result = handle_shared_resource_frame(
        &mut state,
        ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id: request.stream_id,
            request_id: request.request_id,
            nested_invocation_id: InvocationId::from_bytes([0; 16]),
            target_revision: request.target_revision,
            resource_kind: request.resource_kind,
        }),
        &mut writer,
        &active,
        &registry,
    )
    .await;
    assert!(matches!(result, Err(ResourceTransportFailure::Shape)));
    assert!(!state.accepted);
    assert_eq!(state.accepted_nested_invocation_id, None);
}

#[tokio::test]
async fn broker_retains_accepted_nested_invocation_identity() {
    let (active, registry) = transport_test_context();
    let request = transport_test_request(active.pair(), 1);
    let nested_invocation_id = InvocationId::from_bytes([0x40; 16]);
    let mut protocol = ResourceProtocolConnection::new();
    protocol
        .open(request.clone())
        .expect("resource request opens");
    let (completion, _completions) = mpsc::channel(1);
    let mut state = BrokerResourceState {
        request: request.clone(),
        expected_type: ResolvedType::Scalar(StandardScalar::Integer),
        resource_kind: ProtocolResourceKind::Single,
        protocol,
        completion,
        accepted: false,
        accepted_nested_invocation_id: None,
        scalar_value: None,
        cancellation_requested: false,
        stream_values_seen: false,
        terminal_provenance: ResourceTerminalProvenance::Uncommitted,
        scalar_value_after_cancellation: false,
    };
    let (_reader, mut writer) = tokio::io::duplex(128);
    assert!(
        handle_shared_resource_frame(
            &mut state,
            ResourceServerFrame::Accepted(ResourceAccepted {
                stream_id: request.stream_id,
                request_id: request.request_id,
                nested_invocation_id,
                target_revision: request.target_revision,
                resource_kind: request.resource_kind,
            }),
            &mut writer,
            &active,
            &registry,
        )
        .await
        .expect("resource acceptance applies")
    );
    assert!(state.accepted);
    assert_eq!(
        state.accepted_nested_invocation_id,
        Some(nested_invocation_id)
    );
}

#[tokio::test]
async fn broker_retains_accepted_nested_invocation_identity_for_stream_completion() {
    let (active, registry) = transport_test_context();
    let mut request = transport_test_request(active.pair(), 1);
    request.resource_kind = ProtocolResourceKind::Stream;
    let nested_invocation_id = InvocationId::from_bytes([0x41; 16]);
    let mut protocol = ResourceProtocolConnection::new();
    protocol
        .open(request.clone())
        .expect("resource request opens");
    let (completion, mut completions) = mpsc::channel(1);
    let mut state = BrokerResourceState {
        request: request.clone(),
        expected_type: ResolvedType::Scalar(StandardScalar::Integer),
        resource_kind: ProtocolResourceKind::Stream,
        protocol,
        completion,
        accepted: false,
        accepted_nested_invocation_id: None,
        scalar_value: None,
        cancellation_requested: false,
        stream_values_seen: false,
        terminal_provenance: ResourceTerminalProvenance::Uncommitted,
        scalar_value_after_cancellation: false,
    };
    let (_reader, mut writer) = tokio::io::duplex(128);
    handle_shared_resource_frame(
        &mut state,
        ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id: request.stream_id,
            request_id: request.request_id,
            nested_invocation_id,
            target_revision: request.target_revision,
            resource_kind: ProtocolResourceKind::Stream,
        }),
        &mut writer,
        &active,
        &registry,
    )
    .await
    .expect("resource acceptance applies");
    let keep = handle_shared_resource_frame(
        &mut state,
        ResourceServerFrame::Completed(ResourceCompleted {
            stream_id: request.stream_id,
            request_id: request.request_id,
            target_revision: active.pair(),
            final_batch_sequence: 0,
            total_items: 0,
        }),
        &mut writer,
        &active,
        &registry,
    )
    .await
    .expect("stream completion applies");
    assert!(!keep);
    assert!(matches!(
        completions.recv().await,
        Some(Ok(ResourceTransportOutcome::StreamCompleted {
            nested_invocation_id: actual,
        })) if actual == nested_invocation_id
    ));
}

#[tokio::test]
async fn broker_retains_accepted_nested_invocation_identity_for_cancelled_resource() {
    let (active, registry) = transport_test_context();
    let request = transport_test_request(active.pair(), 1);
    let nested_invocation_id = InvocationId::from_bytes([0x42; 16]);
    let mut protocol = ResourceProtocolConnection::new();
    protocol
        .open(request.clone())
        .expect("resource request opens");
    let (completion, mut completions) = mpsc::channel(1);
    let mut state = BrokerResourceState {
        request: request.clone(),
        expected_type: ResolvedType::Scalar(StandardScalar::Integer),
        resource_kind: ProtocolResourceKind::Single,
        protocol,
        completion,
        accepted: false,
        accepted_nested_invocation_id: None,
        scalar_value: None,
        cancellation_requested: false,
        stream_values_seen: false,
        terminal_provenance: ResourceTerminalProvenance::Uncommitted,
        scalar_value_after_cancellation: false,
    };
    let (_reader, mut writer) = tokio::io::duplex(128);
    handle_shared_resource_frame(
        &mut state,
        ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id: request.stream_id,
            request_id: request.request_id,
            nested_invocation_id,
            target_revision: request.target_revision,
            resource_kind: request.resource_kind,
        }),
        &mut writer,
        &active,
        &registry,
    )
    .await
    .expect("resource acceptance applies");
    let keep = handle_shared_resource_frame(
        &mut state,
        ResourceServerFrame::Cancelled(ResourceCancelled {
            stream_id: request.stream_id,
            request_id: request.request_id,
            target_revision: active.pair(),
            reason: ResourceCancellationCode::ParentInvocationCancelled,
        }),
        &mut writer,
        &active,
        &registry,
    )
    .await
    .expect("resource cancellation applies");
    assert!(!keep);
    assert!(matches!(
        completions.recv().await,
        Some(Ok(ResourceTransportOutcome::Cancelled {
            nested_invocation_id: Some(actual),
        })) if actual == nested_invocation_id
    ));
}

#[tokio::test]
async fn broker_drains_late_acceptance_and_values_after_cancel_before_completion() {
    let (active, registry) = transport_test_context();
    let request = transport_test_request(active.pair(), 1);
    let sibling_request = transport_test_request(active.pair(), 2);
    let malformed_acceptance_request = transport_test_request(active.pair(), 3);
    let malformed_values_request = transport_test_request(active.pair(), 4);
    let nested_invocation_id = InvocationId::from_bytes([0x40; 16]);
    let sibling_nested_invocation_id = InvocationId::from_bytes([0x45; 16]);

    let make_state =
        |request: ResourceRequest,
         completion: Sender<Result<ResourceTransportOutcome, ResourceTransportFailure>>,
         cancellation_requested: bool,
         accepted_nested_invocation_id: Option<InvocationId>| {
            let mut protocol = ResourceProtocolConnection::new();
            protocol
                .open(request.clone())
                .expect("resource request opens");
            if let Some(nested_invocation_id) = accepted_nested_invocation_id.clone() {
                protocol
                    .apply_constructed(
                        &active,
                        &registry,
                        ResourceServerFrame::Accepted(ResourceAccepted {
                            stream_id: request.stream_id,
                            request_id: request.request_id,
                            nested_invocation_id,
                            target_revision: request.target_revision,
                            resource_kind: request.resource_kind,
                        }),
                    )
                    .expect("resource acceptance applies");
            }
            if cancellation_requested {
                protocol
                    .receive(ResourceClientFrame::Cancel(ResourceCancel {
                        stream_id: request.stream_id,
                        request_id: request.request_id,
                        reason: ResourceCancellationCode::ParentInvocationCancelled,
                    }))
                    .expect("resource cancellation applies");
            }
            BrokerResourceState {
                request,
                expected_type: ResolvedType::Scalar(StandardScalar::Integer),
                resource_kind: ProtocolResourceKind::Single,
                protocol,
                completion,
                accepted: accepted_nested_invocation_id.is_some(),
                accepted_nested_invocation_id,
                scalar_value: None,
                cancellation_requested,
                stream_values_seen: false,
                terminal_provenance: ResourceTerminalProvenance::Uncommitted,
                scalar_value_after_cancellation: false,
            }
        };

    let accepted = ResourceAccepted {
        stream_id: request.stream_id,
        request_id: request.request_id,
        nested_invocation_id,
        target_revision: request.target_revision,
        resource_kind: request.resource_kind,
    };
    let value = RuntimeValue::Integer(7);
    let byte_count = encode_constructed_value(&active, &registry, &value)
        .expect("encoded resource value")
        .len() as u32;
    let values = ResourceValues {
        stream_id: request.stream_id,
        request_id: request.request_id,
        target_revision: active.pair(),
        batch_sequence: 0,
        item_count: 1,
        byte_count,
        values: vec![value],
    };
    let completed = ResourceCompleted {
        stream_id: request.stream_id,
        request_id: request.request_id,
        target_revision: active.pair(),
        final_batch_sequence: 0,
        total_items: 1,
    };

    let sibling_accepted = ResourceAccepted {
        stream_id: sibling_request.stream_id,
        request_id: sibling_request.request_id,
        nested_invocation_id: sibling_nested_invocation_id,
        target_revision: sibling_request.target_revision,
        resource_kind: sibling_request.resource_kind,
    };
    let sibling_value = RuntimeValue::Integer(8);
    let sibling_byte_count = encode_constructed_value(&active, &registry, &sibling_value)
        .expect("encoded sibling resource value")
        .len() as u32;
    let sibling_values = ResourceValues {
        stream_id: sibling_request.stream_id,
        request_id: sibling_request.request_id,
        target_revision: active.pair(),
        batch_sequence: 0,
        item_count: 1,
        byte_count: sibling_byte_count,
        values: vec![sibling_value],
    };
    let sibling_completed = ResourceCompleted {
        stream_id: sibling_request.stream_id,
        request_id: sibling_request.request_id,
        target_revision: active.pair(),
        final_batch_sequence: 0,
        total_items: 1,
    };

    let (completion, mut completions) = mpsc::channel(2);
    let (sibling_completion, mut sibling_completions) = mpsc::channel(2);
    let (malformed_acceptance_completion, mut malformed_acceptance_completions) = mpsc::channel(1);
    let (malformed_values_completion, mut malformed_values_completions) = mpsc::channel(1);
    let mut resources = BTreeMap::from([
        (
            request.stream_id,
            make_state(request.clone(), completion, true, None),
        ),
        (
            sibling_request.stream_id,
            make_state(sibling_request.clone(), sibling_completion, false, None),
        ),
        (
            malformed_acceptance_request.stream_id,
            make_state(
                malformed_acceptance_request.clone(),
                malformed_acceptance_completion,
                true,
                None,
            ),
        ),
        (
            malformed_values_request.stream_id,
            make_state(
                malformed_values_request.clone(),
                malformed_values_completion,
                true,
                Some(InvocationId::from_bytes([0x44; 16])),
            ),
        ),
    ]);
    let mut tombstones = BrokerResourceTombstones::new();
    let mut root = None;
    let resource_terminal_provenance = Arc::new(Mutex::new(BTreeMap::new()));
    let resource_high_water_mark = Some(malformed_values_request.stream_id);
    let (_reader, mut writer) = tokio::io::duplex(512);

    let bytes =
        encode_resource_server_frame(&active, &registry, &ResourceServerFrame::Accepted(accepted))
            .expect("encoded late acceptance");
    handle_shared_broker_frame(
        BrokerWireFrame {
            resource: true,
            bytes,
        },
        &mut writer,
        &active,
        &registry,
        &mut root,
        &mut resources,
        resource_high_water_mark,
        &mut tombstones,
        &resource_terminal_provenance,
    )
    .await
    .expect("late acceptance is drained by the outer broker");
    let primary = resources
        .get(&request.stream_id)
        .expect("late acceptance retains primary state");
    assert!(primary.accepted);
    assert_eq!(
        primary.accepted_nested_invocation_id,
        Some(nested_invocation_id)
    );
    assert!(resources.contains_key(&sibling_request.stream_id));

    let bytes =
        encode_resource_server_frame(&active, &registry, &ResourceServerFrame::Values(values))
            .expect("encoded late values");
    handle_shared_broker_frame(
        BrokerWireFrame {
            resource: true,
            bytes,
        },
        &mut writer,
        &active,
        &registry,
        &mut root,
        &mut resources,
        resource_high_water_mark,
        &mut tombstones,
        &resource_terminal_provenance,
    )
    .await
    .expect("late values are drained by the outer broker");
    let primary = resources
        .get(&request.stream_id)
        .expect("late values retain primary state");
    assert!(matches!(
        &primary.scalar_value,
        Some(RuntimeValue::Integer(7))
    ));
    assert!(primary.scalar_value_after_cancellation);
    assert!(resources.contains_key(&sibling_request.stream_id));

    let malformed_acceptance = ResourceServerFrame::Accepted(ResourceAccepted {
        stream_id: malformed_acceptance_request.stream_id,
        request_id: malformed_acceptance_request.request_id,
        nested_invocation_id: InvocationId::from_bytes([0x41; 16]),
        target_revision: malformed_acceptance_request.target_revision,
        resource_kind: ProtocolResourceKind::Stream,
    });
    let bytes = encode_resource_server_frame(&active, &registry, &malformed_acceptance)
        .expect("malformed late acceptance encodes");
    handle_shared_broker_frame(
        BrokerWireFrame {
            resource: true,
            bytes,
        },
        &mut writer,
        &active,
        &registry,
        &mut root,
        &mut resources,
        resource_high_water_mark,
        &mut tombstones,
        &resource_terminal_provenance,
    )
    .await
    .expect("outer broker contains malformed late acceptance");
    assert!(!resources.contains_key(&malformed_acceptance_request.stream_id));
    assert_eq!(
        tombstones.get(&malformed_acceptance_request.stream_id),
        Some(&malformed_acceptance_request.request_id)
    );
    assert!(matches!(
        malformed_acceptance_completions.recv().await,
        Some(Err(ResourceTransportFailure::Shape))
    ));
    assert!(resources.contains_key(&sibling_request.stream_id));

    let malformed_value = RuntimeValue::Text("late wrong type".to_owned());
    let malformed_byte_count = encode_constructed_value(&active, &registry, &malformed_value)
        .expect("encoded malformed late value")
        .len() as u32;
    let malformed_values = ResourceServerFrame::Values(ResourceValues {
        stream_id: malformed_values_request.stream_id,
        request_id: malformed_values_request.request_id,
        target_revision: active.pair(),
        batch_sequence: 0,
        item_count: 1,
        byte_count: malformed_byte_count,
        values: vec![malformed_value],
    });
    let bytes = encode_resource_server_frame(&active, &registry, &malformed_values)
        .expect("malformed late values encode");
    handle_shared_broker_frame(
        BrokerWireFrame {
            resource: true,
            bytes,
        },
        &mut writer,
        &active,
        &registry,
        &mut root,
        &mut resources,
        resource_high_water_mark,
        &mut tombstones,
        &resource_terminal_provenance,
    )
    .await
    .expect("outer broker contains malformed late values");
    assert!(!resources.contains_key(&malformed_values_request.stream_id));
    assert_eq!(
        tombstones.get(&malformed_values_request.stream_id),
        Some(&malformed_values_request.request_id)
    );
    assert!(matches!(
        malformed_values_completions.recv().await,
        Some(Err(ResourceTransportFailure::Shape))
    ));
    assert!(resources.contains_key(&sibling_request.stream_id));

    for frame in [
        ResourceServerFrame::Accepted(sibling_accepted),
        ResourceServerFrame::Values(sibling_values),
    ] {
        let bytes = encode_resource_server_frame(&active, &registry, &frame)
            .expect("encoded sibling resource response");
        handle_shared_broker_frame(
            BrokerWireFrame {
                resource: true,
                bytes,
            },
            &mut writer,
            &active,
            &registry,
            &mut root,
            &mut resources,
            resource_high_water_mark,
            &mut tombstones,
            &resource_terminal_provenance,
        )
        .await
        .expect("sibling resource remains isolated");
    }
    assert!(resources.contains_key(&request.stream_id));
    assert!(resources.contains_key(&sibling_request.stream_id));
    assert!(sibling_completions.try_recv().is_err());

    let bytes = encode_resource_server_frame(
        &active,
        &registry,
        &ResourceServerFrame::Completed(completed),
    )
    .expect("encoded late completion");
    handle_shared_broker_frame(
        BrokerWireFrame {
            resource: true,
            bytes,
        },
        &mut writer,
        &active,
        &registry,
        &mut root,
        &mut resources,
        resource_high_water_mark,
        &mut tombstones,
        &resource_terminal_provenance,
    )
    .await
    .expect("late completion closes the cancelled primary resource");
    assert!(!resources.contains_key(&request.stream_id));
    assert!(resources.contains_key(&sibling_request.stream_id));
    assert_eq!(
        tombstones.get(&request.stream_id),
        Some(&request.request_id)
    );
    assert!(matches!(
        completions.recv().await,
        Some(Ok(ResourceTransportOutcome::Cancelled {
            nested_invocation_id: Some(actual),
        })) if actual == nested_invocation_id
    ));
    assert!(sibling_completions.try_recv().is_err());

    let bytes = encode_resource_server_frame(
        &active,
        &registry,
        &ResourceServerFrame::Completed(sibling_completed),
    )
    .expect("encoded sibling completion");
    handle_shared_broker_frame(
        BrokerWireFrame {
            resource: true,
            bytes,
        },
        &mut writer,
        &active,
        &registry,
        &mut root,
        &mut resources,
        resource_high_water_mark,
        &mut tombstones,
        &resource_terminal_provenance,
    )
    .await
    .expect("sibling completion remains isolated");
    assert!(resources.is_empty());
    assert!(matches!(
        sibling_completions.recv().await,
        Some(Ok(ResourceTransportOutcome::Ready {
            value: RuntimeValue::Integer(8),
            nested_invocation_id,
        })) if nested_invocation_id == sibling_nested_invocation_id
    ));
}

#[tokio::test]
async fn broker_publishes_late_committed_failure_after_cancel() {
    let (active, registry) = transport_test_context();
    let request = transport_test_request(active.pair(), 1);
    let nested_invocation_id = InvocationId::from_bytes([0x40; 16]);
    let accepted = ResourceAccepted {
        stream_id: request.stream_id,
        request_id: request.request_id,
        nested_invocation_id,
        target_revision: request.target_revision,
        resource_kind: request.resource_kind,
    };
    let mut protocol = ResourceProtocolConnection::new();
    protocol
        .open(request.clone())
        .expect("resource request opens");
    protocol
        .apply_constructed(&active, &registry, ResourceServerFrame::Accepted(accepted))
        .expect("resource acceptance applies");
    protocol
        .receive(ResourceClientFrame::Cancel(ResourceCancel {
            stream_id: request.stream_id,
            request_id: request.request_id,
            reason: ResourceCancellationCode::ParentInvocationCancelled,
        }))
        .expect("resource cancellation applies");
    let (completion, mut completions) = mpsc::channel(2);
    let mut state = BrokerResourceState {
        request: request.clone(),
        expected_type: ResolvedType::Scalar(StandardScalar::Integer),
        resource_kind: ProtocolResourceKind::Single,
        protocol,
        completion,
        accepted: true,
        accepted_nested_invocation_id: Some(InvocationId::from_bytes([0x40; 16])),
        scalar_value: None,
        cancellation_requested: true,
        stream_values_seen: false,
        terminal_provenance: ResourceTerminalProvenance::Authenticated,
        scalar_value_after_cancellation: false,
    };
    let (_reader, mut writer) = tokio::io::duplex(128);
    let keep = handle_shared_resource_frame(
        &mut state,
        ResourceServerFrame::Failed(ResourceFailed {
            stream_id: request.stream_id,
            request_id: request.request_id,
            target_revision: active.pair(),
            failure: CallFailure::ExecuteDenied,
        }),
        &mut writer,
        &active,
        &registry,
    )
    .await
    .expect("late committed failure is valid");
    assert!(!keep);
    let Some(Ok(ResourceTransportOutcome::Failed {
        failure: CallFailure::ExecuteDenied,
        nested_invocation_id: terminal_nested_invocation_id,
    })) = completions.recv().await
    else {
        panic!("expected committed failure outcome");
    };
    assert_eq!(terminal_nested_invocation_id, Some(nested_invocation_id));
}

#[tokio::test]
async fn broker_publishes_late_committed_completed_after_cancel() {
    let (active, registry) = transport_test_context();
    let request = transport_test_request(active.pair(), 1);
    let accepted = ResourceAccepted {
        stream_id: request.stream_id,
        request_id: request.request_id,
        nested_invocation_id: InvocationId::from_bytes([0x40; 16]),
        target_revision: request.target_revision,
        resource_kind: request.resource_kind,
    };
    let value = RuntimeValue::Integer(7);
    let byte_count = encode_constructed_value(&active, &registry, &value)
        .expect("encoded resource value")
        .len() as u32;
    let values = ResourceValues {
        stream_id: request.stream_id,
        request_id: request.request_id,
        target_revision: active.pair(),
        batch_sequence: 0,
        item_count: 1,
        byte_count,
        values: vec![value],
    };
    let mut protocol = ResourceProtocolConnection::new();
    protocol
        .open(request.clone())
        .expect("resource request opens");
    protocol
        .apply_constructed(&active, &registry, ResourceServerFrame::Accepted(accepted))
        .expect("resource acceptance applies");
    protocol
        .receive(ResourceClientFrame::Cancel(ResourceCancel {
            stream_id: request.stream_id,
            request_id: request.request_id,
            reason: ResourceCancellationCode::ParentInvocationCancelled,
        }))
        .expect("resource cancellation applies");
    let (completion, mut completions) = mpsc::channel(2);
    let mut state = BrokerResourceState {
        request: request.clone(),
        expected_type: ResolvedType::Scalar(StandardScalar::Integer),
        resource_kind: ProtocolResourceKind::Single,
        protocol,
        completion,
        accepted: true,
        accepted_nested_invocation_id: Some(InvocationId::from_bytes([0x40; 16])),
        scalar_value: None,
        cancellation_requested: true,
        stream_values_seen: false,
        terminal_provenance: ResourceTerminalProvenance::Authenticated,
        scalar_value_after_cancellation: false,
    };
    let (_reader, mut writer) = tokio::io::duplex(128);
    assert!(
        handle_shared_resource_frame(
            &mut state,
            ResourceServerFrame::Values(values),
            &mut writer,
            &active,
            &registry,
        )
        .await
        .expect("late committed values are drained")
    );
    let keep = handle_shared_resource_frame(
        &mut state,
        ResourceServerFrame::Completed(ResourceCompleted {
            stream_id: request.stream_id,
            request_id: request.request_id,
            target_revision: active.pair(),
            final_batch_sequence: 0,
            total_items: 1,
        }),
        &mut writer,
        &active,
        &registry,
    )
    .await
    .expect("late committed completion is published");
    assert!(!keep);
    assert!(matches!(
        completions.recv().await,
        Some(Ok(ResourceTransportOutcome::Ready {
            value: RuntimeValue::Integer(7),
            nested_invocation_id,
        })) if nested_invocation_id == InvocationId::from_bytes([0x40; 16])
    ));
}

#[tokio::test]
async fn broker_publishes_cancelled_for_dropped_late_completed_without_value() {
    let (active, registry) = transport_test_context();
    let request = transport_test_request(active.pair(), 1);
    let accepted = ResourceAccepted {
        stream_id: request.stream_id,
        request_id: request.request_id,
        nested_invocation_id: InvocationId::from_bytes([0x40; 16]),
        target_revision: request.target_revision,
        resource_kind: request.resource_kind,
    };
    let mut protocol = ResourceProtocolConnection::new();
    protocol
        .open(request.clone())
        .expect("resource request opens");
    protocol
        .apply_constructed(&active, &registry, ResourceServerFrame::Accepted(accepted))
        .expect("resource acceptance applies");
    protocol
        .receive(ResourceClientFrame::Cancel(ResourceCancel {
            stream_id: request.stream_id,
            request_id: request.request_id,
            reason: ResourceCancellationCode::ParentInvocationCancelled,
        }))
        .expect("resource cancellation applies");
    let (completion, mut completions) = mpsc::channel(2);
    let mut state = BrokerResourceState {
        request: request.clone(),
        expected_type: ResolvedType::Scalar(StandardScalar::Integer),
        resource_kind: ProtocolResourceKind::Single,
        protocol,
        completion,
        accepted: true,
        accepted_nested_invocation_id: Some(InvocationId::from_bytes([0x40; 16])),
        scalar_value: None,
        cancellation_requested: true,
        stream_values_seen: false,
        terminal_provenance: ResourceTerminalProvenance::Uncommitted,
        scalar_value_after_cancellation: false,
    };
    let (_reader, mut writer) = tokio::io::duplex(128);
    let keep = handle_shared_resource_frame(
        &mut state,
        ResourceServerFrame::Completed(ResourceCompleted {
            stream_id: request.stream_id,
            request_id: request.request_id,
            target_revision: active.pair(),
            final_batch_sequence: 0,
            total_items: 1,
        }),
        &mut writer,
        &active,
        &registry,
    )
    .await
    .expect("late uncommitted completion is superseded by cancellation");
    assert!(!keep);
    assert!(matches!(
        completions.recv().await,
        Some(Ok(ResourceTransportOutcome::Cancelled {
            nested_invocation_id: Some(actual),
        })) if actual == InvocationId::from_bytes([0x40; 16])
    ));
}

#[tokio::test]
async fn broker_publishes_cancelled_after_late_failure_before_acceptance() {
    let (active, registry) = transport_test_context();
    let request = transport_test_request(active.pair(), 1);
    let mut protocol = ResourceProtocolConnection::new();
    protocol
        .open(request.clone())
        .expect("resource request opens");
    protocol
        .receive(ResourceClientFrame::Cancel(ResourceCancel {
            stream_id: request.stream_id,
            request_id: request.request_id,
            reason: ResourceCancellationCode::ParentInvocationCancelled,
        }))
        .expect("resource cancellation applies");
    let (completion, mut completions) = mpsc::channel(2);
    let mut state = BrokerResourceState {
        request: request.clone(),
        expected_type: ResolvedType::Scalar(StandardScalar::Integer),
        resource_kind: ProtocolResourceKind::Single,
        protocol,
        completion,
        accepted: false,
        accepted_nested_invocation_id: None,
        scalar_value: None,
        cancellation_requested: true,
        stream_values_seen: false,
        terminal_provenance: ResourceTerminalProvenance::Uncommitted,
        scalar_value_after_cancellation: false,
    };
    let (_reader, mut writer) = tokio::io::duplex(128);
    let keep = handle_shared_resource_frame(
        &mut state,
        ResourceServerFrame::Failed(ResourceFailed {
            stream_id: request.stream_id,
            request_id: request.request_id,
            target_revision: active.pair(),
            failure: CallFailure::ExecuteDenied,
        }),
        &mut writer,
        &active,
        &registry,
    )
    .await
    .expect("late failure is superseded by cancellation");
    assert!(!keep);
    assert!(matches!(
        completions.recv().await,
        Some(Ok(ResourceTransportOutcome::Cancelled {
            nested_invocation_id: None,
        }))
    ));
}

#[tokio::test]
async fn broker_rejects_zero_nested_invocation_identity_for_streams() {
    let (active, registry) = transport_test_context();
    let mut request = transport_test_request(active.pair(), 1);
    request.resource_kind = ProtocolResourceKind::Stream;
    let mut protocol = ResourceProtocolConnection::new();
    protocol
        .open(request.clone())
        .expect("resource request opens");
    let (completion, _completions) = mpsc::channel(1);
    let mut state = BrokerResourceState {
        request: request.clone(),
        expected_type: ResolvedType::Scalar(StandardScalar::Integer),
        resource_kind: ProtocolResourceKind::Stream,
        protocol,
        completion,
        accepted: false,
        accepted_nested_invocation_id: None,
        scalar_value: None,
        cancellation_requested: false,
        stream_values_seen: false,
        terminal_provenance: ResourceTerminalProvenance::Uncommitted,
        scalar_value_after_cancellation: false,
    };
    let (_reader, mut writer) = tokio::io::duplex(128);
    let result = handle_shared_resource_frame(
        &mut state,
        ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id: request.stream_id,
            request_id: request.request_id,
            nested_invocation_id: InvocationId::from_bytes([0; 16]),
            target_revision: request.target_revision,
            resource_kind: ProtocolResourceKind::Stream,
        }),
        &mut writer,
        &active,
        &registry,
    )
    .await;
    assert!(matches!(result, Err(ResourceTransportFailure::Shape)));
    assert!(!state.accepted);
    assert_eq!(state.accepted_nested_invocation_id, None);
}

#[tokio::test]
async fn broker_publishes_cancelled_without_nested_identity_before_acceptance() {
    let (active, registry) = transport_test_context();
    let request = transport_test_request(active.pair(), 1);
    let mut protocol = ResourceProtocolConnection::new();
    protocol
        .open(request.clone())
        .expect("resource request opens");
    protocol
        .receive(ResourceClientFrame::Cancel(ResourceCancel {
            stream_id: request.stream_id,
            request_id: request.request_id,
            reason: ResourceCancellationCode::ParentInvocationCancelled,
        }))
        .expect("resource cancellation applies");
    let (completion, mut completions) = mpsc::channel(1);
    let mut state = BrokerResourceState {
        request: request.clone(),
        expected_type: ResolvedType::Scalar(StandardScalar::Integer),
        resource_kind: ProtocolResourceKind::Single,
        protocol,
        completion,
        accepted: false,
        accepted_nested_invocation_id: None,
        scalar_value: None,
        cancellation_requested: true,
        stream_values_seen: false,
        terminal_provenance: ResourceTerminalProvenance::Uncommitted,
        scalar_value_after_cancellation: false,
    };
    let (_reader, mut writer) = tokio::io::duplex(128);
    let keep = handle_shared_resource_frame(
        &mut state,
        ResourceServerFrame::Cancelled(ResourceCancelled {
            stream_id: request.stream_id,
            request_id: request.request_id,
            target_revision: active.pair(),
            reason: ResourceCancellationCode::ParentInvocationCancelled,
        }),
        &mut writer,
        &active,
        &registry,
    )
    .await
    .expect("pre-accept cancellation closes the resource");
    assert!(!keep);
    assert!(matches!(
        completions.recv().await,
        Some(Ok(ResourceTransportOutcome::Cancelled {
            nested_invocation_id: None,
        }))
    ));
}

#[test]
fn cancellation_waiter_suppresses_stream_values_before_terminal() {
    let (sender, receiver) = mpsc::channel(2);
    let producer = thread::spawn(move || {
        sender
            .blocking_send(Ok(ResourceTransportOutcome::StreamValues(vec![
                RuntimeValue::Integer(1),
            ])))
            .expect("stream batch reaches cancellation waiter");
        sender
            .blocking_send(Ok(ResourceTransportOutcome::Cancelled {
                nested_invocation_id: None,
            }))
            .expect("terminal cancellation reaches cancellation waiter");
    });
    let (result, waiter, completion) =
        InstalledClientResourceExecutor::wait_for_cancelled_transport(receiver)
            .expect("cancellation waiter returns a terminal result");
    assert!(waiter.is_none());
    assert!(completion.is_none());
    assert!(matches!(
        result,
        Some(CancellationWaitOutcome::Terminal(Ok(
            ResourceTransportOutcome::Cancelled {
                nested_invocation_id: None,
            },
        )))
    ));
    producer.join().expect("cancellation producer");
}

#[test]
fn cancellation_disposition_preserves_committed_terminals_and_drops_late_frames() {
    use orna_protocol::ResourceFrameDisposition::{Applied, DroppedLate};

    assert_eq!(
        resource_transport_disposition_action(
            Applied,
            true,
            true,
            ResourceTerminalProvenance::Authenticated,
            false,
        ),
        ResourceFrameDispositionAction::Apply,
    );
    assert_eq!(
        resource_transport_disposition_action(
            Applied,
            true,
            true,
            ResourceTerminalProvenance::Uncommitted,
            false,
        ),
        ResourceFrameDispositionAction::Cancel,
    );
    assert_eq!(
        resource_transport_disposition_action(
            Applied,
            true,
            false,
            ResourceTerminalProvenance::Uncommitted,
            false,
        ),
        ResourceFrameDispositionAction::Drain,
    );
    assert_eq!(
        resource_transport_disposition_action(
            DroppedLate,
            true,
            true,
            ResourceTerminalProvenance::Authenticated,
            false,
        ),
        ResourceFrameDispositionAction::Apply,
    );
    assert_eq!(
        resource_transport_disposition_action(
            DroppedLate,
            true,
            true,
            ResourceTerminalProvenance::Uncommitted,
            false,
        ),
        ResourceFrameDispositionAction::Cancel,
    );
    assert_eq!(
        resource_transport_disposition_action(
            DroppedLate,
            true,
            false,
            ResourceTerminalProvenance::Uncommitted,
            false,
        ),
        ResourceFrameDispositionAction::Drop,
    );
    assert_eq!(
        resource_transport_disposition_action(
            DroppedLate,
            false,
            true,
            ResourceTerminalProvenance::Uncommitted,
            false,
        ),
        ResourceFrameDispositionAction::Reject,
    );
    assert_eq!(
        resource_transport_disposition_action(
            Applied,
            true,
            true,
            ResourceTerminalProvenance::Uncommitted,
            true,
        ),
        ResourceFrameDispositionAction::Apply,
    );
    assert_eq!(
        resource_transport_disposition_action(
            DroppedLate,
            true,
            true,
            ResourceTerminalProvenance::Uncommitted,
            true,
        ),
        ResourceFrameDispositionAction::Apply,
    );
    assert_eq!(
        resource_transport_disposition_action(
            Applied,
            false,
            false,
            ResourceTerminalProvenance::Uncommitted,
            false,
        ),
        ResourceFrameDispositionAction::Apply,
    );
    assert_eq!(
        resource_transport_disposition_action(
            DroppedLate,
            false,
            false,
            ResourceTerminalProvenance::Uncommitted,
            false,
        ),
        ResourceFrameDispositionAction::Reject,
    );
}

#[test]
fn terminal_provenance_is_removed_after_consumption() {
    let (broker, _receiver) = SharedInvokeBroker::pending();
    let request_id = InvocationId::new();
    broker.record_resource_terminal_provenance(
        7,
        request_id,
        ResourceTerminalProvenance::Authenticated,
    );
    assert_eq!(
        broker.resource_terminal_provenance(7, request_id),
        ResourceTerminalProvenance::Authenticated,
    );
    broker.take_resource_terminal_provenance(7, request_id);
    assert_eq!(
        broker.resource_terminal_provenance(7, request_id),
        ResourceTerminalProvenance::Uncommitted,
    );
}

#[test]
fn cancellation_decision_only_returns_cancelled_when_request_wins() {
    assert_eq!(
        resource_transport_cancellation_action(true),
        ResourceTransportCancellationAction::ReturnCancelled,
    );
    assert_eq!(
        resource_transport_cancellation_action(false),
        ResourceTransportCancellationAction::ContinueCommitted,
    );
}

#[test]
fn values_go_to_stdout_and_progress_to_stderr_without_interleave() {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let outcome = render_event_stream(
        &echo_events(),
        false,
        &mut stdout,
        &mut stderr,
        &mut encoder,
    )
    .expect("rendering succeeds");
    assert_eq!(outcome, InstalledInvokeOutcome::Completed);
    assert_eq!(stdout, encoded_record());
    let stderr = String::from_utf8(stderr).expect("stderr is text");
    assert!(stderr.contains("invocation started"));
    assert!(stderr.contains("invocation completed in 7ns"));
}

#[test]
fn no_progress_suppresses_diagnostics_but_keeps_values() {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let outcome = render_event_stream(&echo_events(), true, &mut stdout, &mut stderr, &mut encoder)
        .expect("rendering succeeds");
    assert_eq!(outcome, InstalledInvokeOutcome::Completed);
    assert_eq!(stdout, encoded_record());
    assert!(stderr.is_empty());
}

#[test]
fn each_value_writes_one_canonical_record() {
    let invocation = InvocationId::new();
    let values = InvokeEvent::new(
        invocation,
        0,
        InvocationEventBody::value_batch(
            None,
            [
                InvokeValue::new(RuntimeValue::Integer(1)).expect("first value"),
                InvokeValue::new(RuntimeValue::Integer(2)).expect("second value"),
            ],
        )
        .expect("value batch body"),
    )
    .expect("values event");
    let batch = InvocationEventBatch::new(vec![InvocationEventRecord::new(1, values)])
        .expect("event batch");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let outcome = render_event_stream(&batch, false, &mut stdout, &mut stderr, &mut encoder)
        .expect("rendering succeeds");
    assert_eq!(outcome, InstalledInvokeOutcome::Completed);
    assert_eq!(stdout, [encoded_record(), encoded_record()].concat());
}

#[test]
fn denied_prints_one_redacted_line_and_exits_denied() {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let result = SealedInvocationResult::Denied {
        invocation: InvocationId::new(),
    };
    let outcome = render_result(&result, false, &mut stdout, &mut stderr, &mut encoder)
        .expect("rendering succeeds");
    assert_eq!(outcome, InstalledInvokeOutcome::Denied);
    assert!(stdout.is_empty());
    assert_eq!(
        String::from_utf8(stderr).expect("stderr is text"),
        "orna: invoke: invocation denied\n"
    );
}

#[test]
fn failed_event_prints_one_redacted_line_and_exits_target_failure() {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let invocation = InvocationId::new();
    let started = InvokeEvent::new(
        invocation,
        0,
        InvocationEventBody::Started {
            visible_principal: None,
        },
    )
    .expect("started event");
    let failure = InvocationFailure::new(
        InvocationFailurePhase::Bind,
        "INVOKE_BIND_FAILED",
        "invocation arguments were not accepted",
        None,
        InvocationRetryability::No,
    )
    .expect("failure body");
    let failed = InvokeEvent::new(invocation, 1, InvocationEventBody::Failed(failure))
        .expect("failed event");
    let events = InvocationEventBatch::new(vec![
        InvocationEventRecord::new(1, started),
        InvocationEventRecord::new(2, failed),
    ])
    .expect("event batch");
    let result = SealedInvocationResult::Failed { invocation, events };
    let outcome = render_result(&result, false, &mut stdout, &mut stderr, &mut encoder)
        .expect("rendering succeeds");
    assert_eq!(outcome, InstalledInvokeOutcome::TargetFailure);
    assert!(stdout.is_empty());
    assert_eq!(
        String::from_utf8(stderr).expect("stderr is text"),
        "orna: invoke: invocation started\norna: invoke: invocation failed\n"
    );
}

#[test]
fn presentation_failure_returns_the_closed_presentation_error() {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let result = SealedInvocationResult::PresentationFailed {
        invocation: InvocationId::new(),
    };
    let error = render_result(&result, false, &mut stdout, &mut stderr, &mut encoder)
        .expect_err("a presentation failure is a closed presentation error");
    assert_eq!(error.kind(), InstalledInvokeErrorKind::Presentation);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
}

/// Builds one canonical `std.terminal.Document` payload frame.
fn document_frame(body: &[u8]) -> Vec<u8> {
    let mut frame = b"ORNA-TERMINAL-DOCUMENT/1 ".to_vec();
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(body);
    frame
}

/// Builds one canonical `std.io.ByteStream` payload frame.
fn byte_stream_frame(media_type: &[u8], body: &[u8]) -> Vec<u8> {
    let mut frame = b"ORNA-BYTE-STREAM/1 ".to_vec();
    frame.extend_from_slice(&(media_type.len() as u32).to_be_bytes());
    frame.extend_from_slice(media_type);
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(body);
    frame
}

#[test]
fn opaque_terminal_values_render_through_event_stream_to_clean_channels() {
    let (active, registry) = transport_test_context();
    let document_body = b"name | status\nalice | ready\n";
    let byte_stream_body = br#"{"ok":true}"#;
    let document = RuntimeValue::Opaque(
        OpaqueValue::new(
            &active,
            &registry,
            STD_TERMINAL_DOCUMENT_TYPE_ID,
            document_frame(document_body),
        )
        .expect("registered document codec accepts the payload"),
    );
    let byte_stream = RuntimeValue::Opaque(
        OpaqueValue::new(
            &active,
            &registry,
            STD_IO_BYTE_STREAM_TYPE_ID,
            byte_stream_frame(b"application/json", byte_stream_body),
        )
        .expect("registered byte-stream codec accepts the payload"),
    );
    let invocation = InvocationId::from_bytes([0x72; 16]);
    let started = InvokeEvent::new(
        invocation,
        0,
        InvocationEventBody::Started {
            visible_principal: None,
        },
    )
    .expect("started event");
    let values = InvokeEvent::new(
        invocation,
        1,
        InvocationEventBody::value_batch(
            None,
            [
                InvokeValue::new(document).expect("document value"),
                InvokeValue::new(byte_stream).expect("byte-stream value"),
            ],
        )
        .expect("value batch body"),
    )
    .expect("values event");
    let completed = InvokeEvent::new(
        invocation,
        2,
        InvocationEventBody::Completed {
            duration_nanoseconds: 7,
        },
    )
    .expect("completed event");
    let events = InvocationEventBatch::new(vec![
        InvocationEventRecord::new(1, started),
        InvocationEventRecord::new(2, values),
        InvocationEventRecord::new(3, completed),
    ])
    .expect("event batch");

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let outcome = render_event_stream(&events, false, &mut stdout, &mut stderr, &mut encoder)
        .expect("rendering succeeds");

    assert_eq!(outcome, InstalledInvokeOutcome::Completed);
    assert_eq!(
        stdout,
        [document_body.as_slice(), byte_stream_body.as_slice()].concat()
    );
    assert_eq!(
        stderr,
        b"orna: invoke: invocation started\norna: invoke: invocation completed in 7ns\n"
    );
}

#[test]
fn selection_rule_maps_document_and_byte_stream_to_the_tty_runtime() {
    let cases = [
        (
            STD_TERMINAL_DOCUMENT_TYPE_ID,
            Some(orna_runtime_tty::Sink::Document),
        ),
        (
            STD_IO_BYTE_STREAM_TYPE_ID,
            Some(orna_runtime_tty::Sink::ByteStream),
        ),
        (TypeId::from_bytes([0x41; 16]), None),
    ];
    for (opaque_type, expected) in cases {
        assert_eq!(select_runtime_sink(opaque_type), expected);
    }
}

#[test]
fn document_value_renders_as_document_text_on_stdout() {
    let body = b"name | age\nalice | 41\n";
    let frame = document_frame(body);
    let mut stdout = Vec::new();
    render_opaque_payload(orna_runtime_tty::Sink::Document, &frame, &mut stdout)
        .expect("rendering a document frame succeeds");
    assert_eq!(stdout, body);
}

#[test]
fn byte_stream_value_renders_as_raw_bytes_on_stdout() {
    let body = b"{\"ok\":true}";
    let frame = byte_stream_frame(b"application/json", body);
    let mut stdout = Vec::new();
    render_opaque_payload(orna_runtime_tty::Sink::ByteStream, &frame, &mut stdout)
        .expect("rendering a byte-stream frame succeeds");
    // The stream bytes go to stdout with no envelope, progress
    // interleave, or trailing record newline.
    assert_eq!(stdout, body);
}

#[test]
fn a_rejected_runtime_payload_writes_nothing_and_returns_internal() {
    let mut stdout = Vec::new();
    let error = render_opaque_payload(
        orna_runtime_tty::Sink::Document,
        b"ORNA-TERMINAL-DOCUMENT/1 \0\0\0\x05broken",
        &mut stdout,
    )
    .expect_err("an inconsistent frame is rejected");
    assert_eq!(error.kind(), InstalledInvokeErrorKind::Internal);
    assert!(stdout.is_empty());
}

#[test]
fn non_sink_values_keep_the_orv5_envelope() {
    let mut stdout = Vec::new();
    render_value(&RuntimeValue::Integer(41), &mut stdout, &mut encoder)
        .expect("rendering a non-sink value succeeds");
    assert_eq!(stdout, encoded_record());
}

#[test]
fn the_client_offer_names_the_tty_runtime() {
    let request = InstalledInvokeRequest::new(
        InvocationTarget::qualified_name(
            QualifiedSemanticName::new(["std", "invoke", "echo"]).expect("qualified name"),
        )
        .expect("target"),
        Vec::new(),
        None,
        None,
        false,
        false,
        None,
    );
    let sealed = build_sealed_request(&request, Vec::new(), RuntimeFamily::Tty)
        .expect("the sealed request builds");
    let offer = sealed.client_offer();
    assert_eq!(offer.sink_offers().len(), 2);
    assert_eq!(
        offer.sink_offers()[0].descriptor(),
        &TypeDescriptor::named(STD_TERMINAL_DOCUMENT_TYPE_ID)
    );
    assert_eq!(
        offer.sink_offers()[0].media_types(),
        &[DOCUMENT_SINK_MEDIA_TYPE.to_owned()]
    );
    assert!(!offer.sink_offers()[0].streaming());
    assert_eq!(
        offer.sink_offers()[1].descriptor(),
        &TypeDescriptor::named(STD_IO_BYTE_STREAM_TYPE_ID)
    );
    assert_eq!(
        offer.sink_offers()[1].media_types(),
        &[BYTE_STREAM_SINK_MEDIA_TYPE.to_owned()]
    );
    assert!(!offer.sink_offers()[1].streaming());
    // The installed tty runtime offer survives the sealed request
    // construction (ADR 0063).
    assert_eq!(offer.runtime_offers().len(), 1);
    let runtime = &offer.runtime_offers()[0];
    assert_eq!(runtime.name(), "tty");
    assert_eq!(runtime.version(), orna_runtime_tty::RUNTIME_VERSION);
    assert!(!runtime.version().is_empty());
    assert_eq!(
        runtime.consumed_descriptors(),
        &[
            TypeDescriptor::named(STD_TERMINAL_DOCUMENT_TYPE_ID),
            TypeDescriptor::named(STD_IO_BYTE_STREAM_TYPE_ID),
        ]
    );
    assert!(runtime.contracts().is_empty());
    assert_eq!(runtime.preference_rank(), 0);
    assert!(runtime.trusted());
    assert!(runtime.limits().is_none());
}

/// Builds one echo request with the given runtime override.
fn runtime_request(runtime: Option<RuntimeFamily>) -> InstalledInvokeRequest {
    InstalledInvokeRequest::new(
        InvocationTarget::qualified_name(
            QualifiedSemanticName::new(["std", "invoke", "echo"]).expect("qualified name"),
        )
        .expect("target"),
        Vec::new(),
        None,
        None,
        false,
        false,
        runtime,
    )
}

#[test]
fn selection_policy_defaults_to_tty_for_console_and_selects_qt_for_ui() {
    assert_eq!(
        selected_runtime(&runtime_request(None), false),
        Ok(RuntimeFamily::Tty)
    );
    assert_eq!(
        selected_runtime(&runtime_request(Some(RuntimeFamily::Tty)), false),
        Ok(RuntimeFamily::Tty)
    );
    let error = selected_runtime(&runtime_request(Some(RuntimeFamily::Qt)), false)
        .expect_err("Qt cannot consume a terminal result");
    assert_eq!(error.kind(), InstalledInvokeErrorKind::Usage);
    assert!(error.message().contains("std.ui.UI"));
    assert_eq!(
        selected_runtime(&runtime_request(None), true),
        Ok(RuntimeFamily::Qt)
    );
    let error = selected_runtime(&runtime_request(Some(RuntimeFamily::Tty)), true)
        .expect_err("TTY cannot consume a UI result");
    assert_eq!(error.kind(), InstalledInvokeErrorKind::Usage);
    assert!(error.message().contains("std.ui.UI"));
    let error = selected_runtime(&runtime_request(Some(RuntimeFamily::NotInstalled)), false)
        .expect_err("a not-installed family is rejected");
    assert_eq!(error.kind(), InstalledInvokeErrorKind::Usage);
    assert!(error.message().contains("not-installed"));
}
#[test]
fn qt_loader_failures_are_presentation_errors() {
    for error in [
        orna_client::RuntimeLoadError::UnsupportedPlatform,
        orna_client::RuntimeLoadError::LibraryUnavailable,
        orna_client::RuntimeLoadError::QuerySymbolUnavailable,
        orna_client::RuntimeLoadError::NullApi,
        orna_client::RuntimeLoadError::ApiAbiMismatch,
        orna_client::RuntimeLoadError::MissingApiFunction,
        orna_client::RuntimeLoadError::NullDescriptor,
        orna_client::RuntimeLoadError::MalformedDescriptor,
        orna_client::RuntimeLoadError::DescriptorAbiMismatch,
        orna_client::RuntimeLoadError::DescriptorIdentityMismatch,
        orna_client::RuntimeLoadError::DescriptorPlatformMismatch,
        orna_client::RuntimeLoadError::DescriptorThreadModelMismatch,
        orna_client::RuntimeLoadError::DescriptorFeatureMismatch,
        orna_client::RuntimeLoadError::DescriptorSinkOffersMismatch,
        orna_client::RuntimeLoadError::DescriptorContractOffersMismatch,
        orna_client::RuntimeLoadError::DescriptorLimitExceeded,
    ] {
        let mapped = map_qt_runtime_load_error(error);
        assert_eq!(mapped.kind(), InstalledInvokeErrorKind::Presentation);
        assert_eq!(mapped.message(), "the installed Qt runtime is unavailable");
    }
}

#[test]
fn tty_default_selection_maps_document_and_byte_stream() {
    let selected =
        selected_runtime(&runtime_request(None), false).expect("the default selects TTY");
    assert_eq!(selected, RuntimeFamily::Tty);
    // The tty family's sink map consumes exactly the two standard
    // sink types; a UI value keeps the ORV5 envelope.
    assert_eq!(
        select_runtime_sink(STD_TERMINAL_DOCUMENT_TYPE_ID),
        Some(orna_runtime_tty::Sink::Document)
    );
    assert_eq!(
        select_runtime_sink(STD_IO_BYTE_STREAM_TYPE_ID),
        Some(orna_runtime_tty::Sink::ByteStream)
    );
    assert_eq!(select_runtime_sink(STD_UI_TYPE_ID), None);
}

fn echo_definition() -> FunctionDefinition {
    FunctionDefinition::new(
        FunctionId::from_bytes([0x10; 16]),
        QualifiedSemanticName::new(["std", "invoke", "echo"]).expect("qualified name"),
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            ParameterId::from_bytes([0x10; 16]),
            "p_value",
            0,
            ResolvedType::Scalar(StandardScalar::Integer),
            None,
        )],
        FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Integer)),
        FunctionRevisionId::from_bytes([0x20; 16]),
        FunctionSecurity::Invoker,
        None,
        FunctionVolatility::Immutable,
    )
}

fn value_typed_definition(resolved_type: ResolvedType) -> FunctionDefinition {
    FunctionDefinition::new(
        FunctionId::from_bytes([0x11; 16]),
        QualifiedSemanticName::new(["app", "update"]).expect("qualified name"),
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            ParameterId::from_bytes([0x11; 16]),
            "p_value",
            0,
            resolved_type,
            None,
        )],
        FunctionReturn::Single(resolved_type),
        FunctionRevisionId::from_bytes([0x21; 16]),
        FunctionSecurity::Invoker,
        None,
        FunctionVolatility::Immutable,
    )
}

fn pipe_request() -> InvokeRequest {
    let caller = InvocationCallerContext::new(
        InvocationCallerKind::CliPipe,
        false,
        false,
        None,
        None,
        "en-GB",
        "UTC",
        None,
    )
    .expect("pipe caller context");
    let offer = InvocationClientOffer::new(
        5,
        "en-GB",
        "UTC",
        Vec::new(),
        installed_tty_runtime_offers(),
        MAXIMUM_FRAME_SIZE,
        MAXIMUM_ARTIFACT_SIZE,
        None,
        None,
    )
    .expect("client offer");
    InvokeRequest::new(InvokeRequestInput {
        target: InvocationTarget::qualified_name(
            QualifiedSemanticName::new(["std", "invoke", "echo"]).expect("qualified name"),
        )
        .expect("target"),
        arguments: Vec::new(),
        caller_context: caller,
        client_offer: offer,
        output_requirement: None,
        state_profile: None,
        trace_policy: InvocationTracePolicy::Off,
        idempotency_key: None,
        parent_invocation_id: None,
        observer_context: None,
    })
    .expect("checked request")
}

#[test]
fn render_return_type_preserves_scalar_and_rows_and_names_stream_items() {
    assert_eq!(
        render_return_type(&FunctionReturn::Single(ResolvedType::Scalar(
            StandardScalar::Integer,
        ))),
        "INTEGER",
    );
    assert_eq!(
        render_return_type(&FunctionReturn::Stream(ResolvedType::Scalar(
            StandardScalar::Integer,
        ))),
        "STREAM<INTEGER>",
    );
    assert_eq!(
        render_return_type(&FunctionReturn::Rows(vec![
            orna_core::catalogue::FunctionReturnColumnDefinition::new(
                "value",
                0,
                ResolvedType::Scalar(StandardScalar::Integer),
            ),
        ])),
        "ROWS (value INTEGER)",
    );
}
#[test]
fn explain_final_sink_matches_implicit_tty_opaque_routing() {
    assert_eq!(
        render_explain_final_sink(
            None,
            &FunctionReturn::Single(ResolvedType::Value(STD_TERMINAL_DOCUMENT_TYPE_ID)),
        ),
        "tty document sink (opaque result)",
    );
    assert_eq!(
        render_explain_final_sink(
            None,
            &FunctionReturn::Single(ResolvedType::Value(STD_IO_BYTE_STREAM_TYPE_ID)),
        ),
        "tty byte-stream sink (opaque result)",
    );
    assert_eq!(
        render_explain_final_sink(
            None,
            &FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Integer)),
        ),
        "none (canonical result)",
    );
}

#[test]
fn explain_renders_resolution_and_sealed_request_facts() {
    let mut output = Vec::new();
    render_explain(
        &mut output,
        &echo_definition(),
        &pipe_request(),
        "function-rev:test",
        "verified standard std:test",
    )
    .expect("explain renders");
    let plan = String::from_utf8(output).expect("plan is text");
    assert!(plan.contains("target: std.invoke.echo (function:"));
    assert!(plan.contains("revision: function-rev:test (pinned to verified standard std:test)"));
    assert!(plan.contains("domain: Server"));
    assert!(plan.contains("p_value (parameter:"));
    assert!(plan.contains(": INTEGER"));
    assert!(plan.contains("return: INTEGER"));
    assert!(plan.contains("request:"));
    assert!(plan.contains("target: std.invoke.echo"));
    assert!(plan.contains("caller: CliPipe"));
    assert!(
        plan.contains("offer: protocol 5, locale en-GB, timezone UTC, sinks 0, runtimes tty@0.1.0")
    );
    assert!(plan.contains("trace: Off"));
    assert!(plan.contains("output: none"));
}

#[test]
fn explain_renders_sink_and_runtime_plan_without_fabricating_candidates() {
    let mut input = pipe_request().into_input();
    input.client_offer = InvocationClientOffer::new(
        5,
        "en-GB",
        "UTC",
        client_sink_offers(RuntimeFamily::Tty).expect("client sink offers"),
        installed_tty_runtime_offers(),
        MAXIMUM_FRAME_SIZE,
        MAXIMUM_ARTIFACT_SIZE,
        None,
        None,
    )
    .expect("client offer");
    input.output_requirement = Some(
        InvocationOutputRequirement::new(
            Some("json".to_owned()),
            None,
            None,
            InvocationStreamingRequirement::Unspecified,
        )
        .expect("output requirement"),
    );
    let request = InvokeRequest::new(input).expect("request");

    let mut output = Vec::new();
    render_explain(
        &mut output,
        &echo_definition(),
        &request,
        "function-rev:test",
        "verified standard std:test",
    )
    .expect("explain renders");
    let plan = String::from_utf8(output).expect("plan is text");

    assert!(plan.contains("presentation:\n"));
    assert!(plan.contains("  candidates: unavailable before sealed dispatch"));
    assert!(plan.contains("  rejections: unavailable before sealed dispatch"));
    assert!(plan.contains("  selected presenter: unavailable before sealed dispatch"));
    assert!(plan.contains("  final sink: deferred until sealed presenter selection"));
    assert!(plan.contains("sinks:\n"));
    let document_id = STD_TERMINAL_DOCUMENT_TYPE_ID.canonical();
    let byte_stream_id = STD_IO_BYTE_STREAM_TYPE_ID.canonical();
    assert!(plan.contains(document_id.as_str()));
    assert!(plan.contains(byte_stream_id.as_str()));
    assert!(plan.contains("runtime:\n  selected: tty@"));
    assert!(plan.contains("consumes"));
}

#[test]
fn output_requirement_classifies_alias_media_type_and_type_name() {
    let alias = build_output_requirement("json").expect("alias requirement");
    assert_eq!(alias.alias(), Some("json"));
    assert_eq!(alias.media_type(), None);
    assert!(alias.type_selector().is_none());

    let media = build_output_requirement("application/json").expect("media requirement");
    assert_eq!(media.alias(), None);
    assert_eq!(media.media_type(), Some("application/json"));
    assert!(media.type_selector().is_none());

    let typed = build_output_requirement("std.ui.UI").expect("type requirement");
    assert_eq!(typed.alias(), None);
    assert_eq!(typed.media_type(), None);
    assert!(matches!(
        typed.type_selector(),
        Some(InvocationOutputTypeSelector::QualifiedName(name))
            if name.to_string() == "std.ui.UI"
    ));

    let error = build_output_requirement("").expect_err("empty output is a usage error");
    assert_eq!(error.kind(), InstalledInvokeErrorKind::Usage);
}

#[test]
fn error_kinds_map_to_the_spec_exit_table() {
    let cases = [
        (InstalledInvokeErrorKind::Usage, 2),
        (InstalledInvokeErrorKind::Authentication, 3),
        (InstalledInvokeErrorKind::Authorisation, 4),
        (InstalledInvokeErrorKind::Presentation, 5),
        (InstalledInvokeErrorKind::Cancelled, 6),
        (InstalledInvokeErrorKind::Internal, 7),
    ];
    for (kind, exit) in cases {
        assert_eq!(exit_code_for_test(kind), exit, "{kind:?}");
    }
}

fn exit_code_for_test(kind: InstalledInvokeErrorKind) -> u8 {
    match kind {
        InstalledInvokeErrorKind::Usage => 2,
        InstalledInvokeErrorKind::Authentication => 3,
        InstalledInvokeErrorKind::Authorisation => 4,
        InstalledInvokeErrorKind::Presentation => 5,
        InstalledInvokeErrorKind::Cancelled => 6,
        InstalledInvokeErrorKind::Internal => 7,
    }
}

#[test]
fn binding_rejects_positional_and_unknown_inputs_as_usage() {
    let definition = echo_definition();
    let positional = bind_cli_arguments(
        &definition,
        &[CliArgumentInput::Positional("41".to_owned())],
    )
    .expect_err("a positional argument is rejected");
    assert_eq!(
        positional.to_string(),
        "unexpected positional argument `41`"
    );

    let unknown = bind_cli_arguments(
        &definition,
        &[CliArgumentInput::Friendly {
            name: "p_other".to_owned(),
            value: "1".to_owned(),
        }],
    )
    .expect_err("an unknown parameter is rejected");
    assert_eq!(unknown.to_string(), "unknown parameter `p_other`");
}

#[test]
fn canonicalises_sealed_invoke_arguments_by_parameter_identity() {
    let lower = ParameterId::from_bytes([0x01; 16]);
    let higher = ParameterId::from_bytes([0x02; 16]);
    let arguments = vec![
        InvocationArgument::new(
            InvocationParameterSelector::parameter_id(higher),
            InvokeValue::new(RuntimeValue::Integer(2)).expect("integer value"),
        ),
        InvocationArgument::new(
            InvocationParameterSelector::parameter_id(lower),
            InvokeValue::new(RuntimeValue::Integer(1)).expect("integer value"),
        ),
    ];

    let canonical = canonicalise_invocation_arguments(arguments);

    assert!(matches!(
        canonical[0].selector(),
        InvocationParameterSelector::ParameterId(id) if *id == lower
    ));
    assert!(matches!(
        canonical[1].selector(),
        InvocationParameterSelector::ParameterId(id) if *id == higher
    ));
}
#[test]
fn binding_maps_verified_standard_integer_value_type_to_cli_scalar() {
    let standard = verify_standard_library_snapshot(
        retained_standard_library_snapshot().expect("retained standard snapshot"),
    )
    .expect("verified standard snapshot");
    let application = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0x94; 16]),
        Vec::new(),
        Vec::new(),
    )
    .expect("empty application catalogue");
    let definition = value_typed_definition(ResolvedType::value(orna_standard::INTEGER_TYPE_ID));
    let bound = bind_installed_cli_arguments(
        &application,
        Some(&standard),
        &definition,
        &[CliArgumentInput::Friendly {
            name: "value".to_owned(),
            value: "74".to_owned(),
        }],
    )
    .expect("the verified standard INTEGER value binds as a CLI scalar");
    assert_eq!(bound[0].value().value(), &RuntimeValue::Integer(74));
}

#[test]
fn binding_rejects_application_value_types_and_ambiguous_standard_ids() {
    let standard = verify_standard_library_snapshot(
        retained_standard_library_snapshot().expect("retained standard snapshot"),
    )
    .expect("verified standard snapshot");
    let application_value_id = TypeId::from_bytes([0xa7; 16]);
    let application = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([0x95; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x96; 16]),
            QualifiedSemanticName::new(["app"]).expect("schema name"),
        )],
        Vec::new(),
        vec![
            ValueTypeDefinition::primitive(
                application_value_id,
                QualifiedSemanticName::new(["app", "integer"]).expect("value name"),
                ValueTypeMutability::Immutable,
                ValueTypePersistence::Persistable,
                "orna.kernel.value.integer@1",
            ),
            ValueTypeDefinition::primitive(
                orna_standard::INTEGER_TYPE_ID,
                QualifiedSemanticName::new(["app", "shadow_integer"])
                    .expect("ambiguous value name"),
                ValueTypeMutability::Immutable,
                ValueTypePersistence::Persistable,
                "orna.kernel.value.integer@1",
            ),
        ],
        Vec::new(),
    )
    .expect("application value catalogue");

    assert_eq!(
        installed_cli_resolved_type(
            &application,
            Some(&standard),
            ResolvedType::value(application_value_id),
        ),
        ResolvedType::value(application_value_id),
    );
    assert_eq!(
        installed_cli_resolved_type(
            &application,
            Some(&standard),
            ResolvedType::value(orna_standard::INTEGER_TYPE_ID),
        ),
        ResolvedType::value(orna_standard::INTEGER_TYPE_ID),
    );
    for resolved_type in [
        ResolvedType::value(application_value_id),
        ResolvedType::value(orna_standard::INTEGER_TYPE_ID),
    ] {
        let error = bind_installed_cli_arguments(
            &application,
            Some(&standard),
            &value_typed_definition(resolved_type),
            &[CliArgumentInput::Friendly {
                name: "value".to_owned(),
                value: "74".to_owned(),
            }],
        )
        .expect_err("application and ambiguous value types stay unsupported");
        assert!(matches!(
            error,
            orna_core::invocation_binding::InvocationBindingError::ConversionFailed {
                detail: orna_core::invocation_binding::InvocationConversionError::UnsupportedType {
                    resolved_type: actual,
                },
                ..
            } if actual == resolved_type
        ));
    }

    // UUID has no ordinary RuntimeValue representation in ADR 0016 v1.
    assert_eq!(
        installed_cli_resolved_type(
            &application,
            Some(&standard),
            ResolvedType::value(orna_standard::UUID_TYPE_ID),
        ),
        ResolvedType::value(orna_standard::UUID_TYPE_ID),
    );
}

fn inspect_test_context() -> (
    ActiveDatabaseRevision,
    orna_core::value::OpaqueCodecRegistry,
) {
    let source_bundle = SourceBundleId::from_bytes([0x91; 16]);
    let source_revision = SourceRevisionId::from_bytes([0x92; 16]);
    let bundle_hash = source_bundle_digest(&[]).expect("source bundle digest");
    let source = StoredSourceRevision::new(
        source_bundle,
        source_revision,
        None,
        Vec::new(),
        bundle_hash,
        source_revision_record_digest(source_bundle, None, bundle_hash)
            .expect("source revision digest"),
    )
    .expect("stored source revision");
    let catalogue = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0x93; 16]),
        Vec::new(),
        Vec::new(),
    )
    .expect("empty catalogue");
    let standard = verify_standard_library_snapshot(
        retained_standard_library_snapshot().expect("retained standard snapshot"),
    )
    .expect("verified standard snapshot");
    let catalogue_hash =
        catalogue_digest(&catalogue, &[], &[], &[], &[]).expect("catalogue digest");
    let pair = RevisionPair::new(source.id(), catalogue.revision());
    let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            pair,
            source,
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(Vec::new(), Vec::new(), Vec::new(), Vec::new()),
        ),
        CatalogueHashContext::version_two(standard.clone()),
    )
    .expect("active revision");
    let registry = registered_opaque_codecs(&standard).expect("standard codecs");
    (active, registry)
}

#[test]
fn inspector_carrier_errors_map_to_stable_codes() {
    assert_eq!(
        map_inspect_carrier_error(InspectCarrierError::EnvelopeTooLarge {
            actual: 17,
            maximum: 16,
        }),
        "inspect.limit"
    );
    assert_eq!(
        map_inspect_carrier_error(InspectCarrierError::RowCountExceeded {
            actual: 2,
            maximum: 1,
        }),
        "inspect.limit"
    );
    assert_eq!(
        map_inspect_carrier_error(InspectCarrierError::RowTooLarge {
            actual: 17,
            maximum: 16,
        }),
        "inspect.limit"
    );
    assert_eq!(
        map_inspect_carrier_error(InspectCarrierError::InvalidMagic),
        "inspect.malformed_carrier"
    );
    assert_eq!(
        map_inspect_carrier_error(InspectCarrierError::UnknownProjectionTag(0xff)),
        "inspect.malformed_carrier"
    );
    assert_eq!(
        map_inspect_carrier_error(InspectCarrierError::InvalidTargetInvocation),
        "inspect.invalid_target"
    );
    assert_eq!(
        map_inspect_carrier_error(InspectCarrierError::TargetInvocationMismatch {
            expected: InvocationId::from_bytes([0x11; 16]),
            actual: InvocationId::from_bytes([0x22; 16]),
        }),
        "inspect.epoch_mismatch"
    );
    assert_eq!(
        map_inspect_opaque_value_error(OpaqueValueError::UnregisteredType {
            opaque_type: TypeId::from_bytes([0x33; 16]),
        }),
        "inspect.unknown_carrier"
    );
    assert_eq!(
        map_inspect_opaque_value_error(OpaqueValueError::InspectCarrierRevisionMismatch {
            opaque_type: TypeId::from_bytes([0x44; 16]),
        },),
        "inspect.epoch_mismatch"
    );
}

#[test]
fn inspector_snapshot_target_rejects_zero_object_bytes() {
    let target = RuntimeValue::Reference {
        target: SYS_INSPECT_INVOCATION_TYPE_ID,
        object: orna_core::ObjectId::from_bytes([0; 16]),
    };
    assert_eq!(
        inspect_snapshot_request_target(&target),
        Err("inspect.invalid_target".to_owned()),
    );
}

#[test]
fn inspector_snapshot_row_rejects_zero_value_batch_count() {
    let target = InvocationId::from_bytes([0x17; 16]);
    let epoch = InspectEpochId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7]);
    let root_target = FunctionId::from_bytes([0x18; 16]);
    let mut row = row(INSPECT_SNAPSHOT_ROW_TAG, 0);
    row.extend_from_slice(&epoch.to_bytes());
    row.extend_from_slice(&target.to_bytes());
    row.extend_from_slice(&[0x18; 16]);
    row.push(1);
    row.extend_from_slice(&0_u64.to_be_bytes());
    row.push(1);
    row.extend_from_slice(&0_u64.to_be_bytes());
    row.push(0);

    assert_eq!(row.len(), 76);
    let mut no_values = row.clone();
    no_values[66] = 0;
    no_values.truncate(68);
    assert_eq!(no_values.len(), 68);
    assert_eq!(
        decode_snapshot_row_payload(&no_values, 7),
        Ok((epoch, target, root_target))
    );
    assert_eq!(
        decode_snapshot_row_payload(&row, 7),
        Err("inspect.malformed_carrier".to_owned())
    );
    row[67..75].copy_from_slice(&1_u64.to_be_bytes());
    assert_eq!(
        decode_snapshot_row_payload(&row, 7),
        Ok((epoch, target, root_target))
    );
    row.push(0x19);
    assert_eq!(
        decode_snapshot_row_payload(&row, 7),
        Err("inspect.malformed_carrier".to_owned())
    );
}

#[test]
fn inspector_snapshot_row_rejects_forged_root_provenance() {
    let target = InvocationId::from_bytes([0x17; 16]);
    let epoch = InspectEpochId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7]);
    let expected_root = FunctionId::from_bytes([0x18; 16]);
    let forged_root = FunctionId::from_bytes([0x19; 16]);
    let mut row = row(INSPECT_SNAPSHOT_ROW_TAG, 0);
    row.extend_from_slice(&epoch.to_bytes());
    row.extend_from_slice(&target.to_bytes());
    row.extend_from_slice(&expected_root.to_bytes());
    row.push(1);
    row.extend_from_slice(&0_u64.to_be_bytes());
    row.push(0);
    row.push(0);

    let (_, _, decoded_root) = decode_snapshot_row_payload(&row, 7).expect("valid snapshot row");
    assert_eq!(
        require_inspect_root_provenance(expected_root, decoded_root),
        Ok(())
    );

    row[41..57].copy_from_slice(&forged_root.to_bytes());
    let (_, _, decoded_root) =
        decode_snapshot_row_payload(&row, 7).expect("forged root remains well-formed");
    assert_eq!(
        require_inspect_root_provenance(expected_root, decoded_root),
        Err("inspect.epoch_mismatch".to_owned())
    );
}

#[test]
fn inspector_enriched_row_rejects_forged_root_provenance() {
    let (active, registry) = inspect_test_context();
    let epoch = InspectEpochId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7]);
    let target = InvocationId::from_bytes([0x10; 16]);
    let expected_root = FunctionId::from_bytes([0x11; 16]);
    let forged_root = FunctionId::from_bytes([0x12; 16]);
    let mut payload = row(InspectCarrierKind::SecurityDecisions.tag(), 0);
    id(&mut payload, &epoch.to_bytes());
    id(&mut payload, &target.to_bytes());
    id(&mut payload, &expected_root.to_bytes());
    id(&mut payload, &active.pair().source().to_bytes());
    id(&mut payload, &active.pair().catalogue().to_bytes());
    payload.push(1);
    payload.push(0);
    payload.extend_from_slice(&[4, 1, 0, 2]);

    let encoded = encode_inspect_row(&active, &registry, payload.clone())
        .expect("canonical enriched Inspector row");
    let (_, _, decoded_root) = decode_enriched_inspect_row_target(
        &active,
        &registry,
        &encoded,
        InspectCarrierKind::SecurityDecisions,
        7,
    )
    .expect("valid enriched Inspector row");
    assert_eq!(
        require_inspect_root_provenance(expected_root, decoded_root),
        Ok(())
    );

    let mut forged_payload = payload;
    forged_payload[41..57].copy_from_slice(&forged_root.to_bytes());
    let forged_encoded = encode_inspect_row(&active, &registry, forged_payload)
        .expect("forged root remains well-formed");
    let (_, _, decoded_root) = decode_enriched_inspect_row_target(
        &active,
        &registry,
        &forged_encoded,
        InspectCarrierKind::SecurityDecisions,
        7,
    )
    .expect("forged root remains structurally valid");
    assert_eq!(
        require_inspect_root_provenance(expected_root, decoded_root),
        Err("inspect.epoch_mismatch".to_owned())
    );
}

#[test]
fn inspector_enriched_row_rejects_zero_target() {
    let (active, registry) = inspect_test_context();
    let epoch = InspectEpochId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7]);
    let mut payload = row(InspectCarrierKind::SecurityDecisions.tag(), 0);
    id(&mut payload, &epoch.to_bytes());
    id(&mut payload, &[0; 16]);
    id(&mut payload, &[0x11; 16]);
    id(&mut payload, &active.pair().source().to_bytes());
    id(&mut payload, &active.pair().catalogue().to_bytes());
    payload.push(1);
    payload.push(0);
    payload.extend_from_slice(&[4, 1, 0, 2]);

    let encoded =
        encode_inspect_row(&active, &registry, payload).expect("canonical enriched Inspector row");
    assert_eq!(
        decode_enriched_inspect_row_target(
            &active,
            &registry,
            &encoded,
            InspectCarrierKind::SecurityDecisions,
            7,
        ),
        Err("inspect.invalid_target".to_owned())
    );
}

#[test]
fn inspector_projection_requires_target_provenance() {
    let target = InvocationId::from_bytes([0x11; 16]);
    assert_eq!(
        require_inspect_target_provenance(None, target),
        Err("inspect.malformed_carrier".to_owned()),
    );
    assert_eq!(
        require_inspect_target_provenance(Some(InvocationId::from_bytes([0x22; 16])), target,),
        Err("inspect.epoch_mismatch".to_owned()),
    );
    assert_eq!(
        require_inspect_target_provenance(Some(target), target),
        Ok(())
    );
}

#[tokio::test]
async fn inspector_render_recursion_checks_root_parent_and_non_recursive_targets() {
    let root = InvocationId::from_bytes([0x31; 16]);
    let parent = InvocationId::from_bytes([0x32; 16]);
    let descendant = InvocationId::from_bytes([0x33; 16]);
    let mut checked = Vec::new();

    let result = reject_recursive_inspect_target(root, root, parent, |observer, target| {
        checked.push(observer);
        async move { Ok(observer == target) }
    })
    .await;
    assert_eq!(result, Err("inspect.recursion".to_owned()));
    assert_eq!(checked, vec![root]);

    checked.clear();
    let result = reject_recursive_inspect_target(descendant, root, parent, |observer, target| {
        checked.push(observer);
        async move { Ok(observer == parent && target == descendant) }
    })
    .await;
    assert_eq!(result, Err("inspect.recursion".to_owned()));
    assert_eq!(checked, vec![root, parent]);

    checked.clear();
    let result = reject_recursive_inspect_target(
        InvocationId::from_bytes([0x34; 16]),
        root,
        root,
        |observer, _target| {
            checked.push(observer);
            async { Ok(false) }
        },
    )
    .await;
    assert_eq!(result, Ok(()));
    assert_eq!(checked, vec![root]);
}

#[tokio::test]
async fn inspector_denied_recursive_target_does_not_query_lineage() {
    let target = InvocationId::from_bytes([0x35; 16]);
    let observer = InvocationId::from_bytes([0x36; 16]);
    let observer_lineage = [observer];
    let mut checked = Vec::new();

    let result: Result<(), String> = authorize_inspect_target_before_recursion(
        || async { Err("inspect.denied".to_owned()) },
        target,
        &observer_lineage,
        |ancestor, candidate| {
            checked.push((ancestor, candidate));
            async move { Ok(true) }
        },
    )
    .await;

    assert_eq!(result, Err("inspect.denied".to_owned()));
    assert!(
        checked.is_empty(),
        "denied targets must not be classified for recursion"
    );
}

#[test]
fn inspector_projection_requires_matching_observer_context() {
    let root = InvocationId::from_bytes([0x61; 16]);
    let parent = InvocationId::from_bytes([0x62; 16]);
    let other = InvocationId::from_bytes([0x63; 16]);
    let context = InspectObserverContext::new(root, parent).expect("observer context");

    assert_eq!(
        require_inspect_observer_context(Some(context), root, parent),
        Ok(())
    );
    assert_eq!(
        require_inspect_observer_context(Some(context), other, parent),
        Err("inspect.epoch_mismatch".to_owned())
    );
    assert_eq!(
        require_inspect_observer_context(None, root, parent),
        Err("inspect.epoch_mismatch".to_owned())
    );
    assert_eq!(
        require_inspect_observer_context(Some(context), InvocationId::from_bytes([0; 16]), parent,),
        Err("inspect.epoch_mismatch".to_owned())
    );
}

#[test]
fn inspector_rejects_forged_current_observer_root() {
    let root = InvocationId::from_bytes([0x71; 16]);
    let other = InvocationId::from_bytes([0x72; 16]);

    assert_eq!(
        require_current_observer_invocation(Some(root), root),
        Ok(root)
    );
    assert_eq!(
        require_current_observer_invocation(Some(root), other),
        Err("inspect.epoch_mismatch".to_owned())
    );
    assert_eq!(
        require_current_observer_invocation(None, root),
        Err("inspect.epoch_mismatch".to_owned())
    );
}

#[test]
fn inspector_projection_binding_rejects_target_epoch_and_revision_mismatches() {
    let target = InvocationId::from_bytes([0x11; 16]);
    let other_target = InvocationId::from_bytes([0x22; 16]);
    let mut epoch_bytes = [0; 16];
    epoch_bytes[15] = 0x33;
    let epoch = InspectEpochId::from_bytes(epoch_bytes);
    let pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0x44; 16]),
        CatalogueRevisionId::from_bytes([0x55; 16]),
    );
    let envelope = InspectCarrierEnvelope::new_with_target(
        InspectCarrierKind::Snapshot,
        target,
        InspectCarrierProvenance::trusted_for_target(0x33, target, pair.source(), pair.catalogue()),
        Vec::new(),
    )
    .expect("snapshot envelope");
    assert_eq!(
        validate_inspect_projection_binding(Some(target), &envelope, epoch, target, pair,),
        Ok(())
    );
    assert_eq!(
        validate_inspect_projection_binding(Some(other_target), &envelope, epoch, target, pair,),
        Err("inspect.epoch_mismatch".to_owned()),
    );
    let mut wrong_epoch_bytes = [0; 16];
    wrong_epoch_bytes[15] = 0x34;
    let wrong_epoch = InspectEpochId::from_bytes(wrong_epoch_bytes);
    assert_eq!(
        validate_inspect_projection_binding(Some(target), &envelope, wrong_epoch, target, pair,),
        Err("inspect.epoch_mismatch".to_owned()),
    );
    let wrong_pair = RevisionPair::new(SourceRevisionId::from_bytes([0x66; 16]), pair.catalogue());
    assert_eq!(
        validate_inspect_projection_binding(Some(target), &envelope, epoch, target, wrong_pair,),
        Err("inspect.epoch_mismatch".to_owned()),
    );
}

#[test]
fn inspector_calls_schema_requires_values_classifier() {
    let invocation = InvocationId::from_bytes([0x31; 16]);
    let schema = InvokeValue::new(RuntimeValue::Boolean(true)).expect("schema value");
    let call = CallRow::new(invocation, Some(schema), 1, 42).expect("call row");

    let redacted = encode_calls(std::slice::from_ref(&call), false).expect("redacted calls");
    let visible = encode_calls(std::slice::from_ref(&call), true).expect("visible calls");

    assert_eq!(redacted[0][25], 0);
    assert_eq!(visible[0][25], 1);
}

#[test]
fn inspect_render_signature_covers_all_projection_carriers() {
    assert_eq!(INSPECT_RENDER_CONTRACT, "std.inspect.render@1");
    assert_eq!(INSPECT_RENDER_CARRIER_SIGNATURE.len(), 9);
    for (tag, expected) in [
        (1, InspectCarrierKind::Snapshot),
        (2, InspectCarrierKind::InvocationNodes),
        (3, InspectCarrierKind::Calls),
        (4, InspectCarrierKind::Resources),
        (5, InspectCarrierKind::StateCells),
        (6, InspectCarrierKind::UiNodes),
        (7, InspectCarrierKind::PresentationCandidates),
        (8, InspectCarrierKind::RuntimeBindings),
        (9, InspectCarrierKind::SecurityDecisions),
    ] {
        assert_eq!(InspectCarrierKind::from_tag(tag), Some(expected));
    }
    assert_eq!(
        inspect_classification_tag(
            InspectCarrierKind::RuntimeBindings,
            InspectPrivilege::OwnInvocation
        ),
        0,
    );
    assert_eq!(
        inspect_classification_tag(
            InspectCarrierKind::SecurityDecisions,
            InspectPrivilege::OwnInvocation
        ),
        0,
    );
}

#[test]
fn inspect_rows_use_canonical_orv5_and_preserve_identity_payload() {
    let (active, registry) = inspect_test_context();
    let identity = vec![1, 0, 0, 0, 0, 0, 0, 0, 7, 0xaa, 0xbb];
    let encoded =
        encode_inspect_row(&active, &registry, identity.clone()).expect("Inspector row encodes");
    orna_core::inspect_carrier::validate_inspect_rows(std::slice::from_ref(&encoded))
        .expect("Inspector row is canonical ORV5");
    let decoded =
        decode_constructed_value(&active, &registry, &encoded).expect("Inspector row decodes");
    let RuntimeValue::Constructed(constructed) = decoded else {
        panic!("Inspector row must use the constructed representation");
    };
    let ConstructedValueKind::List(values) = constructed.kind() else {
        panic!("Inspector row must use a deterministic list representation");
    };
    let [RuntimeValue::Bytes(payload)] = values else {
        panic!("Inspector row must carry exactly one identity payload");
    };
    assert_eq!(payload, &identity);
}

#[test]
fn unarmed_security_and_runtime_carriers_redact_classified_bytes() {
    let (active, registry) = inspect_test_context();
    let target = InvocationId::from_bytes([0x32; 16]);
    let principal = PrincipalId::from_bytes([0xa1; 16]);
    let audit_reference = SecurityAuditEventId::from_bytes([0xa3; 16]);
    let denial_reason = "denial-reason-secret";
    let security = SecurityDecisionRow::new(
        InspectSecurityDecisionKind::Inspect,
        InspectSecurityDecisionOutcome::Denied,
        vec![principal],
        Some(FunctionId::from_bytes([0xa4; 16])),
        Some(denial_reason.to_owned()),
        vec![audit_reference],
    )
    .expect("security fixture must validate");
    let runtime = RuntimeBindingRow::new(
        "runtime-secret".to_owned(),
        "platform-secret".to_owned(),
        Vec::new(),
        vec![(
            "runtime-contract-secret".to_owned(),
            "9".to_owned(),
            vec!["platform-detail-secret".to_owned()],
        )],
        true,
        1,
    )
    .expect("runtime fixture must validate");
    let ui = UiNodeRow::new(
        FunctionId::from_bytes([0xa5; 16]),
        "call-site-secret".to_owned(),
        "ui-runtime-contract-secret".to_owned(),
    )
    .expect("UI fixture must validate");
    let selected_sink = TypeDescriptor::map(
        TypeDescriptor::list(TypeDescriptor::named(TypeId::from_bytes([0xa6; 16])))
            .expect("selected sink list must validate"),
        TypeDescriptor::option(TypeDescriptor::reference(TypeId::from_bytes([0xa7; 16])))
            .expect("selected sink option must validate"),
    )
    .expect("selected sink map must validate");
    let presentation = PresentationCandidateRow::new(
        "presenter-secret".to_owned(),
        true,
        "platform-reason-secret".to_owned(),
        Some(selected_sink),
        Some("presentation-runtime-secret".to_owned()),
    )
    .expect("presentation fixture must validate");
    let epoch = InspectSnapshotEpoch::new(
        InspectEpochId::from_bytes([0x31; 16]),
        target,
        active.pair().source(),
        active.pair().catalogue(),
        PrincipalId::from_bytes([0x33; 16]),
        std::time::SystemTime::UNIX_EPOCH,
        FunctionId::from_bytes([0x34; 16]),
        InspectOutcomeKind::Denied,
        InspectSnapshotSummary::new(1, InspectResultSummary::NoValues, None)
            .expect("summary must validate"),
        &InspectSnapshotOptions::structural(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        vec![security.clone()],
    )
    .expect("epoch must validate");

    let unarmed = [InspectPrivilege::OwnInvocation];
    let security_payload = make_inspect_carrier(
        &active,
        &registry,
        InspectCarrierKind::SecurityDecisions,
        &epoch,
        target,
        encode_security_decisions(std::slice::from_ref(&security), false)
            .expect("unarmed security rows encode"),
        0,
    )
    .expect("unarmed security carrier encodes");
    let runtime_payload = make_inspect_carrier(
        &active,
        &registry,
        InspectCarrierKind::RuntimeBindings,
        &epoch,
        target,
        encode_runtime_bindings(std::slice::from_ref(&runtime), false)
            .expect("unarmed runtime rows encode"),
        0,
    )
    .expect("unarmed runtime carrier encodes");
    let ui_payload = make_inspect_carrier(
        &active,
        &registry,
        InspectCarrierKind::UiNodes,
        &epoch,
        target,
        encode_ui_nodes(std::slice::from_ref(&ui), false, false).expect("unarmed UI rows encode"),
        0,
    )
    .expect("unarmed UI carrier encodes");
    let unarmed_presentation_rows =
        encode_presentation_candidates(std::slice::from_ref(&presentation), false)
            .expect("unarmed presentation rows encode");
    let presentation_payload = make_inspect_carrier(
        &active,
        &registry,
        InspectCarrierKind::PresentationCandidates,
        &epoch,
        target,
        unarmed_presentation_rows.clone(),
        0,
    )
    .expect("unarmed presentation carrier encodes");
    for (payload, kind) in [
        (&security_payload, InspectCarrierKind::SecurityDecisions),
        (&runtime_payload, InspectCarrierKind::RuntimeBindings),
        (&ui_payload, InspectCarrierKind::UiNodes),
        (
            &presentation_payload,
            InspectCarrierKind::PresentationCandidates,
        ),
    ] {
        let carrier = InspectCarrierEnvelope::decode(payload).expect("carrier decodes");
        assert_eq!(carrier.carrier_kind(), kind);
        orna_core::inspect_carrier::validate_inspect_rows(carrier.rows())
            .expect("carrier rows remain valid");
        assert_eq!(carrier.rows().len(), 1);
    }
    let contains =
        |payload: &[u8], bytes: &[u8]| payload.windows(bytes.len()).any(|window| window == bytes);
    let contains_row = |rows: &[Vec<u8>], bytes: &[u8]| rows.iter().any(|row| contains(row, bytes));
    assert!(!contains(&security_payload, &principal.to_bytes()));
    assert!(!contains(&security_payload, denial_reason.as_bytes()));
    assert!(!contains(&security_payload, &audit_reference.to_bytes()));
    assert!(!contains(&security_payload, &[0x33; 16]));
    let mut expected_unarmed_presentation_row = row(7, 0);
    expected_unarmed_presentation_row.extend_from_slice(&u32::MAX.to_be_bytes());
    expected_unarmed_presentation_row.push(1);
    expected_unarmed_presentation_row.extend_from_slice(&u32::MAX.to_be_bytes());
    expected_unarmed_presentation_row.push(INSPECT_REDACTED_FIELD_TAG);
    expected_unarmed_presentation_row.push(INSPECT_REDACTED_FIELD_TAG);
    assert_eq!(
        unarmed_presentation_rows,
        vec![expected_unarmed_presentation_row],
        "denied selected sinks encode only redaction markers",
    );
    let mut selected_descriptor_bytes = vec![4, 2, 0];
    selected_descriptor_bytes.extend_from_slice(&[0xa6; 16]);
    selected_descriptor_bytes.extend_from_slice(&[5, 1]);
    selected_descriptor_bytes.extend_from_slice(&[0xa7; 16]);
    assert!(!contains_row(
        &unarmed_presentation_rows,
        &selected_descriptor_bytes,
    ));
    for secret in [
        b"runtime-secret".as_slice(),
        b"platform-secret".as_slice(),
        b"runtime-contract-secret".as_slice(),
        b"platform-detail-secret".as_slice(),
    ] {
        assert!(!contains(&runtime_payload, secret));
    }
    for (payload, secret) in [
        (&ui_payload, b"call-site-secret".as_slice()),
        (&ui_payload, b"ui-runtime-contract-secret".as_slice()),
        (&presentation_payload, b"presenter-secret".as_slice()),
        (&presentation_payload, b"platform-reason-secret".as_slice()),
        (
            &presentation_payload,
            b"presentation-runtime-secret".as_slice(),
        ),
    ] {
        assert!(!contains(payload, secret));
    }

    assert!(contains_row(
        &encode_security_decisions(std::slice::from_ref(&security), true)
            .expect("armed security rows encode"),
        denial_reason.as_bytes(),
    ));
    assert!(contains_row(
        &encode_runtime_bindings(std::slice::from_ref(&runtime), true)
            .expect("armed runtime rows encode"),
        b"runtime-secret",
    ));
    assert!(contains_row(
        &encode_ui_nodes(std::slice::from_ref(&ui), true, true).expect("armed UI rows encode"),
        b"ui-runtime-contract-secret",
    ));
    let armed_presentation_rows =
        encode_presentation_candidates(std::slice::from_ref(&presentation), true)
            .expect("armed presentation rows encode");
    assert!(contains_row(
        &armed_presentation_rows,
        b"presentation-runtime-secret",
    ));
    let armed_presentation_payload = make_inspect_carrier(
        &active,
        &registry,
        InspectCarrierKind::PresentationCandidates,
        &epoch,
        target,
        armed_presentation_rows.clone(),
        0,
    )
    .expect("armed presentation carrier encodes");
    let armed_carrier =
        InspectCarrierEnvelope::decode(&armed_presentation_payload).expect("carrier decodes");
    assert_eq!(
        armed_carrier.carrier_kind(),
        InspectCarrierKind::PresentationCandidates
    );
    orna_core::inspect_carrier::validate_inspect_rows(armed_carrier.rows())
        .expect("armed carrier rows remain valid");
    assert!(contains(
        &armed_presentation_payload,
        &selected_descriptor_bytes
    ));
    let armed_presentation_row = &armed_presentation_rows[0];
    let descriptor_offset = armed_presentation_row
        .windows(selected_descriptor_bytes.len())
        .position(|window| window == selected_descriptor_bytes.as_slice())
        .expect("granted carrier preserves selected sink descriptor");
    assert_eq!(armed_presentation_row[descriptor_offset - 1], 1);
    assert!(!inspect_classifier_granted(
        &unarmed,
        InspectPrivilege::SecurityDetails
    ));
    assert!(!inspect_classifier_granted(
        &unarmed,
        InspectPrivilege::RuntimeInternals
    ));
}

fn transport_test_context() -> (
    ActiveDatabaseRevision,
    orna_core::value::OpaqueCodecRegistry,
) {
    let source_bundle = SourceBundleId::from_bytes([0x81; 16]);
    let source_revision = SourceRevisionId::from_bytes([0x82; 16]);
    let bundle_hash = source_bundle_digest(&[]).expect("source bundle digest");
    let source = StoredSourceRevision::new(
        source_bundle,
        source_revision,
        None,
        Vec::new(),
        bundle_hash,
        source_revision_record_digest(source_bundle, None, bundle_hash)
            .expect("source revision digest"),
    )
    .expect("stored source revision");
    let catalogue = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0x83; 16]),
        Vec::new(),
        Vec::new(),
    )
    .expect("empty catalogue");
    let standard = orna_standard::verify_standard_library_v6_snapshot(
        orna_standard::retained_standard_library_v6_snapshot().expect("retained standard snapshot"),
    )
    .expect("verified standard snapshot");
    let catalogue_hash =
        catalogue_digest(&catalogue, &[], &[], &[], &[]).expect("catalogue digest");
    let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            RevisionPair::new(source.id(), catalogue.revision()),
            source,
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(Vec::new(), Vec::new(), Vec::new(), Vec::new()),
        ),
        CatalogueHashContext::version_two(standard.clone()),
    )
    .expect("active revision");
    let registry = registered_opaque_codecs(&standard).expect("standard codecs");
    (active, registry)
}

fn transport_test_request(revision: RevisionPair, stream_id: u64) -> ResourceRequest {
    ResourceRequest {
        stream_id,
        request_id: InvocationId::from_bytes([stream_id as u8; 16]),
        parent_invocation_id: InvocationId::from_bytes([0x21; 16]),
        call_site_id: orna_core::CallSiteId::from_bytes([0x22; 16]),
        state_profile: String::new(),
        function_instance_key: String::new(),
        target_function_id: FunctionId::from_bytes([0x23; 16]),
        target_revision: revision,
        generation: stream_id,
        resource_kind: ProtocolResourceKind::Single,
        arguments: Vec::new(),
        item_window: 1,
        byte_window: MAX_RESOURCE_WINDOW,
    }
}

#[test]
fn authenticated_resource_credit_respects_small_request_windows() {
    let (active, _) = transport_test_context();
    let mut request = transport_test_request(active.pair(), 1);
    request.item_window = 2;
    request.byte_window = 3;

    let credit = initial_authenticated_resource_credit(&request)
        .expect("small request windows produce bounded credit");
    assert_eq!(credit.item_count, request.item_window);
    assert_eq!(credit.byte_count, request.byte_window);
    assert_ne!(credit.item_count, MAX_RESOURCE_WINDOW);
    assert_ne!(credit.byte_count, MAX_RESOURCE_WINDOW);
}

#[test]
fn authenticated_resource_credit_accepts_sufficient_request_windows() {
    let (active, _) = transport_test_context();
    let mut request = transport_test_request(active.pair(), 1);
    request.item_window = MAX_RESOURCE_WINDOW;
    request.byte_window = MAX_RESOURCE_WINDOW;

    let credit = initial_authenticated_resource_credit(&request)
        .expect("sufficient request windows produce bounded credit");
    assert_eq!(credit.item_count, MAX_RESOURCE_WINDOW);
    assert_eq!(credit.byte_count, MAX_RESOURCE_WINDOW);
}

#[test]
fn authenticated_resource_credit_rejects_invalid_request_windows() {
    let (active, _) = transport_test_context();
    let mut request = transport_test_request(active.pair(), 1);
    request.item_window = 0;
    assert!(matches!(
        initial_authenticated_resource_credit(&request),
        Err(ResourceTransportFailure::Shape)
    ));

    request.item_window = 1;
    request.byte_window = MAX_RESOURCE_WINDOW + 1;
    assert!(matches!(
        initial_authenticated_resource_credit(&request),
        Err(ResourceTransportFailure::Shape)
    ));
}

#[test]
fn authenticated_resource_values_accept_canonical_values_with_credit() {
    let (active, registry) = transport_test_context();
    let request = transport_test_request(active.pair(), 1);
    let value = RuntimeValue::Integer(7);
    let byte_count = encode_constructed_value(&active, &registry, &value)
        .expect("canonical value encoding")
        .len() as u64;
    let total = validate_authenticated_resource_values(
        &active,
        &registry,
        &request,
        ResolvedType::Scalar(StandardScalar::Integer),
        ProtocolResourceKind::Stream,
        0,
        false,
        0,
        0,
        ResourceCredit::new(1, byte_count).expect("credit"),
        0,
        1,
        byte_count,
        std::slice::from_ref(&value),
    )
    .expect("canonical values fit offered credit");
    assert_eq!(total, 1);
}

#[tokio::test]
async fn broker_rejects_mismatched_resource_value_before_stream_state_mutation() {
    let (active, registry) = transport_test_context();
    let mut request = transport_test_request(active.pair(), 1);
    request.resource_kind = ProtocolResourceKind::Stream;
    let nested_invocation_id = InvocationId::from_bytes([0x40; 16]);
    let mut protocol = ResourceProtocolConnection::new();
    protocol
        .open(request.clone())
        .expect("resource request opens");
    protocol
        .apply_constructed(
            &active,
            &registry,
            ResourceServerFrame::Accepted(ResourceAccepted {
                stream_id: request.stream_id,
                request_id: request.request_id,
                nested_invocation_id,
                target_revision: request.target_revision,
                resource_kind: request.resource_kind,
            }),
        )
        .expect("resource acceptance applies");
    let (completion, mut completions) = mpsc::channel(2);
    let mut state = BrokerResourceState {
        request: request.clone(),
        expected_type: ResolvedType::Scalar(StandardScalar::Integer),
        resource_kind: ProtocolResourceKind::Stream,
        protocol,
        completion,
        accepted: true,
        accepted_nested_invocation_id: Some(nested_invocation_id),
        scalar_value: None,
        cancellation_requested: false,
        stream_values_seen: false,
        terminal_provenance: ResourceTerminalProvenance::Uncommitted,
        scalar_value_after_cancellation: false,
    };
    let protocol_before = state.protocol.clone();
    let credit_before = state
        .protocol
        .resource_credit(request.stream_id, request.request_id)
        .expect("accepted resource retains credit");
    let (_reader, mut writer) = tokio::io::duplex(512);
    let value = RuntimeValue::Text("wrong result type".to_owned());
    let byte_count = encode_constructed_value(&active, &registry, &value)
        .expect("encoded mismatched resource value")
        .len() as u32;

    let error = handle_shared_resource_frame(
        &mut state,
        ResourceServerFrame::Values(ResourceValues {
            stream_id: request.stream_id,
            request_id: request.request_id,
            target_revision: request.target_revision,
            batch_sequence: 0,
            item_count: 1,
            byte_count,
            values: vec![value],
        }),
        &mut writer,
        &active,
        &registry,
    )
    .await
    .expect_err("mismatched result type fails closed at the broker boundary");
    assert!(matches!(error, ResourceTransportFailure::Shape));
    assert_eq!(state.protocol, protocol_before);
    assert_eq!(state.protocol.live_resources(), 1);
    assert_eq!(
        state
            .protocol
            .resource_credit(request.stream_id, request.request_id)
            .expect("resource remains live after rejection"),
        credit_before
    );
    assert_eq!(
        state
            .protocol
            .resource_nested_invocation_id(request.stream_id, request.request_id)
            .expect("resource lineage remains inspectable"),
        Some(nested_invocation_id)
    );
    assert!(state.accepted);
    assert!(!state.stream_values_seen);
    assert!(state.scalar_value.is_none());
    assert_eq!(
        state.terminal_provenance,
        ResourceTerminalProvenance::Uncommitted
    );
    assert!(!state.scalar_value_after_cancellation);
    assert!(completions.try_recv().is_err());
}

#[test]
fn authenticated_resource_values_consume_item_window_across_batches() {
    let (active, registry) = transport_test_context();
    let request = transport_test_request(active.pair(), 1);
    let value = RuntimeValue::Integer(7);
    let byte_count = encode_constructed_value(&active, &registry, &value)
        .expect("canonical value encoding")
        .len() as u64;
    let offered_credit = ResourceCredit::new(1, byte_count * 2).expect("credit");
    let mut total_items = 0;
    let mut total_bytes = 0;
    let mut published_batches = Vec::new();

    let first_total = validate_authenticated_resource_values(
        &active,
        &registry,
        &request,
        ResolvedType::Scalar(StandardScalar::Integer),
        ProtocolResourceKind::Stream,
        0,
        false,
        total_items,
        total_bytes,
        offered_credit,
        0,
        1,
        byte_count,
        std::slice::from_ref(&value),
    )
    .expect("first batch fits the offered item window");
    total_items = first_total;
    total_bytes += byte_count;
    published_batches.push(value.clone());

    assert_eq!(
        authenticated_resource_producer_credit(
            ProtocolResourceKind::Stream,
            false,
            offered_credit.item_count - total_items,
            offered_credit.byte_count - total_bytes,
        ),
        Some(ResourceCredit {
            item_count: 0,
            byte_count,
        })
    );
    assert!(matches!(
        validate_authenticated_resource_values(
            &active,
            &registry,
            &request,
            ResolvedType::Scalar(StandardScalar::Integer),
            ProtocolResourceKind::Stream,
            1,
            false,
            total_items,
            total_bytes,
            authenticated_resource_producer_credit(
                ProtocolResourceKind::Stream,
                false,
                offered_credit.item_count - total_items,
                offered_credit.byte_count - total_bytes,
            )
            .expect("remaining credit probe"),
            1,
            1,
            byte_count,
            std::slice::from_ref(&value),
        ),
        Err(ResourceTransportFailure::Shape)
    ));
    assert_eq!(published_batches, vec![value]);
    assert_eq!((total_items, total_bytes), (1, byte_count));
}

#[test]
fn authenticated_resource_values_reject_item_credit_overrun() {
    let (active, registry) = transport_test_context();
    let request = transport_test_request(active.pair(), 1);
    let values = vec![RuntimeValue::Integer(7), RuntimeValue::Integer(8)];
    let byte_count = values
        .iter()
        .map(|value| {
            encode_constructed_value(&active, &registry, value)
                .expect("encoding")
                .len()
        })
        .sum::<usize>() as u64;
    assert!(matches!(
        validate_authenticated_resource_values(
            &active,
            &registry,
            &request,
            ResolvedType::Scalar(StandardScalar::Integer),
            ProtocolResourceKind::Stream,
            0,
            false,
            0,
            0,
            ResourceCredit::new(1, byte_count).expect("credit"),
            0,
            2,
            byte_count,
            &values,
        ),
        Err(ResourceTransportFailure::Shape)
    ));
}

#[test]
fn authenticated_resource_values_reject_byte_credit_overrun() {
    let (active, registry) = transport_test_context();
    let request = transport_test_request(active.pair(), 1);
    let value = RuntimeValue::Integer(7);
    let byte_count = encode_constructed_value(&active, &registry, &value)
        .expect("canonical value encoding")
        .len() as u64;
    assert!(matches!(
        validate_authenticated_resource_values(
            &active,
            &registry,
            &request,
            ResolvedType::Scalar(StandardScalar::Integer),
            ProtocolResourceKind::Stream,
            0,
            false,
            0,
            0,
            ResourceCredit::new(1, byte_count - 1).expect("credit"),
            0,
            1,
            byte_count,
            std::slice::from_ref(&value),
        ),
        Err(ResourceTransportFailure::Shape)
    ));
}

#[test]
fn authenticated_resource_values_reject_forged_byte_count() {
    let (active, registry) = transport_test_context();
    let request = transport_test_request(active.pair(), 1);
    let value = RuntimeValue::Integer(7);
    let byte_count = encode_constructed_value(&active, &registry, &value)
        .expect("canonical value encoding")
        .len() as u64;
    assert!(matches!(
        validate_authenticated_resource_values(
            &active,
            &registry,
            &request,
            ResolvedType::Scalar(StandardScalar::Integer),
            ProtocolResourceKind::Stream,
            0,
            false,
            0,
            0,
            ResourceCredit::new(1, byte_count + 1).expect("credit"),
            0,
            1,
            byte_count + 1,
            std::slice::from_ref(&value),
        ),
        Err(ResourceTransportFailure::Shape)
    ));
}

#[test]
fn authenticated_resource_values_enforce_total_item_boundary() {
    let (active, registry) = transport_test_context();
    let request = transport_test_request(active.pair(), 1);
    let value = RuntimeValue::Integer(7);
    let byte_count = encode_constructed_value(&active, &registry, &value)
        .expect("canonical value encoding")
        .len() as u64;
    let credit = ResourceCredit::new(1, byte_count).expect("credit");
    assert_eq!(
        validate_authenticated_resource_values(
            &active,
            &registry,
            &request,
            ResolvedType::Scalar(StandardScalar::Integer),
            ProtocolResourceKind::Stream,
            0,
            false,
            MAX_RESOURCE_TOTAL_ITEMS - 1,
            0,
            credit,
            0,
            1,
            byte_count,
            std::slice::from_ref(&value),
        )
        .expect("maximum total is accepted"),
        MAX_RESOURCE_TOTAL_ITEMS
    );
    assert!(matches!(
        validate_authenticated_resource_values(
            &active,
            &registry,
            &request,
            ResolvedType::Scalar(StandardScalar::Integer),
            ProtocolResourceKind::Stream,
            1,
            false,
            MAX_RESOURCE_TOTAL_ITEMS,
            byte_count,
            credit,
            1,
            1,
            byte_count,
            std::slice::from_ref(&value),
        ),
        Err(ResourceTransportFailure::Shape)
    ));
}

fn installed_client_test_request(
    active: &ActiveDatabaseRevision,
    state: &mut ClientStateStore,
    invocation_seed: u8,
) -> ClientResourceRequest {
    let standard = active
        .catalogue_hash_context()
        .standard()
        .expect("transport fixture pins the verified standard snapshot");
    let target = ClientInvocationTarget::verified_standard(
        orna_standard::STD_INVOKE_ECHO_FUNCTION_ID,
        active.pair(),
        standard.revision(),
        orna_standard::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
    );
    let argument = FunctionArgument::new(
        orna_standard::STD_INVOKE_ECHO_PARAMETER_ID,
        RuntimeValue::Integer(7),
    )
    .expect("resource argument");
    let digest =
        ClientResourceKey::canonical_arguments_digest(active, std::slice::from_ref(&argument))
            .expect("resource argument digest");
    let key = ClientResourceKey::new(
        target,
        PrincipalId::from_bytes([0x91; 16]),
        digest,
        Sha256Digest::from_bytes([0x92; 32]),
    );
    state
        .get_or_create_resource(key, ResolvedType::Scalar(StandardScalar::Integer))
        .begin_request_with_context(
            active,
            ClientResourceInvocationContext::new(
                InvocationId::from_bytes([invocation_seed; 16]),
                CallSiteId::from_bytes([invocation_seed.wrapping_add(1); 16]),
                String::new(),
                String::new(),
            ),
            vec![argument],
        )
        .expect("client resource request")
}

fn read_resource_test_frame(stream: &mut StandardUnixStream) -> Vec<u8> {
    let mut encoded = vec![0_u8; RESOURCE_HEADER_LENGTH];
    stream
        .read_exact(&mut encoded)
        .expect("resource frame header");
    let payload_length =
        u32::from_be_bytes(encoded[17..21].try_into().expect("resource length")) as usize;
    encoded.resize(RESOURCE_HEADER_LENGTH + payload_length, 0);
    stream
        .read_exact(&mut encoded[RESOURCE_HEADER_LENGTH..])
        .expect("resource frame payload");
    encoded
}

fn serve_two_scalar_test_requests(
    mut stream: StandardUnixStream,
    active: ActiveDatabaseRevision,
    registry: orna_core::value::OpaqueCodecRegistry,
) -> Vec<ResourceRequest> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("peer read timeout");
    let mut hello = [0_u8; CONSTRUCTED_CLIENT_HELLO.len()];
    stream.read_exact(&mut hello).expect("constructed hello");
    assert_eq!(hello, CONSTRUCTED_CLIENT_HELLO);
    stream
        .write_all(&CONSTRUCTED_SERVER_ACK)
        .expect("constructed acknowledgement");

    let mut requests = Vec::new();
    for expected_stream_id in 1..=2 {
        let encoded = read_resource_test_frame(&mut stream);
        let ResourceClientFrame::Request(request) =
            decode_resource_client_frame(&active, &registry, &encoded)
                .expect("client resource request")
        else {
            panic!("the client sent a non-request resource frame");
        };
        assert_eq!(request.stream_id, expected_stream_id);
        let value = RuntimeValue::Integer(expected_stream_id as i32);
        let byte_count = encode_constructed_value(&active, &registry, &value)
            .expect("encoded resource value")
            .len() as u32;
        let frames = [
            ResourceServerFrame::Accepted(ResourceAccepted {
                stream_id: request.stream_id,
                request_id: request.request_id,
                nested_invocation_id: InvocationId::from_bytes(
                    [0x30 + expected_stream_id as u8; 16],
                ),
                target_revision: request.target_revision,
                resource_kind: ProtocolResourceKind::Single,
            }),
            ResourceServerFrame::Values(ResourceValues {
                stream_id: request.stream_id,
                request_id: request.request_id,
                target_revision: active.pair(),
                batch_sequence: 0,
                item_count: 1,
                byte_count,
                values: vec![value],
            }),
            ResourceServerFrame::Completed(ResourceCompleted {
                stream_id: request.stream_id,
                request_id: request.request_id,
                target_revision: active.pair(),
                final_batch_sequence: 0,
                total_items: 1,
            }),
        ];
        for frame in frames {
            let encoded = encode_resource_server_frame(&active, &registry, &frame)
                .expect("encoded resource response");
            stream.write_all(&encoded).expect("resource response");
        }
        requests.push(request);
    }
    requests
}

#[derive(Clone, Copy)]
enum SocketTerminal {
    Completed,
    Failed(CallFailure),
}

fn serve_one_terminal_test_request(
    mut stream: StandardUnixStream,
    active: ActiveDatabaseRevision,
    registry: orna_core::value::OpaqueCodecRegistry,
    expected_kind: ProtocolResourceKind,
    nested_invocation_id: InvocationId,
    terminal: SocketTerminal,
) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("peer read timeout");
    let mut hello = [0_u8; CONSTRUCTED_CLIENT_HELLO.len()];
    stream.read_exact(&mut hello).expect("constructed hello");
    assert_eq!(hello, CONSTRUCTED_CLIENT_HELLO);
    stream
        .write_all(&CONSTRUCTED_SERVER_ACK)
        .expect("constructed acknowledgement");
    let encoded = read_resource_test_frame(&mut stream);
    let ResourceClientFrame::Request(request) =
        decode_resource_client_frame(&active, &registry, &encoded)
            .expect("client resource request")
    else {
        panic!("the client sent a non-request resource frame");
    };
    assert_eq!(request.resource_kind, expected_kind);
    let accepted = ResourceServerFrame::Accepted(ResourceAccepted {
        stream_id: request.stream_id,
        request_id: request.request_id,
        nested_invocation_id,
        target_revision: request.target_revision,
        resource_kind: expected_kind,
    });
    let encoded = encode_resource_server_frame(&active, &registry, &accepted)
        .expect("encoded resource acceptance");
    stream.write_all(&encoded).expect("resource acceptance");
    let terminal = match terminal {
        SocketTerminal::Completed => ResourceServerFrame::Completed(ResourceCompleted {
            stream_id: request.stream_id,
            request_id: request.request_id,
            target_revision: active.pair(),
            final_batch_sequence: 0,
            total_items: 0,
        }),
        SocketTerminal::Failed(failure) => ResourceServerFrame::Failed(ResourceFailed {
            stream_id: request.stream_id,
            request_id: request.request_id,
            target_revision: active.pair(),
            failure,
        }),
    };
    let encoded = encode_resource_server_frame(&active, &registry, &terminal)
        .expect("encoded resource terminal");
    stream.write_all(&encoded).expect("resource terminal");
}

fn run_scalar_test_request(
    runtime: &tokio::runtime::Runtime,
    transport: &mut ResourceTransportSource,
    active: &ActiveDatabaseRevision,
    registry: &orna_core::value::OpaqueCodecRegistry,
    request: ResourceRequest,
) -> RuntimeValue {
    let (stream, handshake_complete, protocol) = transport
        .take_connection()
        .expect("persistent transport connection");
    let (completion_sender, _completion_receiver) = mpsc::channel(1);
    let (_control_sender, controls) = mpsc::unbounded_channel();
    let expected_nested_invocation_id =
        InvocationId::from_bytes([0x30 + request.stream_id as u8; 16]);
    let run = runtime
        .block_on(run_resource_transport(
            stream,
            handshake_complete,
            protocol,
            active.clone(),
            registry.clone(),
            request,
            ResolvedType::Scalar(StandardScalar::Integer),
            ProtocolResourceKind::Single,
            None,
            controls,
            &completion_sender,
        ))
        .unwrap_or_else(|_| panic!("resource transport run"));
    let stream = run.stream.into_std().expect("restored resource stream");
    transport.restore_connection(stream, true, run.protocol);
    match run.outcome {
        ResourceTransportOutcome::Ready {
            value,
            nested_invocation_id,
        } => {
            assert_eq!(nested_invocation_id, expected_nested_invocation_id);
            value
        }
        _ => panic!("unexpected non-ready scalar outcome"),
    }
}

#[test]
fn socket_transport_retains_nested_identity_for_stream_completion() {
    let (active, registry) = transport_test_context();
    let mut request = transport_test_request(active.pair(), 1);
    request.resource_kind = ProtocolResourceKind::Stream;
    let nested_invocation_id = InvocationId::from_bytes([0x51; 16]);
    let (peer, client) = StandardUnixStream::pair().expect("resource socket pair");
    let peer_thread = thread::spawn({
        let peer_active = active.clone();
        let peer_registry = registry.clone();
        move || {
            serve_one_terminal_test_request(
                peer,
                peer_active,
                peer_registry,
                ProtocolResourceKind::Stream,
                nested_invocation_id,
                SocketTerminal::Completed,
            );
        }
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let (completion_sender, _completion_receiver) = mpsc::channel(1);
    let (_control_sender, controls) = mpsc::unbounded_channel();
    let result = runtime.block_on(run_resource_transport(
        client,
        false,
        ResourceProtocolConnection::new(),
        active,
        registry,
        request,
        ResolvedType::Scalar(StandardScalar::Integer),
        ProtocolResourceKind::Stream,
        None,
        controls,
        &completion_sender,
    ));
    let run = result.expect("stream transport run");
    assert!(matches!(
        run.outcome,
        ResourceTransportOutcome::StreamCompleted {
            nested_invocation_id: actual,
        } if actual == nested_invocation_id
    ));
    peer_thread.join().expect("resource peer");
}

#[test]
fn installed_executor_drop_shuts_down_pending_raw_and_abandons_pending_broker() {
    let (active, _registry) = transport_test_context();
    let mut state = ClientStateStore::new();
    let raw_request = installed_client_test_request(&active, &mut state, 0xe1);
    let broker_request = installed_client_test_request(&active, &mut state, 0xe2);
    let (raw_control, mut raw_controls) = mpsc::unbounded_channel();
    let (worker_done_sender, worker_done_receiver) = std::sync::mpsc::sync_channel(0);
    let worker = thread::spawn(move || {
        let received_shutdown = matches!(
            raw_controls.blocking_recv(),
            Some(ResourceTransportControl::Shutdown)
        );
        worker_done_sender
            .send(received_shutdown)
            .expect("worker completion signal");
    });
    let (_raw_sender, raw_receiver) = mpsc::channel(1);
    let (_broker_sender, broker_receiver) = mpsc::channel(1);
    let (broker, mut commands) = SharedInvokeBroker::pending();
    let executor = InstalledClientResourceExecutor {
        active,
        inspect_kernel: None,
        inspect_session: None,
        current_invocation: None,
        next_stream_id: 1,
        broker: Some(broker.clone()),
        raw_resource_authorizer: None,
        transport: None,
        pending: Some(PendingResourceTransport {
            request: raw_request,
            stream_id: 1,
            receiver: raw_receiver,
            control: raw_control,
            transport_return: std::sync::Arc::new(std::sync::Mutex::new(None)),
            worker,
            cancel_requested: false,
        }),
        broker_pending: Some(PendingBrokerResource {
            request: broker_request.clone(),
            receiver: broker_receiver,
            control: broker,
            stream_id: 2,
            cancel_requested: false,
        }),
        detached: Vec::new(),
        detached_broker: None,
        cancellation: ResourceCancellation::new(),
    };

    drop(executor);

    assert!(
        worker_done_receiver
            .recv()
            .expect("raw worker completion signal"),
        "dropping the executor must send raw transport shutdown",
    );
    assert!(matches!(
        commands.try_recv(),
        Ok(BrokerCommand::CancelResource {
            stream_id: 2,
            request_id,
            reason: ResourceCancellationCode::RuntimeShutdown,
        }) if request_id == broker_request.request_id()
    ));
    assert!(matches!(
        commands.try_recv(),
        Ok(BrokerCommand::AbandonResource {
            stream_id: 2,
            request_id,
            reason: ResourceCancellationCode::RuntimeShutdown,
        }) if request_id == broker_request.request_id()
    ));
}

#[test]
fn installed_executor_abandon_closes_raw_controls_after_late_values() {
    let (active, registry) = transport_test_context();
    let standard = active
        .catalogue_hash_context()
        .standard()
        .expect("transport fixture pins the verified standard snapshot");
    let target = ClientInvocationTarget::verified_standard(
        orna_standard::STD_INVOKE_ECHO_FUNCTION_ID,
        active.pair(),
        standard.revision(),
        orna_standard::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
    );
    let argument = FunctionArgument::new(
        orna_standard::STD_INVOKE_ECHO_PARAMETER_ID,
        RuntimeValue::Integer(7),
    )
    .unwrap();
    let digest =
        ClientResourceKey::canonical_arguments_digest(&active, std::slice::from_ref(&argument))
            .unwrap();
    let key = ClientResourceKey::new(
        target,
        PrincipalId::from_bytes([0x91; 16]),
        digest,
        Sha256Digest::from_bytes([0x92; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource(key, ResolvedType::Scalar(StandardScalar::Integer))
        .begin_request_with_context(
            &active,
            ClientResourceInvocationContext::new(
                InvocationId::from_bytes([0x93; 16]),
                CallSiteId::from_bytes([0x94; 16]),
                String::new(),
                String::new(),
            ),
            vec![argument],
        )
        .unwrap();
    let (peer, client) = StandardUnixStream::pair().expect("resource socket pair");
    let (accepted_sender, accepted_receiver) = std::sync::mpsc::sync_channel(0);
    let (late_sender, late_receiver) = std::sync::mpsc::sync_channel(0);
    let (release_sender, release_receiver) = std::sync::mpsc::sync_channel(0);
    let peer_active = active.clone();
    let peer_registry = registry.clone();
    let request_id = request.request_id();
    let peer_thread = thread::spawn(move || {
        let mut peer = peer;
        peer.set_read_timeout(Some(Duration::from_secs(5)))
            .expect("peer read timeout");
        let mut hello = [0_u8; CONSTRUCTED_CLIENT_HELLO.len()];
        peer.read_exact(&mut hello).expect("constructed hello");
        assert_eq!(hello, CONSTRUCTED_CLIENT_HELLO);
        peer.write_all(&CONSTRUCTED_SERVER_ACK)
            .expect("constructed acknowledgement");
        let encoded = read_resource_test_frame(&mut peer);
        let ResourceClientFrame::Request(protocol_request) =
            decode_resource_client_frame(&peer_active, &peer_registry, &encoded)
                .expect("client resource request")
        else {
            panic!("the client sent a non-request resource frame");
        };
        assert_eq!(protocol_request.request_id, request_id);
        let accepted = ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id: protocol_request.stream_id,
            request_id,
            nested_invocation_id: InvocationId::from_bytes([0x95; 16]),
            target_revision: protocol_request.target_revision,
            resource_kind: protocol_request.resource_kind,
        });
        let encoded = encode_resource_server_frame(&peer_active, &peer_registry, &accepted)
            .expect("encoded resource acceptance");
        peer.write_all(&encoded).expect("resource acceptance");
        accepted_sender.send(()).expect("accepted signal");

        let encoded = read_resource_test_frame(&mut peer);
        let ResourceClientFrame::Cancel(cancel) =
            decode_resource_client_frame(&peer_active, &peer_registry, &encoded)
                .expect("client resource cancellation")
        else {
            panic!("the client sent a non-cancellation resource frame");
        };
        assert_eq!(cancel.request_id, request_id);
        let value = RuntimeValue::Integer(7);
        let late_values = ResourceServerFrame::Values(ResourceValues {
            stream_id: protocol_request.stream_id,
            request_id,
            target_revision: protocol_request.target_revision,
            batch_sequence: 0,
            item_count: 1,
            byte_count: encode_constructed_value(&peer_active, &peer_registry, &value)
                .expect("encoded late value")
                .len() as u32,
            values: vec![value],
        });
        let encoded = encode_resource_server_frame(&peer_active, &peer_registry, &late_values)
            .expect("encoded late values");
        let late_write = peer.write_all(&encoded);
        late_sender
            .send(late_write)
            .expect("late values write signal");
        release_receiver.recv().expect("release peer");
    });
    let mut executor = InstalledClientResourceExecutor {
        active: active.clone(),
        inspect_kernel: None,
        inspect_session: None,
        current_invocation: None,
        next_stream_id: 1,
        broker: None,
        raw_resource_authorizer: None,
        transport: Some(ResourceTransportSource::Injected(
            InjectedResourceTransport::Stream(PersistentResourceTransport {
                stream: Some(client),
                handshake_complete: false,
                protocol: ResourceProtocolConnection::new(),
                server_task: None,
            }),
        )),
        pending: None,
        broker_pending: None,
        detached: Vec::new(),
        detached_broker: None,
        cancellation: ResourceCancellation::new(),
    };

    assert!(matches!(
        executor.execute(request.clone()),
        ClientResourceCompletion::Pending { .. }
    ));
    assert!(
        accepted_receiver
            .recv_timeout(Duration::from_secs(1))
            .is_ok(),
        "resource request was not accepted before abandon",
    );
    assert_eq!(executor.abandon(request), Ok(()));
    let late_result = late_receiver.recv_timeout(Duration::from_secs(1));
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    let worker_finished = loop {
        if executor
            .detached
            .first()
            .is_some_and(|resource| resource.worker.is_finished())
        {
            break true;
        }
        if std::time::Instant::now() >= deadline {
            break false;
        }
        thread::yield_now();
    };
    release_sender.send(()).expect("release peer");
    peer_thread.join().expect("resource peer");
    assert!(late_result.is_ok(), "late values did not reach the worker");
    assert!(
        worker_finished,
        "direct abandon must close controls after late values"
    );
    executor.reap_detached();
    assert!(executor.detached.is_empty());
    assert!(executor.transport.is_none());
    assert!(executor.poll().is_none());
}

#[test]
fn installed_executor_cancelled_before_execute_does_not_dispatch() {
    let (active, _registry) = transport_test_context();
    let mut state = ClientStateStore::new();
    let request = installed_client_test_request(&active, &mut state, 0x97);
    let (broker, mut commands) = SharedInvokeBroker::pending();
    let (_peer, client) = StandardUnixStream::pair().expect("resource socket pair");
    let cancellation = ResourceCancellation::new();
    assert!(cancellation.request_cancel());
    let mut executor = InstalledClientResourceExecutor {
        active,
        inspect_kernel: None,
        inspect_session: None,
        current_invocation: None,
        next_stream_id: 1,
        broker: Some(broker.clone()),
        raw_resource_authorizer: None,
        transport: Some(ResourceTransportSource::Injected(
            InjectedResourceTransport::Stream(PersistentResourceTransport {
                stream: Some(client),
                handshake_complete: false,
                protocol: ResourceProtocolConnection::new(),
                server_task: None,
            }),
        )),
        pending: None,
        broker_pending: None,
        detached: Vec::new(),
        detached_broker: None,
        cancellation,
    };

    assert!(matches!(
        executor.execute(request.clone()),
        ClientResourceCompletion::Cancelled {
            request_id,
            key,
            generation,
        } if request_id == request.request_id()
            && key == request.key()
            && generation == request.generation()
    ));
    assert_eq!(executor.next_stream_id, 1);
    assert!(executor.transport.is_some());
    assert!(executor.broker_pending.is_none());
    assert!(
        commands.try_recv().is_err(),
        "cancelled request was dispatched"
    );
    assert!(
        broker
            .resource_expectations
            .lock()
            .expect(BROKER_RESOURCE_EXPECTATION_LOCK)
            .is_empty(),
        "cancelled request was registered with the broker"
    );
}

#[test]
fn detached_raw_poll_publishes_terminal_after_timeout_marker() {
    let (active, _registry) = transport_test_context();
    let mut state = ClientStateStore::new();
    let request = installed_client_test_request(&active, &mut state, 0x91);
    let (sender, completion) = std::sync::mpsc::sync_channel(2);
    sender
        .send(CancellationWaitOutcome::TimedOut)
        .expect("timeout marker");
    let worker = thread::spawn(|| {});
    let mut executor = InstalledClientResourceExecutor {
        active,
        inspect_kernel: None,
        inspect_session: None,
        current_invocation: None,
        next_stream_id: 1,
        broker: None,
        raw_resource_authorizer: None,
        transport: None,
        pending: None,
        broker_pending: None,
        detached: vec![DetachedResourceTransport {
            request: request.clone(),
            control: None,
            worker,
            waiter: None,
            completion: Some(completion),
            transport_return: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }],
        detached_broker: None,
        cancellation: ResourceCancellation::new(),
    };

    assert!(executor.poll().is_none());
    sender
        .send(CancellationWaitOutcome::Terminal(Ok(
            ResourceTransportOutcome::Cancelled {
                nested_invocation_id: None,
            },
        )))
        .expect("delayed cancellation terminal");
    assert!(matches!(
        executor.poll(),
        Some(ClientResourceCompletion::Cancelled { .. })
    ));
    assert!(executor.detached.is_empty());
}

#[test]
fn detached_broker_poll_publishes_terminal_after_timeout_marker() {
    let (active, _registry) = transport_test_context();
    let mut state = ClientStateStore::new();
    let request = installed_client_test_request(&active, &mut state, 0x92);
    let (broker, _commands) = SharedInvokeBroker::pending();
    let (sender, completion) = std::sync::mpsc::sync_channel(2);
    sender
        .send(CancellationWaitOutcome::TimedOut)
        .expect("timeout marker");
    let mut executor = InstalledClientResourceExecutor {
        active,
        inspect_kernel: None,
        inspect_session: None,
        current_invocation: None,
        next_stream_id: 1,
        broker: Some(broker.clone()),
        raw_resource_authorizer: None,
        transport: None,
        pending: None,
        broker_pending: None,
        detached: Vec::new(),
        detached_broker: Some(DetachedBrokerResource {
            control: broker,
            stream_id: 1,
            request: request.clone(),
            waiter: None,
            completion: Some(completion),
            abandoned: false,
        }),
        cancellation: ResourceCancellation::new(),
    };

    assert!(executor.poll().is_none());
    sender
        .send(CancellationWaitOutcome::Terminal(Ok(
            ResourceTransportOutcome::Cancelled {
                nested_invocation_id: None,
            },
        )))
        .expect("delayed cancellation terminal");
    assert!(matches!(
        executor.poll(),
        Some(ClientResourceCompletion::Cancelled { .. })
    ));
    assert!(executor.detached_broker.is_none());
}

#[test]
fn detached_broker_abandon_suppresses_delayed_terminal() {
    let (active, _registry) = transport_test_context();
    let mut state = ClientStateStore::new();
    let request = installed_client_test_request(&active, &mut state, 0x92);
    let (broker, mut commands) = SharedInvokeBroker::pending();
    let (sender, completion) = std::sync::mpsc::sync_channel(2);
    sender
        .send(CancellationWaitOutcome::TimedOut)
        .expect("timeout marker");
    let mut executor = InstalledClientResourceExecutor {
        active,
        inspect_kernel: None,
        inspect_session: None,
        current_invocation: None,
        next_stream_id: 1,
        broker: Some(broker.clone()),
        raw_resource_authorizer: None,
        transport: None,
        pending: None,
        broker_pending: None,
        detached: Vec::new(),
        detached_broker: Some(DetachedBrokerResource {
            control: broker,
            stream_id: 1,
            request: request.clone(),
            waiter: None,
            completion: Some(completion),
            abandoned: false,
        }),
        cancellation: ResourceCancellation::new(),
    };

    assert!(executor.poll().is_none());
    assert_eq!(executor.abandon(request.clone()), Ok(()));
    assert!(matches!(
        commands.try_recv(),
        Ok(BrokerCommand::AbandonResource { stream_id: 1, .. })
    ));
    assert!(
        sender
            .send(CancellationWaitOutcome::Terminal(Ok(
                ResourceTransportOutcome::Cancelled {
                    nested_invocation_id: None,
                },
            )))
            .is_err(),
        "abandon must drop the detached completion channel",
    );
    assert!(executor.poll().is_none());
    executor.reap_detached();
    assert!(executor.detached_broker.is_none());
}

#[test]
fn installed_executor_detached_raw_abandon_requires_exact_identity() {
    let (active, _registry) = transport_test_context();
    let mut state = ClientStateStore::new();
    let request = installed_client_test_request(&active, &mut state, 0x93);
    let mismatched_request = installed_client_test_request(&active, &mut state, 0x96);
    let (control, mut controls) = mpsc::unbounded_channel();
    let (finished_sender, finished_receiver) = std::sync::mpsc::sync_channel(0);
    let worker = thread::spawn(move || {
        assert!(matches!(
            controls.blocking_recv(),
            Some(ResourceTransportControl::Shutdown)
        ));
        finished_sender.send(()).expect("worker completion signal");
    });
    let mut executor = InstalledClientResourceExecutor {
        active,
        inspect_kernel: None,
        inspect_session: None,
        current_invocation: None,
        next_stream_id: 1,
        broker: None,
        raw_resource_authorizer: None,
        transport: None,
        pending: None,
        broker_pending: None,
        detached: vec![DetachedResourceTransport {
            request: request.clone(),
            control: Some(control),
            worker,
            waiter: None,
            completion: None,
            transport_return: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }],
        detached_broker: None,
        cancellation: ResourceCancellation::new(),
    };

    assert_eq!(
        executor.abandon(mismatched_request),
        Err("resource executor request mismatch".to_owned())
    );
    assert!(
        executor
            .detached
            .first()
            .is_some_and(|resource| { resource.control.is_some() })
    );

    assert_eq!(executor.abandon(request), Ok(()));
    assert!(
        executor
            .detached
            .first()
            .is_some_and(|resource| { resource.control.is_none() })
    );
    finished_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("detached raw worker did not receive shutdown");
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while !executor.detached.is_empty() && std::time::Instant::now() < deadline {
        executor.reap_detached();
        if !executor.detached.is_empty() {
            thread::yield_now();
        }
    }
    assert!(executor.detached.is_empty());
}

#[test]
fn installed_executor_detached_broker_abandon_requires_exact_identity() {
    let (active, _registry) = transport_test_context();
    let mut state = ClientStateStore::new();
    let request = installed_client_test_request(&active, &mut state, 0xa3);
    let mismatched_request = installed_client_test_request(&active, &mut state, 0xa6);
    let (broker, mut commands) = SharedInvokeBroker::pending();
    let mut executor = InstalledClientResourceExecutor {
        active,
        inspect_kernel: None,
        inspect_session: None,
        current_invocation: None,
        next_stream_id: 1,
        broker: Some(broker.clone()),
        raw_resource_authorizer: None,
        transport: None,
        pending: None,
        broker_pending: None,
        detached: Vec::new(),
        detached_broker: Some(DetachedBrokerResource {
            control: broker,
            stream_id: 1,
            request: request.clone(),
            waiter: None,
            completion: None,
            abandoned: false,
        }),
        cancellation: ResourceCancellation::new(),
    };

    assert_eq!(
        executor.abandon(mismatched_request),
        Err("resource executor request mismatch".to_owned())
    );
    assert!(
        executor
            .detached_broker
            .as_ref()
            .is_some_and(|resource| !resource.abandoned)
    );
    assert!(commands.try_recv().is_err());

    assert_eq!(executor.abandon(request.clone()), Ok(()));
    assert!(
        executor
            .detached_broker
            .as_ref()
            .is_some_and(|resource| resource.abandoned)
    );
    assert!(matches!(
        commands.try_recv(),
        Ok(BrokerCommand::AbandonResource {
            stream_id: 1,
            request_id,
            reason: ResourceCancellationCode::RuntimeShutdown,
        }) if request_id == request.request_id()
    ));
    executor.reap_detached();
    assert!(executor.detached_broker.is_none());
}

#[test]
fn installed_executor_broker_abandon_retains_pending_on_closed_channel() {
    let (active, _registry) = transport_test_context();
    let mut state = ClientStateStore::new();
    let request = installed_client_test_request(&active, &mut state, 0xa9);
    let (broker, commands) = SharedInvokeBroker::pending();
    drop(commands);
    let (_sender, receiver) = mpsc::channel(1);
    let mut executor = InstalledClientResourceExecutor {
        active,
        inspect_kernel: None,
        inspect_session: None,
        current_invocation: None,
        next_stream_id: 1,
        broker: Some(broker.clone()),
        raw_resource_authorizer: None,
        transport: None,
        pending: None,
        broker_pending: Some(PendingBrokerResource {
            request: request.clone(),
            receiver,
            control: broker,
            stream_id: 1,
            cancel_requested: false,
        }),
        detached: Vec::new(),
        detached_broker: None,
        cancellation: ResourceCancellation::new(),
    };

    assert_eq!(
        executor.abandon(request.clone()),
        Err("resource executor broker unavailable".to_owned())
    );
    assert!(
        executor
            .broker_pending
            .as_ref()
            .is_some_and(|pending| { same_resource_request_identity(&pending.request, &request) }),
        "closed broker must retain the exact pending request",
    );
}

#[test]
fn installed_executor_detached_raw_cancel_requires_exact_identity() {
    let (active, _registry) = transport_test_context();
    let mut state = ClientStateStore::new();
    let request = installed_client_test_request(&active, &mut state, 0xb3);
    let mismatched_request = installed_client_test_request(&active, &mut state, 0xb6);
    let (control, _controls) = mpsc::unbounded_channel();
    let worker = thread::spawn(|| {});
    let mut executor = InstalledClientResourceExecutor {
        active,
        inspect_kernel: None,
        inspect_session: None,
        current_invocation: None,
        next_stream_id: 1,
        broker: None,
        raw_resource_authorizer: None,
        transport: None,
        pending: None,
        broker_pending: None,
        detached: vec![DetachedResourceTransport {
            request: request.clone(),
            control: Some(control),
            worker,
            waiter: None,
            completion: None,
            transport_return: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }],
        detached_broker: None,
        cancellation: ResourceCancellation::new(),
    };

    assert!(matches!(
        executor.cancel(mismatched_request),
        ClientResourceCompletion::Failed {
            code,
            ..
        } if code == SERVER_RESOURCE_INTERNAL_CODE
    ));
    assert_eq!(executor.detached.len(), 1);
    assert!(matches!(
        executor.cancel(request.clone()),
        ClientResourceCompletion::Pending {
            request_id,
            key,
            generation,
        } if request_id == request.request_id()
            && key == request.key()
            && generation == request.generation()
    ));
    assert_eq!(executor.detached.len(), 1);
}

#[test]
fn installed_executor_detached_broker_cancel_requires_exact_identity() {
    let (active, _registry) = transport_test_context();
    let mut state = ClientStateStore::new();
    let request = installed_client_test_request(&active, &mut state, 0xc3);
    let mismatched_request = installed_client_test_request(&active, &mut state, 0xc6);
    let (broker, _commands) = SharedInvokeBroker::pending();
    let mut executor = InstalledClientResourceExecutor {
        active,
        inspect_kernel: None,
        inspect_session: None,
        current_invocation: None,
        next_stream_id: 1,
        broker: Some(broker.clone()),
        raw_resource_authorizer: None,
        transport: None,
        pending: None,
        broker_pending: None,
        detached: Vec::new(),
        detached_broker: Some(DetachedBrokerResource {
            control: broker,
            stream_id: 1,
            request: request.clone(),
            waiter: None,
            completion: None,
            abandoned: false,
        }),
        cancellation: ResourceCancellation::new(),
    };

    assert!(matches!(
        executor.cancel(mismatched_request),
        ClientResourceCompletion::Failed {
            code,
            ..
        } if code == SERVER_RESOURCE_INTERNAL_CODE
    ));
    assert!(executor.detached_broker.is_some());
    assert!(matches!(
        executor.cancel(request.clone()),
        ClientResourceCompletion::Pending {
            request_id,
            key,
            generation,
        } if request_id == request.request_id()
            && key == request.key()
            && generation == request.generation()
    ));
    assert!(executor.detached_broker.is_some());
}

#[test]
fn reap_detached_drops_reset_injected_transport() {
    let (active, _registry) = transport_test_context();
    let mut state = ClientStateStore::new();
    let request = installed_client_test_request(&active, &mut state, 0xd3);
    let transport_return = std::sync::Arc::new(std::sync::Mutex::new(Some(
        ResourceTransportSource::Injected(InjectedResourceTransport::Stream(
            PersistentResourceTransport {
                stream: None,
                handshake_complete: false,
                protocol: ResourceProtocolConnection::new(),
                server_task: None,
            },
        )),
    )));
    let worker = thread::spawn(|| {});
    let mut executor = InstalledClientResourceExecutor {
        active,
        inspect_kernel: None,
        inspect_session: None,
        current_invocation: None,
        next_stream_id: 1,
        broker: None,
        raw_resource_authorizer: None,
        transport: None,
        pending: None,
        broker_pending: None,
        detached: vec![DetachedResourceTransport {
            request: request.clone(),
            control: None,
            worker,
            waiter: None,
            completion: None,
            transport_return: transport_return.clone(),
        }],
        detached_broker: None,
        cancellation: ResourceCancellation::new(),
    };

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while !executor.detached.is_empty() && std::time::Instant::now() < deadline {
        executor.reap_detached();
        if !executor.detached.is_empty() {
            thread::yield_now();
        }
    }
    assert!(executor.detached.is_empty());
    assert!(executor.transport.is_none());
    assert!(
        transport_return
            .lock()
            .expect("transport return lock")
            .is_none()
    );
    assert!(matches!(
        executor.execute(request),
        ClientResourceCompletion::Failed {
            code,
            ..
        } if code == SERVER_RESOURCE_INTERNAL_CODE
    ));
}

#[test]
fn socket_transport_retains_nested_identity_for_terminal_failure() {
    let (active, registry) = transport_test_context();
    let request = transport_test_request(active.pair(), 1);
    let nested_invocation_id = InvocationId::from_bytes([0x52; 16]);
    let (peer, client) = StandardUnixStream::pair().expect("resource socket pair");
    let peer_thread = thread::spawn({
        let peer_active = active.clone();
        let peer_registry = registry.clone();
        move || {
            serve_one_terminal_test_request(
                peer,
                peer_active,
                peer_registry,
                ProtocolResourceKind::Single,
                nested_invocation_id,
                SocketTerminal::Failed(CallFailure::ExecuteDenied),
            );
        }
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let (completion_sender, _completion_receiver) = mpsc::channel(1);
    let (_control_sender, controls) = mpsc::unbounded_channel();
    let result = runtime.block_on(run_resource_transport(
        client,
        false,
        ResourceProtocolConnection::new(),
        active,
        registry,
        request,
        ResolvedType::Scalar(StandardScalar::Integer),
        ProtocolResourceKind::Single,
        None,
        controls,
        &completion_sender,
    ));
    let run = result.expect("failed transport run");
    assert!(matches!(
        run.outcome,
        ResourceTransportOutcome::Failed {
            failure: CallFailure::ExecuteDenied,
            nested_invocation_id: Some(actual),
        } if actual == nested_invocation_id
    ));
    peer_thread.join().expect("resource peer");
}

#[test]
fn socket_transport_rejects_zero_nested_invocation_identity() {
    let (active, registry) = transport_test_context();
    let request = transport_test_request(active.pair(), 1);
    let (peer, client) = StandardUnixStream::pair().expect("resource socket pair");
    let peer_active = active.clone();
    let peer_registry = registry.clone();
    let peer_thread = thread::spawn(move || {
        let mut peer = peer;
        peer.set_read_timeout(Some(Duration::from_secs(5)))
            .expect("peer read timeout");
        let mut hello = [0_u8; CONSTRUCTED_CLIENT_HELLO.len()];
        peer.read_exact(&mut hello).expect("constructed hello");
        assert_eq!(hello, CONSTRUCTED_CLIENT_HELLO);
        peer.write_all(&CONSTRUCTED_SERVER_ACK)
            .expect("constructed acknowledgement");
        let encoded = read_resource_test_frame(&mut peer);
        let ResourceClientFrame::Request(request) =
            decode_resource_client_frame(&peer_active, &peer_registry, &encoded)
                .expect("client resource request")
        else {
            panic!("the client sent a non-request resource frame");
        };
        let frame = ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id: request.stream_id,
            request_id: request.request_id,
            nested_invocation_id: InvocationId::from_bytes([0x44; 16]),
            target_revision: request.target_revision,
            resource_kind: request.resource_kind,
        });
        let mut encoded = encode_resource_server_frame(&peer_active, &peer_registry, &frame)
            .expect("encoded resource response");
        let nested_start = RESOURCE_HEADER_LENGTH + 8 + 16;
        encoded[nested_start..nested_start + 16].fill(0);
        peer.write_all(&encoded).expect("resource response");
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let (completion_sender, _completion_receiver) = mpsc::channel(1);
    let (_control_sender, controls) = mpsc::unbounded_channel();
    let (stream, handshake_complete, protocol) = (client, false, ResourceProtocolConnection::new());
    let result = runtime.block_on(run_resource_transport(
        stream,
        handshake_complete,
        protocol,
        active,
        registry,
        request,
        ResolvedType::Scalar(StandardScalar::Integer),
        ProtocolResourceKind::Single,
        None,
        controls,
        &completion_sender,
    ));
    assert!(matches!(result, Err(ResourceTransportFailure::Shape)));
    peer_thread.join().expect("peer thread");
}

#[test]
fn persistent_transport_reuses_handshake_and_monotonic_stream_ids() {
    let (active, registry) = transport_test_context();
    let (peer, client) = StandardUnixStream::pair().expect("resource socket pair");
    let peer_active = active.clone();
    let peer_registry = registry.clone();
    let peer_thread =
        thread::spawn(move || serve_two_scalar_test_requests(peer, peer_active, peer_registry));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let mut transport = ResourceTransportSource::Injected(InjectedResourceTransport::Stream(
        PersistentResourceTransport {
            stream: Some(client),
            handshake_complete: false,
            protocol: ResourceProtocolConnection::new(),
            server_task: None,
        },
    ));

    assert_eq!(
        run_scalar_test_request(
            &runtime,
            &mut transport,
            &active,
            &registry,
            transport_test_request(active.pair(), 1),
        ),
        RuntimeValue::Integer(1),
    );
    assert_eq!(
        run_scalar_test_request(
            &runtime,
            &mut transport,
            &active,
            &registry,
            transport_test_request(active.pair(), 2),
        ),
        RuntimeValue::Integer(2),
    );
    let persistent = transport.persistent();
    assert_eq!(persistent.protocol.high_water_mark(), Some(2));
    assert!(persistent.stream.is_some());
    let requests = peer_thread.join().expect("resource peer");
    assert_eq!(
        requests
            .iter()
            .map(|request| request.stream_id)
            .collect::<Vec<_>>(),
        vec![1, 2],
    );
}

#[test]
fn persistent_transport_reset_clears_state_after_transport_error() {
    let (active, _registry) = transport_test_context();
    let (_peer, client) = StandardUnixStream::pair().expect("resource socket pair");
    let mut source = ResourceTransportSource::Injected(InjectedResourceTransport::Stream(
        PersistentResourceTransport {
            stream: Some(client),
            handshake_complete: true,
            protocol: ResourceProtocolConnection::new(),
            server_task: None,
        },
    ));
    source
        .persistent()
        .protocol
        .open(transport_test_request(active.pair(), 1))
        .expect("resource protocol state");

    source.reset();
    let transport = source.persistent();
    assert!(transport.stream.is_none());
    assert!(!transport.handshake_complete);
    assert_eq!(transport.protocol.high_water_mark(), None);
    assert_eq!(transport.protocol.live_resources(), 0);
}

#[test]
fn inspect_denials_do_not_disclose_epoch_existence() {
    let missing_epoch = inspect_kernel_error_code(PostgresKernelError::InspectDenied {
        reason: orna_core::security::InspectDenial::MissingEpoch,
    });
    let missing_privilege = inspect_kernel_error_code(PostgresKernelError::InspectDenied {
        reason: orna_core::security::InspectDenial::MissingPrivilege,
    });

    assert_eq!(missing_epoch, INSPECT_DENIED_CODE);
    assert_eq!(missing_privilege, INSPECT_DENIED_CODE);
}
#[tokio::test]
async fn broker_resource_stream_ids_span_sequential_root_executors() {
    let (active, registry) = transport_test_context();
    let (broker, mut commands) = SharedInvokeBroker::pending();
    let mut connection = ProtocolConnection::new();
    let mut root = None;
    let mut resources = BTreeMap::new();
    let mut resource_high_water_mark = None;
    let mut tombstones = BrokerResourceTombstones::new();
    let (_reader, mut writer) = tokio::io::duplex(64 * 1024);

    let mut first_state = ClientStateStore::new();
    let first_request = installed_client_test_request(&active, &mut first_state, 0xe3);
    let mut first = InstalledClientResourceExecutor {
        active: active.clone(),
        inspect_kernel: None,
        inspect_session: None,
        current_invocation: None,
        next_stream_id: 1,
        broker: Some(broker.clone()),
        raw_resource_authorizer: None,
        transport: None,
        pending: None,
        broker_pending: None,
        detached: Vec::new(),
        detached_broker: None,
        cancellation: ResourceCancellation::new(),
    };
    assert!(matches!(
        first.execute(first_request),
        ClientResourceCompletion::Pending { .. }
    ));
    let first_command = commands.try_recv().expect("first root resource command");
    let first_protocol_request = match &first_command {
        BrokerCommand::StartResource { request, .. } => request.clone(),
        _ => panic!("first executor sent a non-resource command"),
    };
    assert_eq!(first_protocol_request.stream_id, 1);
    handle_shared_broker_command(
        first_command,
        &mut writer,
        &active,
        &registry,
        &mut connection,
        &mut root,
        &mut resources,
        &mut resource_high_water_mark,
        &mut tombstones,
        &Arc::new(Mutex::new(BTreeMap::new())),
    )
    .await
    .expect("first resource command is accepted");
    let first_failure = encode_resource_server_frame(
        &active,
        &registry,
        &ResourceServerFrame::Failed(ResourceFailed {
            stream_id: first_protocol_request.stream_id,
            request_id: first_protocol_request.request_id,
            target_revision: first_protocol_request.target_revision,
            failure: CallFailure::ExecuteDenied,
        }),
    )
    .expect("first resource failure encodes");
    handle_shared_broker_frame(
        BrokerWireFrame {
            resource: true,
            bytes: first_failure,
        },
        &mut writer,
        &active,
        &registry,
        &mut root,
        &mut resources,
        resource_high_water_mark,
        &mut tombstones,
        &Arc::new(Mutex::new(BTreeMap::new())),
    )
    .await
    .expect("first resource terminal is accepted");
    assert!(matches!(
        first.poll(),
        Some(ClientResourceCompletion::Failed { .. })
    ));
    drop(first);

    let mut second_state = ClientStateStore::new();
    let second_request = installed_client_test_request(&active, &mut second_state, 0xe4);
    let mut second = InstalledClientResourceExecutor {
        active: active.clone(),
        inspect_kernel: None,
        inspect_session: None,
        current_invocation: None,
        next_stream_id: 1,
        broker: Some(broker),
        raw_resource_authorizer: None,
        transport: None,
        pending: None,
        broker_pending: None,
        detached: Vec::new(),
        detached_broker: None,
        cancellation: ResourceCancellation::new(),
    };
    assert!(matches!(
        second.execute(second_request),
        ClientResourceCompletion::Pending { .. }
    ));
    let second_command = commands.try_recv().expect("second root resource command");
    let second_protocol_request = match &second_command {
        BrokerCommand::StartResource { request, .. } => request.clone(),
        _ => panic!("second executor sent a non-resource command"),
    };
    assert_eq!(second_protocol_request.stream_id, 2);
    handle_shared_broker_command(
        second_command,
        &mut writer,
        &active,
        &registry,
        &mut connection,
        &mut root,
        &mut resources,
        &mut resource_high_water_mark,
        &mut tombstones,
        &Arc::new(Mutex::new(BTreeMap::new())),
    )
    .await
    .expect("second resource command advances connection high-water");
    assert_eq!(resource_high_water_mark, Some(2));
    let _ = second
        .broker_pending
        .take()
        .expect("second root resource remains pending");
}
