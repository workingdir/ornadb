use super::*;

#[tokio::test]
async fn closed_shutdown_receiver_wakes_waiter() {
    let (sender, mut shutdown) = watch::channel(false);
    drop(sender);
    timeout(Duration::from_millis(25), wait_for_shutdown(&mut shutdown))
        .await
        .expect("closed shutdown receiver resolves");
}

#[tokio::test]
async fn server_shutdown_stops_reads_but_drains_accepted_work() {
    let dispatcher = GatedDispatch::new();
    let polled = Arc::clone(&dispatcher.polled);
    let resources = LocalRawSocketResources::new();
    let (shutdown_sender, shutdown) = watch::channel(false);
    let (server, mut client) = UnixStream::pair().expect("Unix stream pair");
    let mut server_task = tokio::spawn(drive_authenticated_stream_until_shutdown(
        dispatcher.clone(),
        test_session(),
        server,
        resources,
        shutdown,
    ));

    shutdown_sender.send(false).expect("false signal");
    send_parameter_free_call(&mut client, 1).await;
    assert!(matches!(
        read_server_frame(&mut client).await,
        ServerFrame::CallAccepted { stream: 1, .. }
    ));
    shutdown_sender.send(true).expect("shutdown signal");
    assert!(
        timeout(Duration::from_millis(25), &mut server_task)
            .await
            .is_err(),
        "shutdown returned before protected work drained"
    );
    assert!(polled.load(Ordering::SeqCst));

    dispatcher.release.notify_one();
    server_task
        .await
        .expect("connection task")
        .expect("ordered shutdown");
    let mut byte = [0_u8; 1];
    assert_eq!(client.read(&mut byte).await.expect("shutdown EOF"), 0);
}

#[tokio::test]
async fn server_shutdown_interrupts_a_blocked_socket_write() {
    let (mut writer, _reader) = tokio::io::duplex(1);
    let (shutdown_sender, mut shutdown) = watch::channel(false);
    let mut write =
        tokio::spawn(
            async move { write_all_until_shutdown(&mut writer, &[1, 2], &mut shutdown).await },
        );
    tokio::task::yield_now().await;

    shutdown_sender.send(false).expect("false signal");
    assert!(
        timeout(Duration::from_millis(25), &mut write)
            .await
            .is_err(),
        "a false value incorrectly signalled shutdown"
    );

    shutdown_sender.send(true).expect("shutdown signal");
    assert!(!write.await.expect("write task").expect("shutdown write"));
}

#[test]
fn fixed_listener_replaces_only_a_verified_stale_socket_and_drains_connections() {
    let runtime_directory = listener_test_directory();
    let _ = fs::remove_dir_all(&runtime_directory);
    fs::create_dir_all(&runtime_directory).expect("runtime directory");
    fs::set_permissions(&runtime_directory, fs::Permissions::from_mode(0o711))
        .expect("runtime directory mode");
    let socket_path = runtime_directory.join(SOCKET_NAME);
    let stale = StandardUnixListener::bind(&socket_path).expect("stale socket");
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o666))
        .expect("stale socket mode");
    drop(stale);

    let server = start_local_raw_socket(&runtime_directory, unavailable_kernel())
        .expect("fixed listener starts");
    assert!(server.is_healthy());
    let metadata = fs::symlink_metadata(&socket_path).expect("public socket metadata");
    assert!(metadata.file_type().is_socket());
    assert_eq!(metadata.mode() & 0o7777, 0o666);
    assert_eq!(metadata.nlink(), 1);

    let clients: Vec<_> = (0..CONNECTION_LIMIT)
        .map(|_| BlockingUnixStream::connect(&socket_path).expect("admitted connection"))
        .collect();
    std::thread::sleep(Duration::from_millis(25));
    let mut rejected = BlockingUnixStream::connect(&socket_path).expect("capacity connection");
    rejected
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("capacity timeout");
    let mut byte = [0_u8; 1];
    assert_eq!(
        std::io::Read::read(&mut rejected, &mut byte).expect("capacity close"),
        0
    );

    drop(clients);
    server.stop().expect("ordered listener stop");
    assert!(!socket_path.exists());

    fs::write(&socket_path, b"hostile").expect("hostile socket path");
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o666))
        .expect("hostile path mode");
    assert!(matches!(
        start_local_raw_socket(&runtime_directory, unavailable_kernel()),
        Err(LocalRawSocketServerError::InvalidSocketState)
    ));
    fs::remove_file(&socket_path).expect("hostile path cleanup");
    fs::remove_dir(&runtime_directory).expect("runtime directory cleanup");
}

