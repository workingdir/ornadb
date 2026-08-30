use super::*;

#[tokio::test]
async fn raw_driver_round_trips_session_input_on_one_socket() {
    let root = InvocationId::from_bytes([0x9; 16]);
    let bridge = SessionBridge::new(root, 1).expect("session bridge creates");
    let received = Arc::new(Mutex::new(None));
    let dispatcher = SessionBridgeDispatch {
        bridge,
        received: Arc::clone(&received),
    };
    let (server, mut client) = UnixStream::pair().expect("Unix stream pair");
    let server_task = tokio::spawn(drive_authenticated_stream(
        dispatcher,
        test_session(),
        server,
        LocalRawSocketResources::new(),
    ));

    send_client_frame(
        &mut client,
        &ClientFrame::CallRawStart {
            stream: 1,
            function: FUNCTION,
        },
    )
    .await;
    send_client_frame(
        &mut client,
        &ClientFrame::CallArgumentsComplete { stream: 1 },
    )
    .await;
    assert!(matches!(
        read_server_frame(&mut client).await,
        ServerFrame::CallAccepted {
            stream: 1,
            invocation
        } if invocation == root
    ));

    let mut encoded = vec![0_u8; SESSION_HEADER_LENGTH];
    client
        .read_exact(&mut encoded)
        .await
        .expect("session request header");
    let payload_length = u32::from_be_bytes(encoded[55..59].try_into().unwrap()) as usize;
    encoded.resize(SESSION_HEADER_LENGTH + payload_length, 0);
    client
        .read_exact(&mut encoded[SESSION_HEADER_LENGTH..])
        .await
        .expect("session request payload");
    let SessionServerFrame::InputRequested(request) =
        decode_session_server_frame(&encoded).expect("session request decodes");
    assert_eq!(request.root_invocation_id, root);
    assert_eq!(request.call_stream, 1);

    let response = SessionClientFrame::InputLine {
        root_invocation_id: root,
        call_stream: 1,
        request_invocation_id: request.request_invocation_id,
        line: "select 1".to_owned(),
    };
    client
        .write_all(&encode_session_client_frame(&response).expect("session response encodes"))
        .await
        .expect("session response writes");
    assert_eq!(
        read_server_frame(&mut client).await,
        ServerFrame::CallCompleted { stream: 1 }
    );
    assert_eq!(
        received
            .lock()
            .expect("session input result lock")
            .as_deref(),
        Some("select 1")
    );

    client.shutdown().await.expect("client shutdown");
    server_task
        .await
        .expect("raw driver task")
        .expect("raw driver closes");
}

#[derive(Clone)]
pub(super) struct GatedDispatch {
    pub(super) release: Arc<Notify>,
    pub(super) polled: Arc<AtomicBool>,
}

