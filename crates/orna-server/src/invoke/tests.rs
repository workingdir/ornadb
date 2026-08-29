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

mod inspector;
mod resource_transport;

const ENCODED_VALUE: &[u8] = b"ORV5-encoded-value";

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