fn listener_test_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target")
        .join(format!("raw-socket-listener-{}", std::process::id()))
}

#[tokio::test]
async fn shared_dispatch_limit_rejects_a_second_connection_before_acceptance() {
    let resources = LocalRawSocketResources::new();
    let held: Vec<_> = (1..KERNEL_OPERATION_LIMIT)
        .map(|_| {
            resources
                .reserve_kernel_operation()
                .expect("operation permit")
        })
        .collect();
    let gated = GatedDispatch::new();
    let (first_server, mut first_client) = UnixStream::pair().expect("first stream pair");
    let first_task = tokio::spawn(drive_authenticated_stream(
        gated.clone(),
        test_session(),
        first_server,
        resources.clone(),
    ));
    send_parameter_free_call(&mut first_client, 1).await;
    assert!(matches!(
        read_server_frame(&mut first_client).await,
        ServerFrame::CallAccepted { stream: 1, .. }
    ));
    assert_eq!(resources.kernel_operations.available_permits(), 0);

    let (second_server, mut second_client) = UnixStream::pair().expect("second stream pair");
    let second_task = tokio::spawn(drive_authenticated_stream(
        TestDispatch::new(vec![ServerAction::Completed { stream: 1 }]),
        test_session(),
        second_server,
        resources.clone(),
    ));
    send_parameter_free_call(&mut second_client, 1).await;
    let mut response = [0_u8; 1];
    assert_eq!(
        second_client
            .read(&mut response)
            .await
            .expect("capacity close"),
        0
    );
    assert!(matches!(
        second_task.await.expect("second connection task"),
        Err(LocalRawSocketError::KernelCapacity)
    ));
    assert_eq!(resources.kernel_operations.available_permits(), 0);

    gated.release.notify_one();
    assert_eq!(
        read_server_frame(&mut first_client).await,
        ServerFrame::CallCompleted { stream: 1 }
    );
    first_client
        .shutdown()
        .await
        .expect("first client shutdown");
    first_task
        .await
        .expect("first connection task")
        .expect("first connection closes");
    drop(held);
    assert_eq!(
        resources.kernel_operations.available_permits(),
        KERNEL_OPERATION_LIMIT
    );
}

#[tokio::test]
async fn retained_payload_and_operation_guards_survive_cancelled_connection_drain() {
    let resources = LocalRawSocketResources::new();
    let gated = GatedDispatch::new();
    let (server, mut client) = UnixStream::pair().expect("Unix stream pair");
    let server_task = tokio::spawn(drive_authenticated_stream(
        gated.clone(),
        test_session(),
        server,
        resources.clone(),
    ));
    let start = ClientFrame::CallRawStart {
        stream: 1,
        function: FUNCTION,
    };
    let complete = ClientFrame::CallArgumentsComplete { stream: 1 };
    let retained = encode_client_frame(&start)
        .expect("start frame encodes")
        .len()
        - FRAME_HEADER_LENGTH;
    send_client_frame(&mut client, &start).await;
    send_client_frame(&mut client, &complete).await;
    assert!(matches!(
        read_server_frame(&mut client).await,
        ServerFrame::CallAccepted { stream: 1, .. }
    ));
    send_client_frame(&mut client, &ClientFrame::CallCancel { stream: 1 }).await;
    drop(client);
    while !gated.polled.load(Ordering::SeqCst) {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        resources.payload.available_permits(),
        SHARED_PAYLOAD_BYTES - retained
    );
    assert_eq!(
        resources.kernel_operations.available_permits(),
        KERNEL_OPERATION_LIMIT - 1
    );

    gated.release.notify_one();
    let _ = server_task
        .await
        .expect("connection task drains after cancellation");
    assert_eq!(resources.payload.available_permits(), SHARED_PAYLOAD_BYTES);
    assert_eq!(
        resources.kernel_operations.available_permits(),
        KERNEL_OPERATION_LIMIT
    );
}