impl GatedDispatch {
    pub(super) fn new() -> Self {
        Self {
            release: Arc::new(Notify::new()),
            polled: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl DispatchService for GatedDispatch {
    fn start(
        &self,
        _session: AuthenticatedSession,
        stream: u64,
        _call: RawCall,
    ) -> StartedDispatch {
        let release = Arc::clone(&self.release);
        let polled = Arc::clone(&self.polled);
        StartedDispatch {
            accepted: ServerAction::Accepted {
                stream,
                invocation: InvocationId::from_bytes([9; 16]),
            },
            started: None,
            start_gate: None,
            future: Box::pin(async move {
                polled.store(true, Ordering::SeqCst);
                release.notified().await;
                DispatchCompletion {
                    sealed_producer: None,
                    sealed_invocation: None,
                    sealed_next_event_sequence: 1,
                    sealed_next_outer_sequence: 2,
                    actions: VecDeque::from([ServerAction::Completed { stream }]),
                    cancellation: ServerAction::Cancelled { stream },
                    cancellation_token: None,
                    start_gate: None,
                    start_delivered: false,
                    terminal_delivered: false,
                    terminal_claimed: false,
                    worker_completed: false,
                    _guards: None,
                }
            }),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum GatedPreflightOutcome {
    Accepted,
    RejectedInternalFailure,
    Error,
}

#[derive(Clone)]
pub(super) struct GatedInvokePreflightDispatch {
    outcome: GatedPreflightOutcome,
    pub(super) preflight_started: Arc<Notify>,
    pub(super) preflight_release: Arc<Notify>,
    pub(super) cancellation_seen: Arc<Notify>,
    pub(super) start_invoked: Arc<AtomicBool>,
}

impl GatedInvokePreflightDispatch {
    pub(super) fn new(outcome: GatedPreflightOutcome) -> Self {
        Self {
            outcome,
            preflight_started: Arc::new(Notify::new()),
            preflight_release: Arc::new(Notify::new()),
            cancellation_seen: Arc::new(Notify::new()),
            start_invoked: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl DispatchService for GatedInvokePreflightDispatch {
    fn start(
        &self,
        _session: AuthenticatedSession,
        _stream: u64,
        _call: RawCall,
    ) -> StartedDispatch {
        panic!("sealed preflight test does not issue an ordinary raw call")
    }

    fn preflight_invoke(
        &self,
        _session: AuthenticatedSession,
        _request: orna_protocol::RetainedInvokeRequest,
        _version: RawProtocolVersion,
    ) -> InvokePreflightFuture {
        let outcome = self.outcome;
        let started = Arc::clone(&self.preflight_started);
        let release = Arc::clone(&self.preflight_release);
        Box::pin(async move {
            started.notify_one();
            release.notified().await;
            match outcome {
                GatedPreflightOutcome::Accepted => Ok(InvokePreflight::Accepted(None)),
                GatedPreflightOutcome::RejectedInternalFailure => {
                    Ok(InvokePreflight::Rejected(CallFailure::InternalFailure))
                }
                GatedPreflightOutcome::Error => Err(PostgresKernelError::DurableInvariant {
                    relation: "test",
                    record: "gated preflight".to_string(),
                    rule: "forced error",
                }),
            }
        })
    }

    fn start_invoke(
        &self,
        _session: AuthenticatedSession,
        stream: u64,
        _request: orna_protocol::RetainedInvokeRequest,
        _version: &RawProtocolVersion,
        _continuation: Option<SealedInvocationContinuation>,
    ) -> StartedDispatch {
        self.start_invoked.store(true, Ordering::SeqCst);
        StartedDispatch {
            accepted: ServerAction::Accepted {
                stream,
                invocation: InvocationId::from_bytes([0x61; 16]),
            },
            started: None,
            start_gate: None,
            future: Box::pin(async move {
                DispatchCompletion {
                    sealed_producer: None,
                    sealed_invocation: None,
                    sealed_next_event_sequence: 1,
                    sealed_next_outer_sequence: 2,
                    actions: VecDeque::from([ServerAction::Completed { stream }]),
                    cancellation: ServerAction::InvokeCancelled { stream },
                    cancellation_token: None,
                    start_gate: None,
                    start_delivered: false,
                    terminal_delivered: false,
                    terminal_claimed: false,
                    worker_completed: false,
                    _guards: None,
                }
            }),
        }
    }

    fn cancelled(&self, _stream: u64) {
        self.cancellation_seen.notify_one();
    }
}

#[tokio::test]
async fn scheduled_shutdown_task_returns_without_awaiting_cleanup() {
    let mut shutdown_tasks = JoinSet::new();
    let (started_sender, started_receiver) = oneshot::channel();
    let (release_sender, release_receiver) = oneshot::channel();
    schedule_shutdown_task(&mut shutdown_tasks, async move {
        started_sender
            .send(())
            .expect("scheduling test receiver remains live");
        release_receiver
            .await
            .expect("scheduling test release remains live");
    });

    timeout(Duration::from_millis(50), started_receiver)
        .await
        .expect("scheduled cleanup starts promptly")
        .expect("scheduled cleanup start signal");
    assert!(
        timeout(Duration::from_millis(10), shutdown_tasks.join_next())
            .await
            .is_err(),
        "cancellation scheduling must not await cleanup inline"
    );

    release_sender
        .send(())
        .expect("scheduled cleanup release remains live");
    timeout(Duration::from_millis(50), shutdown_tasks.join_next())
        .await
        .expect("scheduled cleanup joins")
        .expect("scheduled cleanup task result")
        .expect("scheduled cleanup completes successfully");
}

#[test]
fn resource_completion_channel_is_bounded_by_live_resource_limit() {
    let (sender, mut receiver) =
        mpsc::channel::<(u64, ResourceDispatchCompletion)>(RESOURCE_COMPLETION_CHANNEL_CAPACITY);
    assert_eq!(sender.max_capacity(), RESOURCE_COMPLETION_CHANNEL_CAPACITY);
    for stream_id in 0..RESOURCE_COMPLETION_CHANNEL_CAPACITY as u64 {
        sender
            .try_send((
                stream_id,
                ResourceDispatchCompletion {
                    actions: VecDeque::new(),
                    producer: None,
                    producer_waiting_bytes: None,
                    terminal_provenance: ResourceTerminalProvenance::Uncommitted,
                },
            ))
            .expect("one completion fits per live resource");
    }
    assert_eq!(sender.capacity(), 0);
    assert!(matches!(
        sender.try_send((
            RESOURCE_COMPLETION_CHANNEL_CAPACITY as u64,
            ResourceDispatchCompletion {
                actions: VecDeque::new(),
                producer: None,
                producer_waiting_bytes: None,
                terminal_provenance: ResourceTerminalProvenance::Uncommitted,
            },
        )),
        Err(mpsc::error::TrySendError::Full(_))
    ));
    for _ in 0..RESOURCE_COMPLETION_CHANNEL_CAPACITY {
        receiver
            .try_recv()
            .expect("bounded completions remain queued");
    }
}

#[test]
fn credit_starved_resource_wait_retries_idle_only_before_frame_start() {
    let now = Instant::now();
    let expired = now - FRAME_IDLE_TIMEOUT;
    let state = ResourceReadState::default();
    assert!(!resource_idle_timeout_is_retryable(
        state.is_active(),
        0,
        expired,
        now,
    ));
    state.set_active(true);
    assert!(resource_idle_timeout_is_retryable(
        state.is_active(),
        0,
        expired,
        now,
    ));
    assert!(!resource_idle_timeout_is_retryable(
        state.is_active(),
        1,
        expired,
        now,
    ));
    assert!(!resource_idle_timeout_is_retryable(
        state.is_active(),
        0,
        now + FRAME_IDLE_TIMEOUT,
        now,
    ));
}

#[test]
fn handshake_bytes_and_listener_budgets_are_exact() {
    assert_eq!(CLIENT_HELLO, *b"ORNA\x01\x00\x00\x01\x00\x00\x00\x00");
    assert_eq!(
        CLIENT_CATALOGUE_HELLO,
        *b"ORNA\x01\x00\x00\x02\x00\x00\x00\x00"
    );
    assert_eq!(
        CLIENT_ACTIVE_HELLO,
        *b"ORNA\x01\x00\x00\x03\x00\x00\x00\x00"
    );
    assert_eq!(
        CLIENT_REGISTERED_HELLO,
        *b"ORNA\x01\x00\x00\x04\x00\x00\x00\x00"
    );
    assert_eq!(SERVER_ACK, *b"ORNA\x81\x00\x00\x01\x00\x00\x00\x00");
    assert_eq!(
        SERVER_CATALOGUE_ACK,
        *b"ORNA\x81\x00\x00\x02\x00\x00\x00\x00"
    );
    assert_eq!(SERVER_ACTIVE_ACK, *b"ORNA\x81\x00\x00\x03\x00\x00\x00\x00");
    assert_eq!(
        SERVER_REGISTERED_ACK,
        *b"ORNA\x81\x00\x00\x04\x00\x00\x00\x00"
    );
    assert_eq!(
        requested_protocol(&CLIENT_ACTIVE_HELLO),
        Some(RequestedProtocol::Active)
    );
    assert_eq!(
        requested_protocol(&CLIENT_REGISTERED_HELLO),
        Some(RequestedProtocol::Registered)
    );
    let resources = LocalRawSocketResources::new();
    let payload = resources
        .reserve_payload(SHARED_PAYLOAD_BYTES)
        .expect("complete payload budget");
    assert!(matches!(
        resources.reserve_payload(1),
        Err(LocalRawSocketError::PayloadCapacity)
    ));
    drop(payload);
    assert!(resources.reserve_payload(1).is_ok());

    let operations: Vec<_> = (0..KERNEL_OPERATION_LIMIT)
        .map(|_| {
            resources
                .reserve_kernel_operation()
                .expect("operation permit")
        })
        .collect();
    assert!(matches!(
        resources.reserve_kernel_operation(),
        Err(LocalRawSocketError::KernelCapacity)
    ));
    drop(operations);
    assert!(resources.reserve_kernel_operation().is_ok());
}

#[tokio::test]
async fn queued_resource_permit_waiter_completes_when_cancelled() {
    let resources = LocalRawSocketResources::new();
    let held: Vec<_> = (0..KERNEL_OPERATION_LIMIT)
        .map(|_| {
            resources
                .reserve_kernel_operation()
                .expect("operation permit")
        })
        .collect();
    let cancellation = ResourceCancellation::new();
    let waiter = tokio::spawn({
        let resources = resources.clone();
        let cancellation = cancellation.clone();
        async move { resources.acquire_kernel_operation(&cancellation).await }
    });

    tokio::task::yield_now().await;
    assert_eq!(resources.kernel_operations.available_permits(), 0);
    assert!(cancellation.request_cancel());
    let permit = timeout(Duration::from_millis(50), waiter)
        .await
        .expect("queued resource waiter joins after cancellation")
        .expect("queued resource waiter task");
    assert!(permit.is_none());
    assert_eq!(resources.kernel_operations.available_permits(), 0);
    drop(held);
    assert_eq!(
        resources.kernel_operations.available_permits(),
        KERNEL_OPERATION_LIMIT
    );
}

#[tokio::test]
async fn repeated_cancel_after_terminal_completion_uses_protocol_tombstone() {
    let (version, revision) = constructed_test_version();
    let mut request = resource_request(revision);
    request.resource_kind = ResourceKind::Stream;
    let cancel = ResourceCancel {
        stream_id: request.stream_id,
        request_id: request.request_id,
        reason: ResourceCancellationCode::ClientRequested,
    };
    let mut connection = ResourceProtocolConnection::new();
    connection
        .receive(ResourceClientFrame::Request(request.clone()))
        .unwrap();
    let (server, _client) = UnixStream::pair().unwrap();
    let (_reader, mut writer) = server.into_split();
    let (completion_sender, mut completion_receiver) =
        mpsc::channel::<(u64, ResourceDispatchCompletion)>(RESOURCE_COMPLETION_CHANNEL_CAPACITY);
    let resources = LocalRawSocketResources::new();
    let (_shutdown_sender, mut shutdown) = watch::channel(false);
    let mut pending = BTreeMap::from([(
        request.stream_id,
        ResourceDispatchCompletion {
            actions: resource_actions(&version, &request, Vec::new()),
            producer: None,
            producer_waiting_bytes: None,
            terminal_provenance: ResourceTerminalProvenance::Uncommitted,
        },
    )]);
    let mut cancelled = BTreeMap::new();
    let mut tasks = BTreeMap::new();
    let mut producer_shutdown = JoinSet::new();
    let mut requests = BTreeMap::from([(request.stream_id, request.clone())]);
    assert!(
        flush_resource_pending(
            &version,
            &mut connection,
            &mut pending,
            &mut requests,
            &mut tasks,
            &mut producer_shutdown,
            &mut writer,
            &mut shutdown,
        )
        .await
        .unwrap()
    );
    assert!(pending.is_empty());
    // Keep the request entry to model the local bookkeeping window after
    // the protocol has committed its terminal tombstone.
    requests.insert(request.stream_id, request.clone());
    let mut live_request = request.clone();
    live_request.stream_id = 2;
    live_request.request_id = InvocationId::from_bytes([0x22; 16]);
    connection
        .receive(ResourceClientFrame::Request(live_request))
        .unwrap();

    for _ in 0..2 {
        handle_resource_frame(
            ResourceClientFrame::Cancel(cancel),
            PayloadReservation { _permit: None },
            &ResourceDispatch,
            &test_session(),
            &version,
            &resources,
            &mut connection,
            &mut pending,
            &mut cancelled,
            &mut tasks,
            &mut producer_shutdown,
            &mut requests,
            &completion_sender,
            &mut completion_receiver,
            &mut writer,
            &mut shutdown,
        )
        .await
        .expect("late cancellation is idempotently dropped");
    }
    assert_eq!(connection.live_resources(), 1);

    let unknown = ResourceCancel {
        stream_id: request.stream_id + 2,
        request_id: InvocationId::from_bytes([0x44; 16]),
        reason: ResourceCancellationCode::ClientRequested,
    };
    let error = handle_resource_frame(
        ResourceClientFrame::Cancel(unknown),
        PayloadReservation { _permit: None },
        &ResourceDispatch,
        &test_session(),
        &version,
        &resources,
        &mut connection,
        &mut pending,
        &mut cancelled,
        &mut tasks,
        &mut producer_shutdown,
        &mut requests,
        &completion_sender,
        &mut completion_receiver,
        &mut writer,
        &mut shutdown,
    )
    .await
    .expect_err("unknown resource cancellation must fail");
    assert!(matches!(
        error,
        LocalRawSocketError::ResourceConnection {
            source: ResourceConnectionError::UnknownStream { stream_id: 3 }
        }
    ));
    assert_eq!(connection.live_resources(), 1);
}

#[tokio::test]
async fn constructed_resource_request_delivers_a_scalar_result() {
    let (version, revision) = constructed_test_version();
    let (active, registry) = match &version {
        RawProtocolVersion::Constructed(active, registry) => (active.clone(), registry.clone()),
        _ => unreachable!("constructed test version"),
    };
    let mut request = resource_request(revision);
    request.byte_window = resource_value_byte_count(&version, &RuntimeValue::Integer(7))
        .expect("scalar value byte count") as u64;
    let request_id = request.request_id;
    let encoded =
        encode_resource_client_frame(&active, &registry, &ResourceClientFrame::Request(request))
            .unwrap();
    let resources = LocalRawSocketResources::new();
    let (_shutdown_sender, shutdown) = watch::channel(false);
    let (server, mut client) = UnixStream::pair().unwrap();
    let server_task = tokio::spawn(drive_versioned_authenticated_stream_until_shutdown(
        ResourceDispatch,
        test_session(),
        version,
        server,
        resources,
        shutdown,
    ));

    client.write_all(&encoded).await.unwrap();
    assert!(matches!(
        read_resource_server_frame(&mut client, &active, &registry).await,
        ResourceServerFrame::Accepted(_)
    ));
    assert!(matches!(
        read_resource_server_frame(&mut client, &active, &registry).await,
        ResourceServerFrame::Values(frame)
            if frame.values == vec![RuntimeValue::Integer(7)]
                && frame.item_count == 1
                && frame.batch_sequence == 0
    ));
    assert!(matches!(
        read_resource_server_frame(&mut client, &active, &registry).await,
        ResourceServerFrame::Completed(frame)
            if frame.final_batch_sequence == 0 && frame.total_items == 1
    ));
    let cancel = encode_resource_client_frame(
        &active,
        &registry,
        &ResourceClientFrame::Cancel(ResourceCancel {
            stream_id: 1,
            request_id,
            reason: ResourceCancellationCode::ClientRequested,
        }),
    )
    .unwrap();
    client.write_all(&cancel).await.unwrap();
    assert!(
        timeout(
            Duration::from_millis(50),
            read_resource_server_frame(&mut client, &active, &registry),
        )
        .await
        .is_err()
    );

    client.shutdown().await.unwrap();
    server_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn direct_resource_failure_matrix_is_single_terminal_and_releases_state() {
    let cases = [
        (
            DirectResourceFailureKind::SecurityDenied,
            CallFailure::ExecuteDenied,
        ),
        (
            DirectResourceFailureKind::TargetUnavailable,
            CallFailure::TargetUnavailable,
        ),
        (
            DirectResourceFailureKind::ProducerFailure,
            CallFailure::InternalFailure,
        ),
    ];

    for (index, (kind, expected_failure)) in cases.into_iter().enumerate() {
        let (version, revision) = constructed_test_version();
        let (active, registry) = match &version {
            RawProtocolVersion::Constructed(active, registry) => (active.clone(), registry.clone()),
            _ => unreachable!("constructed test version"),
        };
        let resources = LocalRawSocketResources::new();
        let (server, mut client) = UnixStream::pair().unwrap();
        let (_shutdown_sender, shutdown) = watch::channel(false);
        let authenticated_terminal = Arc::new(AtomicUsize::new(0));
        let server_task = tokio::spawn(drive_versioned_authenticated_stream_until_shutdown(
            DirectResourceFailureDispatch {
                kind,
                authenticated_terminal: Arc::clone(&authenticated_terminal),
            },
            test_session(),
            version,
            server,
            resources.clone(),
            shutdown,
        ));

        let stream_id = index as u64 + 1;
        let mut request = resource_request(revision);
        request.stream_id = stream_id;
        request.request_id = InvocationId::from_bytes([0x50 + index as u8; 16]);
        client
            .write_all(
                &encode_resource_client_frame(
                    &active,
                    &registry,
                    &ResourceClientFrame::Request(request.clone()),
                )
                .unwrap(),
            )
            .await
            .unwrap();

        assert!(matches!(
            read_resource_server_frame(&mut client, &active, &registry).await,
            ResourceServerFrame::Failed(frame)
                if frame.stream_id == stream_id
                    && frame.request_id == request.request_id
                    && frame.failure == expected_failure
        ));

        let cancel = encode_resource_client_frame(
            &active,
            &registry,
            &ResourceClientFrame::Cancel(ResourceCancel {
                stream_id,
                request_id: request.request_id,
                reason: ResourceCancellationCode::ClientRequested,
            }),
        )
        .unwrap();
        client.write_all(&cancel).await.unwrap();
        client.write_all(&cancel).await.unwrap();
        assert!(
            timeout(
                Duration::from_millis(50),
                read_resource_server_frame(&mut client, &active, &registry),
            )
            .await
            .is_err(),
            "direct resource failure must not be replaced by cancellation",
        );

        client.shutdown().await.unwrap();
        server_task.await.unwrap().unwrap();
        assert_eq!(
            authenticated_terminal.load(Ordering::SeqCst),
            1,
            "resource failure must record one authenticated terminal provenance",
        );
        assert_eq!(
            resources.kernel_operations.available_permits(),
            KERNEL_OPERATION_LIMIT,
            "resource failure leaked its kernel-operation permit",
        );
        assert!(
            resources.reserve_payload(SHARED_PAYLOAD_BYTES).is_ok(),
            "resource failure leaked its payload reservation",
        );
    }
}

#[tokio::test]
async fn raw_socket_malformed_pending_resource_values_fails_only_that_stream() {
    let (version, revision) = constructed_test_version();
    let (active, registry) = match &version {
        RawProtocolVersion::Constructed(active, registry) => (active.clone(), registry.clone()),
        _ => unreachable!("constructed test version"),
    };
    let (server, mut client) = UnixStream::pair().unwrap();
    let (_shutdown_sender, shutdown) = watch::channel(false);
    let server_task = tokio::spawn(drive_versioned_authenticated_stream_until_shutdown(
        MalformedPendingResourceDispatch,
        test_session(),
        version,
        server,
        LocalRawSocketResources::new(),
        shutdown,
    ));

    let request = resource_request(revision);
    let request_id = request.request_id;
    client
        .write_all(
            &encode_resource_client_frame(
                &active,
                &registry,
                &ResourceClientFrame::Request(request),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(matches!(
        read_resource_server_frame(&mut client, &active, &registry).await,
        ResourceServerFrame::Accepted(frame)
            if frame.stream_id == 1 && frame.request_id == request_id
    ));
    assert!(matches!(
        read_resource_server_frame(&mut client, &active, &registry).await,
        ResourceServerFrame::Failed(frame)
            if frame.stream_id == 1
                && frame.request_id == request_id
                && frame.failure == CallFailure::InternalFailure
    ));

    let cancel = encode_resource_client_frame(
        &active,
        &registry,
        &ResourceClientFrame::Cancel(ResourceCancel {
            stream_id: 1,
            request_id,
            reason: ResourceCancellationCode::ClientRequested,
        }),
    )
    .unwrap();
    client.write_all(&cancel).await.unwrap();
    assert!(
        timeout(
            Duration::from_millis(50),
            read_resource_server_frame(&mut client, &active, &registry),
        )
        .await
        .is_err(),
        "malformed resource request has one terminal frame and no late replacement"
    );

    for frame in [
        ClientFrame::CallRawStart {
            stream: 1,
            function: FUNCTION,
        },
        ClientFrame::CallArgumentsComplete { stream: 1 },
    ] {
        client
            .write_all(&encode_constructed_client_frame(&active, &registry, &frame).unwrap())
            .await
            .unwrap();
    }
    assert!(matches!(
        read_constructed_server_frame(&mut client, &active, &registry).await,
        ServerFrame::CallAccepted { stream: 1, .. }
    ));
    assert_eq!(
        read_constructed_server_frame(&mut client, &active, &registry).await,
        ServerFrame::CallCompleted { stream: 1 }
    );

    let mut unrelated = resource_request(revision);
    unrelated.stream_id = 2;
    unrelated.request_id = InvocationId::from_bytes([0x22; 16]);
    unrelated.resource_kind = ResourceKind::Stream;
    client
        .write_all(
            &encode_resource_client_frame(
                &active,
                &registry,
                &ResourceClientFrame::Request(unrelated.clone()),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(matches!(
        read_resource_server_frame(&mut client, &active, &registry).await,
        ResourceServerFrame::Accepted(frame)
            if frame.stream_id == unrelated.stream_id
                && frame.request_id == unrelated.request_id
    ));
    assert!(matches!(
        read_resource_server_frame(&mut client, &active, &registry).await,
        ResourceServerFrame::Values(frame)
            if frame.stream_id == unrelated.stream_id
                && frame.values == vec![RuntimeValue::Integer(7)]
    ));
    assert!(matches!(
        read_resource_server_frame(&mut client, &active, &registry).await,
        ResourceServerFrame::Completed(frame)
            if frame.stream_id == unrelated.stream_id
                && frame.request_id == unrelated.request_id
    ));

    client.shutdown().await.unwrap();
    server_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn invalid_scalar_window_update_preserves_deliverable_completion() {
    let (version, revision) = constructed_test_version();
    let request = resource_request(revision);
    let (server, mut client) = UnixStream::pair().unwrap();
    let (_reader, mut writer) = server.into_split();
    let (completion_sender, mut completion_receiver) =
        mpsc::channel::<(u64, ResourceDispatchCompletion)>(RESOURCE_COMPLETION_CHANNEL_CAPACITY);
    let mut connection = ResourceProtocolConnection::new();
    connection
        .receive(ResourceClientFrame::Request(request.clone()))
        .unwrap();
    let mut pending = BTreeMap::from([(
        request.stream_id,
        ResourceDispatchCompletion {
            actions: resource_actions(&version, &request, vec![RuntimeValue::Integer(7)]),
            producer: None,
            producer_waiting_bytes: None,
            terminal_provenance: ResourceTerminalProvenance::Authenticated,
        },
    )]);
    let mut cancelled = BTreeMap::new();
    let mut tasks = BTreeMap::new();
    let mut producer_shutdown = JoinSet::new();
    let mut requests = BTreeMap::from([(request.stream_id, request.clone())]);
    let resources = LocalRawSocketResources::new();
    let (_shutdown_sender, mut shutdown) = watch::channel(false);

    handle_resource_frame(
        ResourceClientFrame::WindowUpdate(ResourceWindowUpdate {
            stream_id: request.stream_id,
            request_id: request.request_id,
            add_items: 1,
            add_bytes: 1,
        }),
        PayloadReservation { _permit: None },
        &ResourceDispatch,
        &test_session(),
        &version,
        &resources,
        &mut connection,
        &mut pending,
        &mut cancelled,
        &mut tasks,
        &mut producer_shutdown,
        &mut requests,
        &completion_sender,
        &mut completion_receiver,
        &mut writer,
        &mut shutdown,
    )
    .await
    .unwrap();

    assert!(matches!(
        pending
            .get(&request.stream_id)
            .and_then(|completion| completion.actions.front()),
        Some(ResourceServerFrame::Accepted(frame))
            if frame.request_id == request.request_id
    ));
    assert!(
        flush_resource_pending(
            &version,
            &mut connection,
            &mut pending,
            &mut requests,
            &mut tasks,
            &mut producer_shutdown,
            &mut writer,
            &mut shutdown,
        )
        .await
        .unwrap()
    );
    let (active, registry) = match &version {
        RawProtocolVersion::Constructed(active, registry) => (active.as_ref(), registry.as_ref()),
        _ => unreachable!("constructed test version"),
    };
    assert!(matches!(
        read_resource_server_frame(&mut client, active, registry).await,
        ResourceServerFrame::Accepted(frame)
            if frame.stream_id == request.stream_id && frame.request_id == request.request_id
    ));
    assert!(matches!(
        read_resource_server_frame(&mut client, active, registry).await,
        ResourceServerFrame::Values(frame)
            if frame.stream_id == request.stream_id
                && frame.values == vec![RuntimeValue::Integer(7)]
    ));
    assert!(matches!(
        read_resource_server_frame(&mut client, active, registry).await,
        ResourceServerFrame::Completed(frame)
            if frame.stream_id == request.stream_id
                && frame.request_id == request.request_id
                && frame.total_items == 1
    ));
}

#[tokio::test]
async fn invalid_scalar_window_update_only_terminates_its_resource_stream() {
    let (version, revision) = constructed_test_version();
    let (active, registry) = match &version {
        RawProtocolVersion::Constructed(active, registry) => (active.clone(), registry.clone()),
        _ => unreachable!("constructed test version"),
    };
    let started = Arc::new(Notify::new());
    let dispatcher = MixedResourceDispatch {
        started: Arc::clone(&started),
    };
    let (server, mut client) = UnixStream::pair().unwrap();
    let (_shutdown_sender, shutdown) = watch::channel(false);
    let server_task = tokio::spawn(drive_versioned_authenticated_stream_until_shutdown(
        dispatcher,
        test_session(),
        version,
        server,
        LocalRawSocketResources::new(),
        shutdown,
    ));

    let scalar = resource_request(revision);
    let scalar_request_id = scalar.request_id;
    client
        .write_all(
            &encode_resource_client_frame(
                &active,
                &registry,
                &ResourceClientFrame::Request(scalar),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    started.notified().await;

    client
        .write_all(
            &encode_resource_client_frame(
                &active,
                &registry,
                &ResourceClientFrame::WindowUpdate(ResourceWindowUpdate {
                    stream_id: 1,
                    request_id: scalar_request_id,
                    add_items: 1,
                    add_bytes: 1,
                }),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let scalar_failure = read_resource_server_frame(&mut client, &active, &registry).await;
    assert!(
        matches!(
            scalar_failure,
            ResourceServerFrame::Failed(frame)
                if frame.stream_id == 1
                    && frame.request_id == scalar_request_id
                    && frame.failure == orna_protocol::CallFailure::InternalFailure
        ),
        "unexpected scalar failure frame: {scalar_failure:?}"
    );

    let mut unrelated = resource_request(revision);
    unrelated.stream_id = 2;
    unrelated.request_id = InvocationId::from_bytes([0x22; 16]);
    unrelated.resource_kind = ResourceKind::Stream;
    unrelated.item_window = 1;
    let value_bytes =
        orna_protocol::encode_constructed_value(&active, &registry, &RuntimeValue::Integer(7))
            .unwrap();
    unrelated.byte_window = value_bytes.len() as u64;
    client
        .write_all(
            &encode_resource_client_frame(
                &active,
                &registry,
                &ResourceClientFrame::Request(unrelated.clone()),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(matches!(
        read_resource_server_frame(&mut client, &active, &registry).await,
        ResourceServerFrame::Accepted(frame)
            if frame.stream_id == unrelated.stream_id
                && frame.request_id == unrelated.request_id
    ));
    assert!(matches!(
        read_resource_server_frame(&mut client, &active, &registry).await,
        ResourceServerFrame::Values(frame)
            if frame.stream_id == unrelated.stream_id
                && frame.values == vec![RuntimeValue::Integer(7)]
    ));

    client
        .write_all(
            &encode_resource_client_frame(
                &active,
                &registry,
                &ResourceClientFrame::WindowUpdate(ResourceWindowUpdate {
                    stream_id: unrelated.stream_id,
                    request_id: unrelated.request_id,
                    add_items: 1,
                    add_bytes: value_bytes.len() as u64,
                }),
            )
            .unwrap(),
        )
        .await
        .unwrap();

    assert!(matches!(
        read_resource_server_frame(&mut client, &active, &registry).await,
        ResourceServerFrame::Values(frame)
            if frame.stream_id == unrelated.stream_id
                && frame.values == vec![RuntimeValue::Integer(8)]
    ));
    assert!(matches!(
        read_resource_server_frame(&mut client, &active, &registry).await,
        ResourceServerFrame::Completed(frame)
            if frame.stream_id == unrelated.stream_id
                && frame.total_items == 2
    ));

    client.shutdown().await.unwrap();
    server_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn constructed_resource_stream_resumes_after_window_update() {
    let (version, revision) = constructed_test_version();
    let (active, registry) = match &version {
        RawProtocolVersion::Constructed(active, registry) => (active.clone(), registry.clone()),
        _ => unreachable!("constructed test version"),
    };
    let value_bytes =
        orna_protocol::encode_constructed_value(&active, &registry, &RuntimeValue::Integer(7))
            .unwrap();
    let mut request = resource_request(revision);
    request.resource_kind = ResourceKind::Stream;
    request.item_window = 1;
    request.byte_window = value_bytes.len() as u64;
    let request_id = request.request_id;
    let encoded =
        encode_resource_client_frame(&active, &registry, &ResourceClientFrame::Request(request))
            .unwrap();
    let (server, mut client) = UnixStream::pair().unwrap();
    let (_shutdown_sender, shutdown) = watch::channel(false);
    let server_task = tokio::spawn(drive_versioned_authenticated_stream_until_shutdown(
        MultiValueResourceDispatch,
        test_session(),
        version,
        server,
        LocalRawSocketResources::new(),
        shutdown,
    ));

    client.write_all(&encoded).await.unwrap();
    assert!(matches!(
        read_resource_server_frame(&mut client, &active, &registry).await,
        ResourceServerFrame::Accepted(_)
    ));
    assert!(matches!(
        read_resource_server_frame(&mut client, &active, &registry).await,
        ResourceServerFrame::Values(frame)
            if frame.values == vec![RuntimeValue::Integer(7)]
                && frame.item_count == 1
                && frame.byte_count == value_bytes.len() as u32
    ));
    assert!(
        timeout(
            Duration::from_millis(50),
            read_resource_server_frame(&mut client, &active, &registry),
        )
        .await
        .is_err()
    );

    let update = encode_resource_client_frame(
        &active,
        &registry,
        &ResourceClientFrame::WindowUpdate(ResourceWindowUpdate {
            stream_id: 1,
            request_id,
            add_items: 1,
            add_bytes: value_bytes.len() as u64,
        }),
    )
    .unwrap();
    client.write_all(&update).await.unwrap();
    assert!(matches!(
        read_resource_server_frame(&mut client, &active, &registry).await,
        ResourceServerFrame::Values(frame)
            if frame.values == vec![RuntimeValue::Integer(8)]
                && frame.batch_sequence == 1
                && frame.item_count == 1
                && frame.byte_count == value_bytes.len() as u32
    ));
    assert!(matches!(
        read_resource_server_frame(&mut client, &active, &registry).await,
        ResourceServerFrame::Completed(frame)
            if frame.final_batch_sequence == 1 && frame.total_items == 2
    ));

    client.shutdown().await.unwrap();
    server_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn constructed_resource_stream_queued_completion_wins_over_credit_blocked_cancellation() {
    let (version, revision) = constructed_test_version();
    let (active, registry) = match &version {
        RawProtocolVersion::Constructed(active, registry) => (active.clone(), registry.clone()),
        _ => unreachable!("constructed test version"),
    };
    let value_bytes =
        orna_protocol::encode_constructed_value(&active, &registry, &RuntimeValue::Integer(7))
            .unwrap();
    let mut request = resource_request(revision);
    request.resource_kind = ResourceKind::Stream;
    request.item_window = 1;
    request.byte_window = value_bytes.len() as u64;
    let request_id = request.request_id;
    let encoded =
        encode_resource_client_frame(&active, &registry, &ResourceClientFrame::Request(request))
            .unwrap();
    let (server, mut client) = UnixStream::pair().unwrap();
    let (_shutdown_sender, shutdown) = watch::channel(false);
    let server_task = tokio::spawn(drive_versioned_authenticated_stream_until_shutdown(
        MultiValueResourceDispatch,
        test_session(),
        version,
        server,
        LocalRawSocketResources::new(),
        shutdown,
    ));

    client.write_all(&encoded).await.unwrap();
    assert!(matches!(
        read_resource_server_frame(&mut client, &active, &registry).await,
        ResourceServerFrame::Accepted(_)
    ));
    assert!(matches!(
        read_resource_server_frame(&mut client, &active, &registry).await,
        ResourceServerFrame::Values(frame)
            if frame.values == vec![RuntimeValue::Integer(7)]
                && frame.item_count == 1
                && frame.byte_count == value_bytes.len() as u32
    ));
    assert!(
        timeout(
            Duration::from_millis(50),
            read_resource_server_frame(&mut client, &active, &registry),
        )
        .await
        .is_err()
    );

    let cancel = encode_resource_client_frame(
        &active,
        &registry,
        &ResourceClientFrame::Cancel(ResourceCancel {
            stream_id: 1,
            request_id,
            reason: ResourceCancellationCode::ClientRequested,
        }),
    )
    .unwrap();
    client.write_all(&cancel).await.unwrap();
    assert!(
        timeout(
            Duration::from_millis(50),
            read_resource_server_frame(&mut client, &active, &registry),
        )
        .await
        .is_err(),
        "credit-blocked values must not produce a replacement cancellation frame"
    );

    let update = encode_resource_client_frame(
        &active,
        &registry,
        &ResourceClientFrame::WindowUpdate(ResourceWindowUpdate {
            stream_id: 1,
            request_id,
            add_items: 1,
            add_bytes: value_bytes.len() as u64,
        }),
    )
    .unwrap();
    client.write_all(&update).await.unwrap();
    assert!(matches!(
        read_resource_server_frame(&mut client, &active, &registry).await,
        ResourceServerFrame::Values(frame)
            if frame.values == vec![RuntimeValue::Integer(8)]
                && frame.batch_sequence == 1
    ));
    assert!(matches!(
        read_resource_server_frame(&mut client, &active, &registry).await,
        ResourceServerFrame::Completed(frame)
            if frame.final_batch_sequence == 1 && frame.total_items == 2
    ));

    client.shutdown().await.unwrap();
    server_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn constructed_resource_cancellation_wins_before_dispatch_completion() {
    let (version, revision) = constructed_test_version();
    let (active, registry) = match &version {
        RawProtocolVersion::Constructed(active, registry) => (active.clone(), registry.clone()),
        _ => unreachable!("constructed test version"),
    };
    let request = resource_request(revision);
    let request_id = request.request_id;
    let encoded =
        encode_resource_client_frame(&active, &registry, &ResourceClientFrame::Request(request))
            .unwrap();
    let started = Arc::new(Notify::new());
    let cancelled = Arc::new(AtomicBool::new(false));
    let (server, mut client) = UnixStream::pair().unwrap();
    let (_shutdown_sender, shutdown) = watch::channel(false);
    let server_task = tokio::spawn(drive_versioned_authenticated_stream_until_shutdown(
        BlockingResourceDispatch {
            started: Arc::clone(&started),
            cancelled: Arc::clone(&cancelled),
        },
        test_session(),
        version,
        server,
        LocalRawSocketResources::new(),
        shutdown,
    ));

    client.write_all(&encoded).await.unwrap();
    started.notified().await;
    let cancel = encode_resource_client_frame(
        &active,
        &registry,
        &ResourceClientFrame::Cancel(ResourceCancel {
            stream_id: 1,
            request_id,
            reason: ResourceCancellationCode::ClientRequested,
        }),
    )
    .unwrap();
    client.write_all(&cancel).await.unwrap();
    assert!(matches!(
        read_resource_server_frame(&mut client, &active, &registry).await,
        ResourceServerFrame::Cancelled(frame)
            if frame.stream_id == 1
                && frame.request_id == request_id
                && frame.reason == ResourceCancellationCode::ClientRequested
    ));
    assert!(cancelled.load(Ordering::SeqCst));
    assert!(
        timeout(
            Duration::from_millis(50),
            read_resource_server_frame(&mut client, &active, &registry),
        )
        .await
        .is_err(),
        "cancellation must emit exactly one terminal frame"
    );
    client.write_all(&cancel).await.unwrap();
    assert!(
        timeout(
            Duration::from_millis(50),
            read_resource_server_frame(&mut client, &active, &registry),
        )
        .await
        .is_err(),
        "repeated cancellation must remain idempotent"
    );

    client.shutdown().await.unwrap();
    server_task.await.unwrap().unwrap();
}

async fn pre_accept_cancel_emits_one_terminal(dispatcher: PreAcceptResourceDispatch) {
    let (version, revision) = constructed_test_version();
    let (active, registry) = match &version {
        RawProtocolVersion::Constructed(active, registry) => (active.clone(), registry.clone()),
        _ => unreachable!("constructed test version"),
    };
    let request = resource_request(revision);
    let (server, mut client) = UnixStream::pair().unwrap();
    let (_reader, mut writer) = server.into_split();
    let (completion_sender, mut completion_receiver) =
        mpsc::channel::<(u64, ResourceDispatchCompletion)>(RESOURCE_COMPLETION_CHANNEL_CAPACITY);
    let mut connection = ResourceProtocolConnection::new();
    let mut pending = BTreeMap::new();
    let mut cancelled = BTreeMap::new();
    let mut tasks: BTreeMap<u64, ResourceTask> = BTreeMap::new();
    let mut producer_shutdown = JoinSet::new();
    let mut requests = BTreeMap::new();
    let resources = LocalRawSocketResources::new();
    let (_shutdown_sender, mut shutdown) = watch::channel(false);

    handle_resource_frame(
        ResourceClientFrame::Request(request.clone()),
        PayloadReservation { _permit: None },
        &dispatcher,
        &test_session(),
        &version,
        &resources,
        &mut connection,
        &mut pending,
        &mut cancelled,
        &mut tasks,
        &mut producer_shutdown,
        &mut requests,
        &completion_sender,
        &mut completion_receiver,
        &mut writer,
        &mut shutdown,
    )
    .await
    .unwrap();
    assert!(requests.contains_key(&request.stream_id));
    assert!(matches!(
        pending
            .get(&request.stream_id)
            .and_then(|completion| completion.actions.front()),
        Some(ResourceServerFrame::Failed(frame))
            if frame.request_id == request.request_id
                && frame.target_revision == request.target_revision
                && frame.failure == CallFailure::InternalFailure
    ));

    let cancel = ResourceCancel {
        stream_id: request.stream_id,
        request_id: request.request_id,
        reason: ResourceCancellationCode::ClientRequested,
    };
    handle_resource_frame(
        ResourceClientFrame::Cancel(cancel),
        PayloadReservation { _permit: None },
        &dispatcher,
        &test_session(),
        &version,
        &resources,
        &mut connection,
        &mut pending,
        &mut cancelled,
        &mut tasks,
        &mut producer_shutdown,
        &mut requests,
        &completion_sender,
        &mut completion_receiver,
        &mut writer,
        &mut shutdown,
    )
    .await
    .unwrap();
    assert!(cancelled.is_empty());
    assert!(matches!(
        pending
            .get(&request.stream_id)
            .and_then(|completion| completion.actions.front()),
        Some(ResourceServerFrame::Cancelled(frame))
            if frame.request_id == request.request_id
                && frame.target_revision == request.target_revision
                && frame.reason == ResourceCancellationCode::ClientRequested
    ));

    assert!(
        flush_resource_pending(
            &version,
            &mut connection,
            &mut pending,
            &mut requests,
            &mut tasks,
            &mut producer_shutdown,
            &mut writer,
            &mut shutdown,
        )
        .await
        .unwrap()
    );
    assert!(pending.is_empty());
    assert!(requests.is_empty());
    assert!(tasks.is_empty());
    assert!(cancelled.is_empty());
    assert_eq!(connection.live_resources(), 0);
    assert!(matches!(
        read_resource_server_frame(&mut client, &active, &registry).await,
        ResourceServerFrame::Cancelled(frame)
            if frame.stream_id == request.stream_id
                && frame.request_id == request.request_id
                && frame.target_revision == request.target_revision
                && frame.reason == ResourceCancellationCode::ClientRequested
    ));
    assert!(
        timeout(
            Duration::from_millis(50),
            read_resource_server_frame(&mut client, &active, &registry),
        )
        .await
        .is_err(),
        "pre-accept cancellation must emit exactly one terminal frame",
    );
}

#[tokio::test]
async fn authorization_denied_resource_cancel_before_accept_emits_one_terminal() {
    pre_accept_cancel_emits_one_terminal(PreAcceptResourceDispatch {
        authorized: false,
        resource_start_calls: Arc::new(AtomicUsize::new(0)),
    })
    .await;
}

#[tokio::test]
async fn start_resource_none_cancel_before_accept_emits_one_terminal() {
    pre_accept_cancel_emits_one_terminal(PreAcceptResourceDispatch {
        authorized: true,
        resource_start_calls: Arc::new(AtomicUsize::new(0)),
    })
    .await;
}

#[tokio::test]
async fn authorization_denied_resource_request_does_not_reserve_or_start() {
    let (version, revision) = constructed_test_version();
    let (active, registry) = match &version {
        RawProtocolVersion::Constructed(active, registry) => (active.clone(), registry.clone()),
        _ => unreachable!("constructed test version"),
    };
    let request = resource_request(revision);
    let (server, mut client) = UnixStream::pair().unwrap();
    let (_reader, mut writer) = server.into_split();
    let (completion_sender, mut completion_receiver) =
        mpsc::channel::<(u64, ResourceDispatchCompletion)>(RESOURCE_COMPLETION_CHANNEL_CAPACITY);
    let mut connection = ResourceProtocolConnection::new();
    let mut pending = BTreeMap::new();
    let mut cancelled = BTreeMap::new();
    let mut tasks: BTreeMap<u64, ResourceTask> = BTreeMap::new();
    let mut producer_shutdown = JoinSet::new();
    let mut requests = BTreeMap::new();
    let resources = LocalRawSocketResources::new();
    let kernel_operations_before = resources.kernel_operations.available_permits();
    let (_shutdown_sender, mut shutdown) = watch::channel(false);
    let resource_start_calls = Arc::new(AtomicUsize::new(0));
    let dispatcher = PreAcceptResourceDispatch {
        authorized: false,
        resource_start_calls: Arc::clone(&resource_start_calls),
    };

    handle_resource_frame(
        ResourceClientFrame::Request(request.clone()),
        PayloadReservation { _permit: None },
        &dispatcher,
        &test_session(),
        &version,
        &resources,
        &mut connection,
        &mut pending,
        &mut cancelled,
        &mut tasks,
        &mut producer_shutdown,
        &mut requests,
        &completion_sender,
        &mut completion_receiver,
        &mut writer,
        &mut shutdown,
    )
    .await
    .unwrap();

    assert_eq!(resource_start_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        resources.kernel_operations.available_permits(),
        kernel_operations_before,
        "authorization denial must not reserve a kernel operation",
    );
    assert!(tasks.is_empty());
    assert!(producer_shutdown.is_empty());
    assert!(cancelled.is_empty());
    assert_eq!(connection.live_resources(), 1);
    assert!(matches!(
        pending
            .get(&request.stream_id)
            .and_then(|completion| completion.actions.front()),
        Some(ResourceServerFrame::Failed(frame))
            if frame.stream_id == request.stream_id
                && frame.request_id == request.request_id
                && frame.target_revision == request.target_revision
                && frame.failure == CallFailure::InternalFailure
    ));

    assert!(
        flush_resource_pending(
            &version,
            &mut connection,
            &mut pending,
            &mut requests,
            &mut tasks,
            &mut producer_shutdown,
            &mut writer,
            &mut shutdown,
        )
        .await
        .unwrap()
    );
    assert!(pending.is_empty());
    assert!(requests.is_empty());
    assert!(tasks.is_empty());
    assert!(cancelled.is_empty());
    assert_eq!(connection.live_resources(), 0);
    assert_eq!(
        resources.kernel_operations.available_permits(),
        kernel_operations_before,
    );
    assert!(matches!(
        read_resource_server_frame(&mut client, &active, &registry).await,
        ResourceServerFrame::Failed(frame)
            if frame.stream_id == request.stream_id
                && frame.request_id == request.request_id
                && frame.target_revision == request.target_revision
                && frame.failure == CallFailure::InternalFailure
    ));
}

#[tokio::test]
async fn queued_resource_completion_wins_over_cancellation() {
    let (version, revision) = constructed_test_version();
    let (active, registry) = match &version {
        RawProtocolVersion::Constructed(active, registry) => (active.clone(), registry.clone()),
        _ => unreachable!("constructed test version"),
    };
    let request = resource_request(revision);
    let (server, mut client) = UnixStream::pair().unwrap();
    let (_reader, mut writer) = server.into_split();
    let (completion_sender, mut completion_receiver) =
        mpsc::channel::<(u64, ResourceDispatchCompletion)>(RESOURCE_COMPLETION_CHANNEL_CAPACITY);
    let mut connection = ResourceProtocolConnection::new();
    let mut pending = BTreeMap::new();
    let mut cancelled = BTreeMap::new();
    let mut tasks: BTreeMap<u64, ResourceTask> = BTreeMap::new();
    let mut producer_shutdown = JoinSet::new();
    let mut requests = BTreeMap::new();
    let resources = LocalRawSocketResources::new();
    let (_shutdown_sender, mut shutdown) = watch::channel(false);
    let hook_called = Arc::new(AtomicBool::new(false));
    let dispatcher = BlockingResourceDispatch {
        started: Arc::new(Notify::new()),
        cancelled: Arc::clone(&hook_called),
    };

    handle_resource_frame(
        ResourceClientFrame::Request(request.clone()),
        PayloadReservation { _permit: None },
        &dispatcher,
        &test_session(),
        &version,
        &resources,
        &mut connection,
        &mut pending,
        &mut cancelled,
        &mut tasks,
        &mut producer_shutdown,
        &mut requests,
        &completion_sender,
        &mut completion_receiver,
        &mut writer,
        &mut shutdown,
    )
    .await
    .unwrap();
    completion_sender
        .send((
            request.stream_id,
            ResourceDispatchCompletion {
                actions: resource_actions(&version, &request, vec![RuntimeValue::Integer(7)]),
                producer: None,
                producer_waiting_bytes: None,
                terminal_provenance: ResourceTerminalProvenance::Authenticated,
            },
        ))
        .await
        .unwrap();

    handle_resource_frame(
        ResourceClientFrame::Cancel(ResourceCancel {
            stream_id: request.stream_id,
            request_id: request.request_id,
            reason: ResourceCancellationCode::ClientRequested,
        }),
        PayloadReservation { _permit: None },
        &dispatcher,
        &test_session(),
        &version,
        &resources,
        &mut connection,
        &mut pending,
        &mut cancelled,
        &mut tasks,
        &mut producer_shutdown,
        &mut requests,
        &completion_sender,
        &mut completion_receiver,
        &mut writer,
        &mut shutdown,
    )
    .await
    .unwrap();

    assert!(matches!(
        pending
            .get(&request.stream_id)
            .and_then(|completion| completion.actions.front()),
        Some(ResourceServerFrame::Accepted(_))
    ));
    assert!(requests.contains_key(&request.stream_id));
    assert!(!hook_called.load(Ordering::SeqCst));
    assert!(cancelled.is_empty());
    assert!(
        flush_resource_pending(
            &version,
            &mut connection,
            &mut pending,
            &mut requests,
            &mut tasks,
            &mut producer_shutdown,
            &mut writer,
            &mut shutdown,
        )
        .await
        .unwrap()
    );
    assert!(pending.is_empty());
    assert!(!requests.contains_key(&request.stream_id));
    assert!(matches!(
        read_resource_server_frame(&mut client, &active, &registry).await,
        ResourceServerFrame::Accepted(frame)
            if frame.stream_id == request.stream_id
                && frame.request_id == request.request_id
                && frame.nested_invocation_id == InvocationId::from_bytes([0x21; 16])
                && frame.target_revision == request.target_revision
                && frame.resource_kind == request.resource_kind
    ));
    assert!(matches!(
        read_resource_server_frame(&mut client, &active, &registry).await,
        ResourceServerFrame::Values(frame)
            if frame.stream_id == request.stream_id
                && frame.request_id == request.request_id
                && frame.target_revision == request.target_revision
                && frame.batch_sequence == 0
                && frame.item_count == 1
                && frame.byte_count
                    == resource_value_byte_count(&version, &RuntimeValue::Integer(7))
                        .expect("resource value byte count")
                && frame.values == vec![RuntimeValue::Integer(7)]
    ));
    assert!(matches!(
        read_resource_server_frame(&mut client, &active, &registry).await,
        ResourceServerFrame::Completed(frame)
            if frame.stream_id == request.stream_id
                && frame.request_id == request.request_id
                && frame.target_revision == request.target_revision
                && frame.final_batch_sequence == 0
                && frame.total_items == 1
    ));
    assert!(
        timeout(
            Duration::from_millis(50),
            read_resource_server_frame(&mut client, &active, &registry),
        )
        .await
        .is_err(),
        "committed completion must not emit cancellation or stale frames",
    );
}
#[tokio::test]
async fn committing_resource_cancellation_does_not_terminalise_stream() {
    let (version, revision) = constructed_test_version();
    let request = resource_request(revision);
    let (server, mut client) = UnixStream::pair().unwrap();
    let (_reader, mut writer) = server.into_split();
    let (completion_sender, mut completion_receiver) =
        mpsc::channel::<(u64, ResourceDispatchCompletion)>(RESOURCE_COMPLETION_CHANNEL_CAPACITY);
    let mut connection = ResourceProtocolConnection::new();
    let mut pending = BTreeMap::new();
    let mut cancelled = BTreeMap::new();
    let mut tasks: BTreeMap<u64, ResourceTask> = BTreeMap::new();
    let mut producer_shutdown = JoinSet::new();
    let mut requests = BTreeMap::new();
    let resources = LocalRawSocketResources::new();
    let (_shutdown_sender, mut shutdown) = watch::channel(false);
    let dispatcher = BlockingResourceDispatch {
        started: Arc::new(Notify::new()),
        cancelled: Arc::new(AtomicBool::new(false)),
    };

    handle_resource_frame(
        ResourceClientFrame::Request(request.clone()),
        PayloadReservation { _permit: None },
        &dispatcher,
        &test_session(),
        &version,
        &resources,
        &mut connection,
        &mut pending,
        &mut cancelled,
        &mut tasks,
        &mut producer_shutdown,
        &mut requests,
        &completion_sender,
        &mut completion_receiver,
        &mut writer,
        &mut shutdown,
    )
    .await
    .unwrap();

    let cancellation = tasks
        .get(&request.stream_id)
        .expect("resource task")
        .cancellation
        .clone();
    assert!(cancellation.try_begin_commit());

    handle_resource_frame(
        ResourceClientFrame::Cancel(ResourceCancel {
            stream_id: request.stream_id,
            request_id: request.request_id,
            reason: ResourceCancellationCode::ClientRequested,
        }),
        PayloadReservation { _permit: None },
        &dispatcher,
        &test_session(),
        &version,
        &resources,
        &mut connection,
        &mut pending,
        &mut cancelled,
        &mut tasks,
        &mut producer_shutdown,
        &mut requests,
        &completion_sender,
        &mut completion_receiver,
        &mut writer,
        &mut shutdown,
    )
    .await
    .unwrap();

    assert!(cancelled.is_empty());
    assert_eq!(connection.live_resources(), 1);
    tasks
        .remove(&request.stream_id)
        .expect("resource task")
        .handle
        .abort();
    completion_sender
        .send((
            request.stream_id,
            ResourceDispatchCompletion {
                actions: resource_actions(&version, &request, vec![RuntimeValue::Integer(7)]),
                producer: None,
                producer_waiting_bytes: None,
                terminal_provenance: ResourceTerminalProvenance::Uncommitted,
            },
        ))
        .await
        .unwrap();
    assert!(
        drain_resource_completions(
            &dispatcher,
            &mut completion_receiver,
            &mut pending,
            &mut cancelled,
            &mut tasks,
        )
        .contains(&request.stream_id)
    );
    assert!(
        flush_resource_pending(
            &version,
            &mut connection,
            &mut pending,
            &mut requests,
            &mut tasks,
            &mut producer_shutdown,
            &mut writer,
            &mut shutdown,
        )
        .await
        .unwrap()
    );
    assert!(pending.is_empty());
    let (active, registry) = match &version {
        RawProtocolVersion::Constructed(active, registry) => (active.as_ref(), registry.as_ref()),
        _ => unreachable!("constructed test version"),
    };
    assert!(matches!(
        timeout(Duration::from_secs(1), read_resource_server_frame(&mut client, active, registry)).await,
        Ok(ResourceServerFrame::Accepted(frame))
            if frame.stream_id == request.stream_id && frame.request_id == request.request_id
    ));
    assert!(matches!(
        timeout(Duration::from_secs(1), read_resource_server_frame(&mut client, active, registry)).await,
        Ok(ResourceServerFrame::Values(frame))
            if frame.stream_id == request.stream_id
                && frame.request_id == request.request_id
                && frame.values == vec![RuntimeValue::Integer(7)]
    ));
    assert!(matches!(
        timeout(Duration::from_secs(1), read_resource_server_frame(&mut client, active, registry)).await,
        Ok(ResourceServerFrame::Completed(frame))
            if frame.stream_id == request.stream_id
                && frame.request_id == request.request_id
                && frame.total_items == 1
    ));
}

#[tokio::test]
async fn direct_committed_completion_wins_after_cancel() {
    let (version, revision) = constructed_test_version();
    let (active, registry) = match &version {
        RawProtocolVersion::Constructed(active, registry) => (active.clone(), registry.clone()),
        _ => unreachable!("constructed test version"),
    };
    let request = resource_request(revision);
    let (server, mut client) = UnixStream::pair().unwrap();
    let (_reader, mut writer) = server.into_split();
    let mut connection = ResourceProtocolConnection::new();
    connection
        .receive(ResourceClientFrame::Request(request.clone()))
        .expect("resource request opens");
    apply_resource_frame(
        &version,
        &mut connection,
        ResourceServerFrame::Accepted(orna_protocol::ResourceAccepted {
            stream_id: request.stream_id,
            request_id: request.request_id,
            nested_invocation_id: InvocationId::from_bytes([0x21; 16]),
            target_revision: request.target_revision,
            resource_kind: request.resource_kind,
        }),
    )
    .expect("resource request accepts");
    connection
        .receive(ResourceClientFrame::Cancel(ResourceCancel {
            stream_id: request.stream_id,
            request_id: request.request_id,
            reason: ResourceCancellationCode::ClientRequested,
        }))
        .expect("cancellation closes the protocol state");

    let mut actions = resource_actions(&version, &request, vec![RuntimeValue::Integer(7)]);
    assert!(matches!(
        actions.pop_front(),
        Some(ResourceServerFrame::Accepted(_))
    ));
    let mut pending = BTreeMap::from([(
        request.stream_id,
        ResourceDispatchCompletion {
            actions,
            producer: None,
            producer_waiting_bytes: None,
            terminal_provenance: ResourceTerminalProvenance::Authenticated,
        },
    )]);
    let mut requests = BTreeMap::from([(request.stream_id, request.clone())]);
    let mut tasks = BTreeMap::new();
    let mut producer_shutdown = JoinSet::new();
    let (_shutdown_sender, mut shutdown) = watch::channel(false);

    assert!(
        flush_resource_pending(
            &version,
            &mut connection,
            &mut pending,
            &mut requests,
            &mut tasks,
            &mut producer_shutdown,
            &mut writer,
            &mut shutdown,
        )
        .await
        .expect("direct committed completion flushes")
    );
    assert!(pending.is_empty());
    assert!(matches!(
        read_resource_server_frame(&mut client, &active, &registry).await,
        ResourceServerFrame::Completed(frame)
            if frame.stream_id == request.stream_id
                && frame.request_id == request.request_id
                && frame.target_revision == request.target_revision
                && frame.final_batch_sequence == 0
                && frame.total_items == 1
    ));
    assert!(
        timeout(
            Duration::from_millis(50),
            read_resource_server_frame(&mut client, &active, &registry),
        )
        .await
        .is_err(),
        "committed completion must not emit stale acceptance, values, or cancellation",
    );
}

#[tokio::test]
async fn direct_committed_failure_wins_after_cancel() {
    let (version, revision) = constructed_test_version();
    let (active, registry) = match &version {
        RawProtocolVersion::Constructed(active, registry) => (active.clone(), registry.clone()),
        _ => unreachable!("constructed test version"),
    };
    let request = resource_request(revision);
    let (server, mut client) = UnixStream::pair().unwrap();
    let (_reader, mut writer) = server.into_split();
    let mut connection = ResourceProtocolConnection::new();
    connection
        .receive(ResourceClientFrame::Request(request.clone()))
        .expect("resource request opens");
    apply_resource_frame(
        &version,
        &mut connection,
        ResourceServerFrame::Accepted(orna_protocol::ResourceAccepted {
            stream_id: request.stream_id,
            request_id: request.request_id,
            nested_invocation_id: InvocationId::from_bytes([0x21; 16]),
            target_revision: request.target_revision,
            resource_kind: request.resource_kind,
        }),
    )
    .expect("resource request accepts");
    connection
        .receive(ResourceClientFrame::Cancel(ResourceCancel {
            stream_id: request.stream_id,
            request_id: request.request_id,
            reason: ResourceCancellationCode::ClientRequested,
        }))
        .expect("cancellation closes the protocol state");

    let mut pending = BTreeMap::from([(
        request.stream_id,
        ResourceDispatchCompletion {
            actions: VecDeque::from([ResourceServerFrame::Failed(orna_protocol::ResourceFailed {
                stream_id: request.stream_id,
                request_id: request.request_id,
                target_revision: request.target_revision,
                failure: CallFailure::InternalFailure,
            })]),
            producer: None,
            producer_waiting_bytes: None,
            terminal_provenance: ResourceTerminalProvenance::Authenticated,
        },
    )]);
    let mut requests = BTreeMap::from([(request.stream_id, request.clone())]);
    let mut tasks = BTreeMap::new();
    let mut producer_shutdown = JoinSet::new();
    let (_shutdown_sender, mut shutdown) = watch::channel(false);

    assert!(
        flush_resource_pending(
            &version,
            &mut connection,
            &mut pending,
            &mut requests,
            &mut tasks,
            &mut producer_shutdown,
            &mut writer,
            &mut shutdown,
        )
        .await
        .expect("direct committed failure flushes")
    );
    assert!(pending.is_empty());
    assert!(matches!(
        read_resource_server_frame(&mut client, &active, &registry).await,
        ResourceServerFrame::Failed(frame)
            if frame.stream_id == request.stream_id
                && frame.request_id == request.request_id
                && frame.failure == CallFailure::InternalFailure
    ));
}

#[tokio::test]
async fn direct_committed_terminal_drains_late_values_without_mutating_protocol_state() {
    for completed in [true, false] {
        let (version, revision) = constructed_test_version();
        let (active, registry) = match &version {
            RawProtocolVersion::Constructed(active, registry) => (active.clone(), registry.clone()),
            _ => unreachable!("constructed test version"),
        };
        let mut request = resource_request(revision);
        request.resource_kind = ResourceKind::Stream;
        request.item_window = 4;
        let request_id = request.request_id;
        let mut connection = ResourceProtocolConnection::new();
        connection
            .receive(ResourceClientFrame::Request(request.clone()))
            .expect("resource request opens");
        apply_resource_frame(
            &version,
            &mut connection,
            ResourceServerFrame::Accepted(orna_protocol::ResourceAccepted {
                stream_id: request.stream_id,
                request_id,
                nested_invocation_id: InvocationId::from_bytes([0x21; 16]),
                target_revision: request.target_revision,
                resource_kind: request.resource_kind,
            }),
        )
        .expect("resource request accepts");
        let first_value = RuntimeValue::Integer(7);
        version
            .apply_resource(
                &mut connection,
                ResourceServerFrame::Values(orna_protocol::ResourceValues {
                    stream_id: request.stream_id,
                    request_id,
                    target_revision: request.target_revision,
                    batch_sequence: 0,
                    item_count: 1,
                    byte_count: resource_value_byte_count(&version, &first_value)
                        .expect("first value encodes"),
                    values: vec![first_value],
                }),
            )
            .expect("first value applies");
        connection
            .receive(ResourceClientFrame::Cancel(ResourceCancel {
                stream_id: request.stream_id,
                request_id,
                reason: ResourceCancellationCode::ClientRequested,
            }))
            .expect("local cancellation closes the protocol state");

        // Keep an unrelated live resource so the clone-based late-frame drain
        // proves that sequence, credit, and total state remain untouched.
        let mut live_request = request.clone();
        live_request.stream_id = 2;
        live_request.request_id = InvocationId::from_bytes([0x22; 16]);
        let live_value = RuntimeValue::Integer(8);
        connection
            .receive(ResourceClientFrame::Request(live_request.clone()))
            .expect("unrelated resource opens");
        apply_resource_frame(
            &version,
            &mut connection,
            ResourceServerFrame::Accepted(orna_protocol::ResourceAccepted {
                stream_id: live_request.stream_id,
                request_id: live_request.request_id,
                nested_invocation_id: InvocationId::from_bytes([0x23; 16]),
                target_revision: live_request.target_revision,
                resource_kind: live_request.resource_kind,
            }),
        )
        .expect("unrelated resource accepts");
        version
            .apply_resource(
                &mut connection,
                ResourceServerFrame::Values(orna_protocol::ResourceValues {
                    stream_id: live_request.stream_id,
                    request_id: live_request.request_id,
                    target_revision: live_request.target_revision,
                    batch_sequence: 0,
                    item_count: 1,
                    byte_count: resource_value_byte_count(&version, &live_value)
                        .expect("live value encodes"),
                    values: vec![live_value],
                }),
            )
            .expect("unrelated value applies");
        let before_late_drain = connection.clone();

        let late_value = RuntimeValue::Integer(9);
        let terminal = if completed {
            ResourceServerFrame::Completed(orna_protocol::ResourceCompleted {
                stream_id: request.stream_id,
                request_id,
                target_revision: request.target_revision,
                final_batch_sequence: 1,
                total_items: 2,
            })
        } else {
            ResourceServerFrame::Failed(orna_protocol::ResourceFailed {
                stream_id: request.stream_id,
                request_id,
                target_revision: request.target_revision,
                failure: CallFailure::InternalFailure,
            })
        };
        let mut pending = BTreeMap::from([(
            request.stream_id,
            ResourceDispatchCompletion {
                actions: VecDeque::from([
                    ResourceServerFrame::Values(orna_protocol::ResourceValues {
                        stream_id: request.stream_id,
                        request_id,
                        target_revision: request.target_revision,
                        batch_sequence: 1,
                        item_count: 1,
                        byte_count: resource_value_byte_count(&version, &late_value)
                            .expect("late value encodes"),
                        values: vec![late_value],
                    }),
                    terminal,
                ]),
                producer: None,
                producer_waiting_bytes: None,
                terminal_provenance: ResourceTerminalProvenance::Authenticated,
            },
        )]);
        let mut requests = BTreeMap::from([(request.stream_id, request.clone())]);
        let mut tasks = BTreeMap::new();
        let mut producer_shutdown = JoinSet::new();
        let (_shutdown_sender, mut shutdown) = watch::channel(false);
        let (server, mut client) = UnixStream::pair().unwrap();
        let (_reader, mut writer) = server.into_split();

        assert!(
            flush_resource_pending(
                &version,
                &mut connection,
                &mut pending,
                &mut requests,
                &mut tasks,
                &mut producer_shutdown,
                &mut writer,
                &mut shutdown,
            )
            .await
            .expect("committed terminal flushes after late value drain")
        );
        assert!(pending.is_empty());
        assert!(requests.is_empty());
        assert_eq!(
            connection, before_late_drain,
            "late values are applied to a clone and cannot consume live state"
        );
        assert_eq!(connection.live_resources(), 1);
        assert_eq!(
            connection
                .resource_credit(live_request.stream_id, live_request.request_id)
                .expect("unrelated live resource remains inspectable"),
            before_late_drain
                .resource_credit(live_request.stream_id, live_request.request_id)
                .expect("snapshot retains unrelated live resource"),
        );
        if completed {
            assert!(matches!(
                read_resource_server_frame(&mut client, &active, &registry).await,
                ResourceServerFrame::Completed(frame)
                    if frame.stream_id == request.stream_id
                        && frame.request_id == request.request_id
                        && frame.final_batch_sequence == 1
                        && frame.total_items == 2
            ));
        } else {
            assert!(matches!(
                read_resource_server_frame(&mut client, &active, &registry).await,
                ResourceServerFrame::Failed(frame)
                    if frame.stream_id == request.stream_id
                        && frame.request_id == request.request_id
                        && frame.failure == CallFailure::InternalFailure
            ));
        }
        drop(writer);
    }
}

#[tokio::test]
async fn server_shutdown_cancels_active_resource_without_emitting_a_terminal_frame() {
    let (version, revision) = constructed_test_version();
    let (active, registry) = match &version {
        RawProtocolVersion::Constructed(active, registry) => (active.clone(), registry.clone()),
        _ => unreachable!("constructed test version"),
    };
    let encoded = encode_resource_client_frame(
        &active,
        &registry,
        &ResourceClientFrame::Request(resource_request(revision)),
    )
    .unwrap();
    let started = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let cancelled = Arc::new(AtomicBool::new(false));
    let dispatcher = ShutdownResourceDispatch {
        started: Arc::clone(&started),
        dropped: Arc::clone(&dropped),
        cancelled: Arc::clone(&cancelled),
    };
    let (shutdown_sender, shutdown) = watch::channel(false);
    let (server, mut client) = UnixStream::pair().unwrap();
    let server_task = tokio::spawn(drive_versioned_authenticated_stream_until_shutdown(
        dispatcher,
        test_session(),
        version,
        server,
        LocalRawSocketResources::new(),
        shutdown,
    ));

    client.write_all(&encoded).await.unwrap();
    started.notified().await;
    shutdown_sender.send(true).unwrap();
    server_task.await.unwrap().unwrap();

    let mut byte = [0_u8; 1];
    assert_eq!(client.read(&mut byte).await.unwrap(), 0);
    assert!(dropped.load(Ordering::SeqCst));
    assert!(cancelled.load(Ordering::SeqCst));
}
