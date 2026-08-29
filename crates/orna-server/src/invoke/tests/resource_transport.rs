//! Installed resource transport and executor lifecycle tests.

use super::*;

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