#[tokio::test]
async fn completed_dispatch_releases_guards_before_flow_control_delivers_it() {
    let resources = LocalRawSocketResources::new();
    let held: Vec<_> = (1..KERNEL_OPERATION_LIMIT)
        .map(|_| {
            resources
                .reserve_kernel_operation()
                .expect("operation permit")
        })
        .collect();
    let dispatcher = TestDispatch::new(vec![
        ServerAction::Events {
            stream: 1,
            events: vec![Event::Value(RuntimeValue::Boolean(true))],
        },
        ServerAction::Completed { stream: 1 },
    ]);
    let (server, mut client) = UnixStream::pair().expect("Unix stream pair");
    let server_task = tokio::spawn(drive_authenticated_stream(
        dispatcher,
        test_session(),
        server,
        resources.clone(),
    ));
    send_parameter_free_call(&mut client, 1).await;
    assert!(matches!(
        read_server_frame(&mut client).await,
        ServerFrame::CallAccepted { stream: 1, .. }
    ));

    let released = timeout(
        Duration::from_secs(1),
        resources.kernel_operations.clone().acquire_owned(),
    )
    .await
    .expect("completed dispatch releases its operation permit before credit")
    .expect("operation semaphore remains open");
    drop(released);
    assert_eq!(
        resources.kernel_operations.available_permits(),
        1,
        "the probe permit is returned while the other permits remain held"
    );
    assert_eq!(
        resources.payload.available_permits(),
        SHARED_PAYLOAD_BYTES,
        "completed action queue owns the produced terminal bytes"
    );

    send_client_frame(
        &mut client,
        &ClientFrame::WindowUpdate {
            stream: 1,
            channel: Channel::ResultValues,
            credit: 1024,
        },
    )
    .await;
    assert!(matches!(
        read_server_frame(&mut client).await,
        ServerFrame::EventBatch { stream: 1, .. }
    ));
    assert_eq!(
        read_server_frame(&mut client).await,
        ServerFrame::CallCompleted { stream: 1 }
    );

    client.shutdown().await.expect("client shutdown");
    server_task
        .await
        .expect("connection task")
        .expect("connection closes");
    drop(held);
    assert_eq!(
        resources.kernel_operations.available_permits(),
        KERNEL_OPERATION_LIMIT
    );
    assert_eq!(resources.payload.available_permits(), SHARED_PAYLOAD_BYTES);
}

#[tokio::test]
async fn aborting_the_public_waiter_does_not_cancel_owned_connection_work() {
    let resources = LocalRawSocketResources::new();
    let gated = GatedDispatch::new();
    let (server, mut client) = UnixStream::pair().expect("Unix stream pair");
    let completed = Arc::new(Notify::new());
    let completion_witness = Arc::clone(&completed);
    let owned_resources = resources.clone();
    let owned_dispatch = gated.clone();
    let (shutdown_guard, _shutdown) = watch::channel(false);
    let owned = run_owned_connection_with_shutdown_guard(shutdown_guard, async move {
        let result =
            drive_authenticated_stream(owned_dispatch, test_session(), server, owned_resources)
                .await;
        completion_witness.notify_one();
        result
    });
    let waiter = tokio::spawn(owned);
    send_parameter_free_call(&mut client, 1).await;
    assert!(matches!(
        read_server_frame(&mut client).await,
        ServerFrame::CallAccepted { stream: 1, .. }
    ));
    waiter.abort();
    assert!(
        waiter
            .await
            .expect_err("waiter is cancelled")
            .is_cancelled()
    );
    while !gated.polled.load(Ordering::SeqCst) {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        resources.kernel_operations.available_permits(),
        KERNEL_OPERATION_LIMIT - 1
    );

    drop(client);
    gated.release.notify_one();
    timeout(Duration::from_secs(1), completed.notified())
        .await
        .expect("detached connection and reader terminate");
}

#[tokio::test]
async fn invalid_hello_and_exhausted_authentication_capacity_close_silently() {
    let resources = LocalRawSocketResources::new();
    let (server, client) = StandardUnixStream::pair().expect("Unix stream pair");
    client.set_nonblocking(true).expect("nonblocking client");
    let mut client = UnixStream::from_std(client).expect("Tokio client stream");
    let invalid_task = tokio::spawn(serve_local_raw_stream(
        unavailable_kernel(),
        server,
        resources.clone(),
    ));
    let mut invalid = CLIENT_HELLO;
    invalid[4] = 0x02;
    client
        .write_all(&invalid)
        .await
        .expect("invalid hello writes");
    let mut response = [0_u8; 1];
    assert_eq!(client.read(&mut response).await.expect("silent close"), 0);
    assert!(matches!(
        invalid_task.await.expect("invalid hello task"),
        Err(LocalRawSocketError::InvalidHello)
    ));

    let operation_permits: Vec<_> = (0..KERNEL_OPERATION_LIMIT)
        .map(|_| {
            resources
                .reserve_kernel_operation()
                .expect("operation permit")
        })
        .collect();
    let (server, client) = StandardUnixStream::pair().expect("Unix stream pair");
    client.set_nonblocking(true).expect("nonblocking client");
    let mut client = UnixStream::from_std(client).expect("Tokio client stream");
    let capacity_task = tokio::spawn(serve_local_raw_stream(
        unavailable_kernel(),
        server,
        resources,
    ));
    client
        .write_all(&CLIENT_HELLO)
        .await
        .expect("valid hello writes");
    assert_eq!(client.read(&mut response).await.expect("silent close"), 0);
    assert!(matches!(
        capacity_task.await.expect("capacity task"),
        Err(LocalRawSocketError::KernelCapacity)
    ));
    drop(operation_permits);
}

#[tokio::test]
async fn oversized_and_unfunded_payloads_fail_before_payload_reads() {
    let resources = LocalRawSocketResources::new();
    let (mut server, mut client) = UnixStream::pair().expect("Unix stream pair");
    let mut oversized = [0_u8; FRAME_HEADER_LENGTH];
    oversized[..4].copy_from_slice(b"ORF1");
    oversized[4] = 0x06;
    oversized[14..].copy_from_slice(&((MAX_FRAME_PAYLOAD_LENGTH + 1) as u32).to_be_bytes());
    client
        .write_all(&oversized)
        .await
        .expect("oversized header writes");
    assert!(matches!(
        read_client_frame(
            &mut server,
            &resources,
            Instant::now() + Duration::from_secs(1)
        )
        .await,
        Err(LocalRawSocketError::Frame {
            source: FrameCodecError::PayloadTooLarge { .. }
        })
    ));
    assert_eq!(resources.payload.available_permits(), SHARED_PAYLOAD_BYTES);

    let payload = resources
        .reserve_payload(SHARED_PAYLOAD_BYTES)
        .expect("complete payload budget");
    let (mut server, mut client) = UnixStream::pair().expect("Unix stream pair");
    let mut unfunded = [0_u8; FRAME_HEADER_LENGTH];
    unfunded[..4].copy_from_slice(b"ORF1");
    unfunded[4] = 0x06;
    unfunded[14..].copy_from_slice(&1_u32.to_be_bytes());
    client
        .write_all(&unfunded)
        .await
        .expect("unfunded header writes");
    assert!(matches!(
        timeout(
            Duration::from_millis(25),
            read_client_frame(
                &mut server,
                &resources,
                Instant::now() + Duration::from_secs(1)
            )
        )
        .await
        .expect("capacity rejects before payload read"),
        Err(LocalRawSocketError::PayloadCapacity)
    ));
    drop(payload);
}

#[tokio::test]
async fn ordinary_versioned_raw_frame_uses_raw_decoder() {
    let resources = LocalRawSocketResources::new();
    let expected = ClientFrame::CallRawStart {
        stream: 1,
        function: FUNCTION,
    };
    let encoded = encode_client_frame(&expected).expect("raw frame encodes");
    assert_eq!(&encoded[..4], b"ORF1");
    let (mut server, mut client) = UnixStream::pair().expect("Unix stream pair");
    client.write_all(&encoded).await.expect("raw frame writes");

    let Some(IncomingFrame::Raw(RawIncomingFrame { frame, reservation })) = read_client_frame(
        &mut server,
        &resources,
        Instant::now() + Duration::from_secs(1),
    )
    .await
    .expect("raw frame reads") else {
        panic!("ordinary frame must use the raw decoder");
    };
    assert_eq!(frame, expected);
    drop(reservation);
}

pub(super) fn test_invoke_request(
    active: &ActiveDatabaseRevision,
    registry: &orna_core::value::OpaqueCodecRegistry,
) -> orna_protocol::RetainedInvokeRequest {
    let request = InvokeRequest::new(InvokeRequestInput {
        target: InvocationTarget::function_id(FunctionId::from_bytes([0x11; 16])),
        arguments: Vec::new(),
        caller_context: InvocationCallerContext::new(
            InvocationCallerKind::Browser,
            false,
            false,
            None,
            None,
            "en-GB",
            "UTC",
            None,
        )
        .expect("caller context"),
        client_offer: InvocationClientOffer::new(
            5,
            "en-GB",
            "UTC",
            Vec::new(),
            Vec::new(),
            1_024,
            0,
            None,
            None,
        )
        .expect("client offer"),
        output_requirement: None,
        state_profile: None,
        trace_policy: InvocationTracePolicy::Off,
        idempotency_key: None,
        parent_invocation_id: None,
        observer_context: None,
    })
    .expect("invoke request");
    encode_invoke_request(active, registry, &request).expect("invoke request encodes")
}

pub(super) fn test_session() -> AuthenticatedSession {
    let principal = PrincipalId::from_bytes([4; 16]);
    SecuritySnapshot::new(
        RevisionPair::new(
            SourceRevisionId::from_bytes([2; 16]),
            CatalogueRevisionId::from_bytes([3; 16]),
        ),
        vec![FUNCTION],
        vec![Principal::new(
            principal,
            PrincipalKind::User,
            PrincipalStatus::Active,
        )],
        vec![],
        vec![],
    )
    .expect("security snapshot")
    .bind_authenticated_session(principal, vec![])
    .expect("authenticated session")
}

fn unavailable_kernel() -> PostgresKernel {
    PostgresKernel::from_str("host=127.0.0.1 port=1 dbname=absent")
        .expect("configuration parses without connecting")
}

pub(super) async fn send_client_frame(stream: &mut UnixStream, frame: &ClientFrame) {
    stream
        .write_all(&encode_client_frame(frame).expect("client frame encodes"))
        .await
        .expect("client frame writes");
}

async fn send_parameter_free_call(stream: &mut UnixStream, stream_id: u64) {
    send_client_frame(
        stream,
        &ClientFrame::CallRawStart {
            stream: stream_id,
            function: FUNCTION,
        },
    )
    .await;
    send_client_frame(
        stream,
        &ClientFrame::CallArgumentsComplete { stream: stream_id },
    )
    .await;
}

pub(super) async fn read_server_frame(stream: &mut UnixStream) -> ServerFrame {
    let encoded = read_encoded_server_frame(stream, "server frame").await;
    decode_server_frame(&encoded).expect("server frame decodes")
}

pub(super) async fn read_catalogue_server_frame(
    stream: &mut UnixStream,
    catalogue: &CatalogueSnapshot,
) -> ServerFrame {
    let encoded = read_encoded_server_frame(stream, "catalogue server frame").await;
    decode_catalogue_server_frame(catalogue, &encoded).expect("catalogue server frame decodes")
}

pub(super) async fn read_constructed_server_frame(
    stream: &mut UnixStream,
    active: &ActiveDatabaseRevision,
    registry: &orna_core::value::OpaqueCodecRegistry,
) -> ServerFrame {
    let encoded = read_encoded_server_frame(stream, "constructed server frame").await;
    decode_constructed_server_frame(active, registry, &encoded)
        .expect("constructed server frame decodes")
}

pub(super) async fn read_encoded_server_frame(stream: &mut UnixStream, name: &str) -> Vec<u8> {
    let mut header = [0_u8; FRAME_HEADER_LENGTH];
    timeout(Duration::from_secs(1), stream.read_exact(&mut header))
        .await
        .unwrap_or_else(|_| panic!("{name} timeout"))
        .unwrap_or_else(|error| panic!("{name} header: {error}"));
    let length = u32::from_be_bytes(header[14..18].try_into().expect("fixed header")) as usize;
    let mut encoded = header.to_vec();
    encoded.resize(FRAME_HEADER_LENGTH + length, 0);
    stream
        .read_exact(&mut encoded[FRAME_HEADER_LENGTH..])
        .await
        .unwrap_or_else(|error| panic!("{name} payload: {error}"));
    encoded
}
