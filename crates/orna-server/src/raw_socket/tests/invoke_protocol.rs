use super::*;

#[tokio::test]
async fn catalogue_connection_drives_enum_arguments_and_results() {
    let catalogue = Arc::new(enum_catalogue());
    let value = RuntimeValue::Enum(EnumValue::new(&catalogue, ENUM_TYPE, "qualified").unwrap());
    let dispatcher = TestDispatch::new(vec![
        ServerAction::Events {
            stream: 1,
            events: vec![Event::Value(value.clone())],
        },
        ServerAction::Completed { stream: 1 },
    ]);
    let resources = LocalRawSocketResources::new();
    let (server, mut client) = UnixStream::pair().expect("Unix stream pair");
    let (_shutdown_sender, shutdown) = watch::channel(false);
    let server_task = tokio::spawn(drive_versioned_authenticated_stream_until_shutdown(
        dispatcher,
        test_session(),
        RawProtocolVersion::Catalogue(Arc::clone(&catalogue)),
        server,
        resources,
        shutdown,
    ));

    for frame in [
        ClientFrame::CallRawStart {
            stream: 1,
            function: FUNCTION,
        },
        ClientFrame::WindowUpdate {
            stream: 1,
            channel: Channel::ResultValues,
            credit: 1024,
        },
        ClientFrame::CallArgument {
            stream: 1,
            parameter: orna_core::ParameterId::from_bytes([0x34; 16]),
            value: value.clone(),
        },
        ClientFrame::CallArgumentsComplete { stream: 1 },
    ] {
        client
            .write_all(
                &encode_catalogue_client_frame(&catalogue, &frame)
                    .expect("catalogue client frame encodes"),
            )
            .await
            .expect("catalogue client frame writes");
    }

    assert!(matches!(
        read_catalogue_server_frame(&mut client, &catalogue).await,
        ServerFrame::CallAccepted { stream: 1, .. }
    ));
    assert!(matches!(
        read_catalogue_server_frame(&mut client, &catalogue).await,
        ServerFrame::EventBatch {
            stream: 1,
            channel: Channel::ResultValues,
            events,
        } if events.len() == 1 && events[0].event == Event::Value(value)
    ));
    assert_eq!(
        read_catalogue_server_frame(&mut client, &catalogue).await,
        ServerFrame::CallCompleted { stream: 1 }
    );

    client.shutdown().await.expect("client shutdown");
    server_task
        .await
        .expect("catalogue connection task")
        .expect("catalogue connection closes");
}

#[tokio::test]
async fn fragmented_frames_flow_control_and_eof_preserve_bounded_state() {
    let dispatcher = TestDispatch::new(vec![
        ServerAction::Events {
            stream: 1,
            events: vec![Event::Value(RuntimeValue::Boolean(true))],
        },
        ServerAction::Completed { stream: 1 },
    ]);
    let resources = LocalRawSocketResources::new();
    let (server, mut client) = UnixStream::pair().expect("Unix stream pair");
    let server_task = tokio::spawn(drive_authenticated_stream(
        dispatcher,
        test_session(),
        server,
        resources.clone(),
    ));

    let ping =
        encode_client_frame(&ClientFrame::Ping { token: [7; 8] }).expect("PING frame encodes");
    for byte in ping {
        client.write_all(&[byte]).await.expect("fragment writes");
    }
    assert_eq!(
        read_server_frame(&mut client).await,
        ServerFrame::Pong { token: [7; 8] }
    );

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
        ServerFrame::CallAccepted { stream: 1, .. }
    ));
    assert!(
        timeout(Duration::from_millis(25), read_server_frame(&mut client))
            .await
            .is_err()
    );

    let ping =
        encode_client_frame(&ClientFrame::Ping { token: [8; 8] }).expect("PING frame encodes");
    for byte in ping {
        client
            .write_all(&[byte])
            .await
            .expect("concurrent fragment writes");
    }
    assert_eq!(
        read_server_frame(&mut client).await,
        ServerFrame::Pong { token: [8; 8] }
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
        .expect("clean EOF");
    assert_eq!(resources.payload.available_permits(), SHARED_PAYLOAD_BYTES);
    assert_eq!(
        resources.kernel_operations.available_permits(),
        KERNEL_OPERATION_LIMIT
    );
}

#[tokio::test]
async fn buffered_sealed_cancel_prevents_acceptance_for_all_preflight_outcomes() {
    for outcome in [
        GatedPreflightOutcome::Accepted,
        GatedPreflightOutcome::RejectedInternalFailure,
        GatedPreflightOutcome::Error,
    ] {
        let (version, _) = constructed_test_version();
        let (active, registry) = match &version {
            RawProtocolVersion::Constructed(active, registry) => (active.clone(), registry.clone()),
            _ => unreachable!("constructed test version"),
        };
        let request = test_invoke_request(&active, &registry);
        let dispatcher = GatedInvokePreflightDispatch::new(outcome);
        let preflight_started = Arc::clone(&dispatcher.preflight_started);
        let cancellation_seen = Arc::clone(&dispatcher.cancellation_seen);
        let preflight_release = Arc::clone(&dispatcher.preflight_release);
        let start_invoked = Arc::clone(&dispatcher.start_invoked);
        let resources = LocalRawSocketResources::new();
        let (server, mut client) = UnixStream::pair().expect("Unix stream pair");
        let (_shutdown_sender, shutdown) = watch::channel(false);
        let server_task = tokio::spawn(drive_versioned_authenticated_stream_until_shutdown(
            dispatcher,
            test_session(),
            version,
            server,
            resources.clone(),
            shutdown,
        ));

        for frame in [
            ClientFrame::CallRawStart {
                stream: 1,
                function: SYS_INVOKE_FUNCTION_ID,
            },
            ClientFrame::CallInvokeRequest {
                stream: 1,
                request: request.clone(),
            },
            ClientFrame::CallArgumentsComplete { stream: 1 },
        ] {
            client
                .write_all(
                    &encode_constructed_client_frame(&active, &registry, &frame)
                        .expect("invoke frame encodes"),
                )
                .await
                .expect("invoke frame writes");
        }
        timeout(Duration::from_secs(1), preflight_started.notified())
            .await
            .expect("sealed preflight starts");

        let cancel = ClientFrame::CallCancel { stream: 1 };
        client
            .write_all(
                &encode_constructed_client_frame(&active, &registry, &cancel)
                    .expect("cancel frame encodes"),
            )
            .await
            .expect("cancel frame writes");
        timeout(Duration::from_secs(1), cancellation_seen.notified())
            .await
            .expect("buffered cancellation reaches dispatching state");
        preflight_release.notify_one();

        assert_eq!(
            read_constructed_server_frame(&mut client, &active, &registry).await,
            ServerFrame::CallCancelled { stream: 1 }
        );
        assert!(
            timeout(
                Duration::from_millis(25),
                read_constructed_server_frame(&mut client, &active, &registry),
            )
            .await
            .is_err(),
            "pre-accept cancellation must not leak failed, accepted, or invocation output"
        );
        assert!(!start_invoked.load(Ordering::SeqCst));

        client.shutdown().await.expect("client shutdown");
        server_task
            .await
            .expect("server task joins")
            .expect("server stream drains preflight");
        assert_eq!(resources.payload.available_permits(), SHARED_PAYLOAD_BYTES);
        assert_eq!(
            resources.kernel_operations.available_permits(),
            KERNEL_OPERATION_LIMIT
        );
    }
}

#[tokio::test]
async fn receiving_cancel_releases_retained_payload_before_connection_close() {
    let dispatcher = TestDispatch::new(Vec::new());
    let resources = LocalRawSocketResources::new();
    let (server, mut client) = UnixStream::pair().expect("Unix stream pair");
    let server_task = tokio::spawn(drive_authenticated_stream(
        dispatcher,
        test_session(),
        server,
        resources.clone(),
    ));

    let start = ClientFrame::CallRawStart {
        stream: 1,
        function: FUNCTION,
    };
    let retained = encode_client_frame(&start)
        .expect("start frame encodes")
        .len()
        - FRAME_HEADER_LENGTH;
    send_client_frame(&mut client, &start).await;
    timeout(Duration::from_secs(1), async {
        loop {
            if resources.payload.available_permits() == SHARED_PAYLOAD_BYTES - retained {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("start payload reservation is retained");

    let second_start = ClientFrame::CallRawStart {
        stream: 2,
        function: FUNCTION,
    };
    let second_retained = encode_client_frame(&second_start)
        .expect("second start frame encodes")
        .len()
        - FRAME_HEADER_LENGTH;
    send_client_frame(&mut client, &second_start).await;
    timeout(Duration::from_secs(1), async {
        loop {
            if resources.payload.available_permits()
                == SHARED_PAYLOAD_BYTES - retained - second_retained
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("both stream payload reservations are retained");

    send_client_frame(&mut client, &ClientFrame::CallCancel { stream: 1 }).await;
    assert_eq!(
        read_server_frame(&mut client).await,
        ServerFrame::CallCancelled { stream: 1 }
    );
    assert_eq!(
        resources.payload.available_permits(),
        SHARED_PAYLOAD_BYTES - second_retained
    );

    send_client_frame(&mut client, &ClientFrame::CallCancel { stream: 2 }).await;
    assert_eq!(
        read_server_frame(&mut client).await,
        ServerFrame::CallCancelled { stream: 2 }
    );
    assert_eq!(resources.payload.available_permits(), SHARED_PAYLOAD_BYTES);

    send_client_frame(&mut client, &ClientFrame::Ping { token: [4; 8] }).await;
    assert_eq!(
        read_server_frame(&mut client).await,
        ServerFrame::Pong { token: [4; 8] }
    );

    client.shutdown().await.expect("client shutdown");
    server_task
        .await
        .expect("connection task")
        .expect("clean EOF");
}

#[tokio::test]
async fn buffered_cancel_precedes_first_finish_poll_but_finish_still_runs() {
    let dispatcher = TestDispatch::new(vec![ServerAction::Completed { stream: 1 }]);
    let resources = LocalRawSocketResources::new();
    let (server, mut client) = UnixStream::pair().expect("Unix stream pair");
    let server_task = tokio::spawn(drive_authenticated_stream(
        dispatcher.clone(),
        test_session(),
        server,
        resources.clone(),
    ));

    let frames = [
        ClientFrame::CallRawStart {
            stream: 1,
            function: FUNCTION,
        },
        ClientFrame::CallArgumentsComplete { stream: 1 },
        ClientFrame::CallCancel { stream: 1 },
    ];
    let encoded: Vec<_> = frames
        .iter()
        .flat_map(|frame| encode_client_frame(frame).expect("client frame encodes"))
        .collect();
    client.write_all(&encoded).await.expect("buffered frames");

    assert!(matches!(
        read_server_frame(&mut client).await,
        ServerFrame::CallAccepted { stream: 1, .. }
    ));
    assert_eq!(
        read_server_frame(&mut client).await,
        ServerFrame::CallCancelled { stream: 1 }
    );
    assert!(
        dispatcher
            .first_poll_saw_cancellation
            .load(Ordering::SeqCst),
        "buffered CALL_CANCEL must be observed before the first finish poll"
    );
    assert!(
        timeout(Duration::from_millis(25), read_server_frame(&mut client))
            .await
            .is_err(),
        "finished clean actions must remain discarded after CALL_CANCELLED"
    );

    client.shutdown().await.expect("client shutdown");
    server_task
        .await
        .expect("connection task")
        .expect("clean EOF");
    assert_eq!(resources.payload.available_permits(), SHARED_PAYLOAD_BYTES);
    assert_eq!(
        resources.kernel_operations.available_permits(),
        KERNEL_OPERATION_LIMIT
    );
}

#[tokio::test]
async fn peer_failure_after_acceptance_still_polls_protected_work() {
    let dispatcher = TestDispatch::new(vec![ServerAction::Completed { stream: 1 }]);
    let polled = Arc::clone(&dispatcher.polled);
    let resources = LocalRawSocketResources::new();
    let (server, mut client) = UnixStream::pair().expect("Unix stream pair");
    let server_task = tokio::spawn(drive_authenticated_stream(
        dispatcher,
        test_session(),
        server,
        resources,
    ));
    for frame in [
        ClientFrame::CallRawStart {
            stream: 1,
            function: FUNCTION,
        },
        ClientFrame::CallArgumentsComplete { stream: 1 },
    ] {
        client
            .write_all(&encode_client_frame(&frame).expect("client frame encodes"))
            .await
            .expect("client frame writes");
    }
    drop(client);

    let _ = timeout(Duration::from_secs(1), server_task)
        .await
        .expect("connection drains after peer failure")
        .expect("connection task joins");
    assert!(polled.load(Ordering::SeqCst));
}

#[test]
fn sealed_pull_credit_is_bounded_by_result_window_and_frame_capacity() {
    let credit =
        sealed_pull_credit(1, None).expect("one byte of live credit creates one bounded pull");
    assert_eq!(credit.item_count, 1);
    assert_eq!(credit.byte_count, 1);

    let credit = sealed_pull_credit(u64::MAX, None).expect("bounded frame credit");
    assert_eq!(credit.item_count, 1);
    assert_eq!(
        credit.byte_count,
        (MAX_FRAME_PAYLOAD_LENGTH as u64).saturating_sub(SEALED_EVENT_FRAME_OVERHEAD),
    );
}

#[test]
fn sealed_event_overhead_matches_constructed_frame_layout() {
    let (version, _revision) = constructed_test_version();
    let (active, registry) = match &version {
        RawProtocolVersion::Constructed(active, registry) => (active, registry),
        _ => unreachable!("constructed test version"),
    };
    let value = RuntimeValue::Boolean(true);
    let encoded_value = encode_constructed_value(active, registry, &value)
        .expect("bounded constructed value encodes");
    let invocation = InvocationId::from_bytes([0x75; 16]);
    let event = InvokeEvent::new(
        invocation,
        1,
        InvocationEventBody::ValueBatch {
            schema: None,
            values: vec![InvokeValue::new(value).expect("invoke value")],
        },
    )
    .expect("value event");
    let encoded = version
        .encode_server_frame(&ServerFrame::EventBatch {
            stream: 1,
            channel: Channel::ResultValues,
            events: vec![orna_protocol::EventRecord {
                sequence: 1,
                event: Event::Value(RuntimeValue::InvokeEvent(event)),
            }],
        })
        .expect("constructed event frame encodes");
    assert_eq!(
        encoded.len() - FRAME_HEADER_LENGTH - encoded_value.len(),
        SEALED_EVENT_FRAME_OVERHEAD as usize,
    );
    assert_eq!(
        SEALED_MAX_VALUE_BYTES,
        (MAX_FRAME_PAYLOAD_LENGTH as u64) - SEALED_EVENT_FRAME_OVERHEAD,
    );
}

#[tokio::test]
async fn flush_pending_yields_after_a_bounded_output_burst() {
    let version = RawProtocolVersion::One;
    let mut connection = ProtocolConnection::new();
    version
        .receive(
            &mut connection,
            ClientFrame::CallRawStart {
                stream: 1,
                function: FUNCTION,
            },
        )
        .expect("raw call starts");
    version
        .receive(
            &mut connection,
            ClientFrame::WindowUpdate {
                stream: 1,
                channel: Channel::ResultValues,
                credit: 4096,
            },
        )
        .expect("result credit applies");
    version
        .receive(
            &mut connection,
            ClientFrame::CallArgumentsComplete { stream: 1 },
        )
        .expect("raw call dispatches");
    version
        .apply(
            &mut connection,
            ServerAction::Accepted {
                stream: 1,
                invocation: InvocationId::from_bytes([0x76; 16]),
            },
        )
        .expect("raw call accepts");

    let actions = (0..=PENDING_FLUSH_FAIRNESS_BUDGET)
        .map(|_| ServerAction::Events {
            stream: 1,
            events: vec![Event::Value(RuntimeValue::Boolean(true))],
        })
        .chain(std::iter::once(ServerAction::Completed { stream: 1 }))
        .collect();
    let mut pending = BTreeMap::from([(
        1,
        DispatchCompletion {
            sealed_producer: None,
            sealed_invocation: None,
            sealed_next_event_sequence: 1,
            sealed_next_outer_sequence: 2,
            actions,
            cancellation: ServerAction::Cancelled { stream: 1 },
            cancellation_token: None,
            start_gate: None,
            start_delivered: true,
            terminal_delivered: false,
            terminal_claimed: false,
            worker_completed: true,
            _guards: None,
        },
    )]);
    let (server, mut client) = UnixStream::pair().expect("socket pair");
    let (_reader, mut writer) = server.into_split();
    let (_shutdown_sender, mut shutdown) = watch::channel(false);
    let mut sealed_pull_tasks = JoinSet::new();
    let mut sealed_pull_in_flight = BTreeSet::new();
    let mut sealed_pull_waiting_bytes = BTreeMap::new();
    let mut producer_shutdown = JoinSet::new();
    let mut fairness_yielded = false;

    assert!(
        flush_pending_with_fairness_boundary(
            &version,
            &mut connection,
            &mut pending,
            &mut sealed_pull_tasks,
            &mut sealed_pull_in_flight,
            &mut sealed_pull_waiting_bytes,
            &mut producer_shutdown,
            &mut writer,
            &mut shutdown,
            &mut fairness_yielded,
        )
        .await
        .expect("first bounded flush succeeds")
    );
    assert!(fairness_yielded);
    assert_eq!(
        pending
            .get(&1)
            .expect("pending completion remains")
            .actions
            .len(),
        2,
    );
    for _ in 0..PENDING_FLUSH_FAIRNESS_BUDGET {
        let _ = read_server_frame(&mut client).await;
    }

    assert!(
        flush_pending_with_fairness_boundary(
            &version,
            &mut connection,
            &mut pending,
            &mut sealed_pull_tasks,
            &mut sealed_pull_in_flight,
            &mut sealed_pull_waiting_bytes,
            &mut producer_shutdown,
            &mut writer,
            &mut shutdown,
            &mut fairness_yielded,
        )
        .await
        .expect("second bounded flush succeeds")
    );
    assert!(!fairness_yielded);
    assert!(pending.is_empty());
    let _ = read_server_frame(&mut client).await;
    assert_eq!(
        read_server_frame(&mut client).await,
        ServerFrame::CallCompleted { stream: 1 }
    );
}

#[test]
fn sealed_pull_credit_waits_for_the_pending_value_size() {
    assert!(sealed_pull_credit(8, Some(9)).is_none());
    assert_eq!(
        sealed_pull_credit(9, Some(9))
            .expect("required value credit")
            .byte_count,
        9
    );
}

#[test]
fn sealed_result_cancellation_wins_after_execution_returns() {
    let invocation = InvocationId::from_bytes([0x73; 16]);
    let event = InvokeEvent::new(
        invocation,
        0,
        InvocationEventBody::Completed {
            duration_nanoseconds: 0,
        },
    )
    .expect("completed event");
    let events = InvocationEventBatch::new(vec![InvocationEventRecord::new(1, event)])
        .expect("completed event batch");
    let execution = Ok(SealedInvocationExecution::Result(
        SealedInvocationResult::Completed { invocation, events },
    ));
    let cancellation = ResourceCancellation::new();
    assert!(!sealed_result_cancellation_won(&cancellation, &execution));
    assert!(cancellation.request_cancel());
    assert!(sealed_result_cancellation_won(&cancellation, &execution));
}

#[test]
fn sealed_failed_result_emits_failure_after_cancellation_requested() {
    let invocation = InvocationId::from_bytes([0x75; 16]);
    let events = redacted_invoke_failure(
        invocation,
        InvocationFailurePhase::Internal,
        "INVOKE_INTERNAL_FAILURE",
        "invocation could not complete",
        InvocationRetryability::Unknown,
    );
    let execution = Ok(SealedInvocationExecution::Result(
        SealedInvocationResult::Failed { invocation, events },
    ));
    let cancellation = ResourceCancellation::new();

    assert!(cancellation.request_cancel());
    assert!(!sealed_result_cancellation_won(&cancellation, &execution));
    let Ok(SealedInvocationExecution::Result(SealedInvocationResult::Failed { events, .. })) =
        execution
    else {
        panic!("post-start failure result changed into cancellation");
    };
    assert_eq!(events.records().len(), 1);
    assert_eq!(
        events.records()[0].event().kind(),
        InvocationEventKind::InvocationFailed
    );
}

#[tokio::test]
async fn oversized_sealed_pull_waiting_value_fails_closed() {
    let (version, _revision) = constructed_test_version();
    let (active, registry) = match &version {
        RawProtocolVersion::Constructed(active, registry) => (active.as_ref(), registry.as_ref()),
        _ => unreachable!("constructed test version"),
    };
    let mut connection = ProtocolConnection::new();
    let request = test_invoke_request(active, registry);
    version
        .receive(
            &mut connection,
            ClientFrame::CallRawStart {
                stream: 1,
                function: SYS_INVOKE_FUNCTION_ID,
            },
        )
        .expect("sealed call starts");
    version
        .receive(
            &mut connection,
            ClientFrame::CallInvokeRequest { stream: 1, request },
        )
        .expect("sealed request applies");
    version
        .receive(
            &mut connection,
            ClientFrame::CallArgumentsComplete { stream: 1 },
        )
        .expect("sealed call dispatches");
    version
        .receive(
            &mut connection,
            ClientFrame::WindowUpdate {
                stream: 1,
                channel: Channel::ResultValues,
                credit: MAX_FRAME_PAYLOAD_LENGTH as u64,
            },
        )
        .expect("sealed call receives result credit");
    let invocation = InvocationId::from_bytes([0x74; 16]);
    version
        .apply(
            &mut connection,
            ServerAction::Accepted {
                stream: 1,
                invocation,
            },
        )
        .expect("sealed call accepts");
    let started = InvokeEvent::new(
        invocation,
        0,
        InvocationEventBody::Started {
            visible_principal: None,
        },
    )
    .expect("started event");
    let started_events = InvocationEventBatch::new(vec![InvocationEventRecord::new(1, started)])
        .expect("started event batch");
    version
        .apply(
            &mut connection,
            ServerAction::InvokeEvents {
                stream: 1,
                events: started_events,
            },
        )
        .expect("started event applies");

    let (server, mut client) = UnixStream::pair().expect("socket pair");
    let (_reader, mut writer) = server.into_split();
    let (_shutdown_sender, mut shutdown) = watch::channel(false);
    let mut pending = BTreeMap::from([(
        1,
        DispatchCompletion {
            sealed_producer: None,
            sealed_invocation: Some(invocation),
            sealed_next_event_sequence: 1,
            sealed_next_outer_sequence: 2,
            actions: VecDeque::new(),
            cancellation: ServerAction::InvokeCancelled { stream: 1 },
            cancellation_token: None,
            start_gate: None,
            start_delivered: true,
            terminal_delivered: false,
            terminal_claimed: false,
            worker_completed: true,
            _guards: None,
        },
    )]);
    let mut sealed_pull_tasks = JoinSet::new();
    let mut sealed_pull_in_flight = BTreeSet::new();
    let mut sealed_pull_waiting_bytes =
        BTreeMap::from([(1, SEALED_MAX_VALUE_BYTES.saturating_add(1))]);
    let mut producer_shutdown = JoinSet::new();

    assert!(
        flush_pending(
            &version,
            &mut connection,
            &mut pending,
            &mut sealed_pull_tasks,
            &mut sealed_pull_in_flight,
            &mut sealed_pull_waiting_bytes,
            &mut producer_shutdown,
            &mut writer,
            &mut shutdown,
        )
        .await
        .expect("oversized sealed value fails closed")
    );
    assert!(matches!(
        orna_protocol::decode_constructed_invocation_event_frame(
            active,
            registry,
            &read_encoded_server_frame(&mut client, "sealed invocation event frame").await,
        )
        .expect("constructed invocation event frame decodes"),
        ServerFrame::EventBatch { stream: 1, .. }
    ));
    assert_eq!(
        read_constructed_server_frame(&mut client, active, registry).await,
        ServerFrame::CallCompleted { stream: 1 }
    );
}

#[tokio::test]
async fn queued_value_and_completion_are_replaced_by_cancel_without_credit() {
    let dispatcher = TestDispatch::new(vec![
        ServerAction::Events {
            stream: 1,
            events: vec![Event::Value(RuntimeValue::Boolean(true))],
        },
        ServerAction::Completed { stream: 1 },
    ]);
    let version = RawProtocolVersion::One;
    let mut connection = ProtocolConnection::new();
    assert!(
        version
            .receive(
                &mut connection,
                ClientFrame::CallRawStart {
                    stream: 1,
                    function: FUNCTION,
                },
            )
            .expect("raw call starts")
            .is_none()
    );
    assert!(matches!(
        version
            .receive(
                &mut connection,
                ClientFrame::CallArgumentsComplete { stream: 1 },
            )
            .expect("raw call dispatches"),
        Some(ClientAction::Dispatch { stream: 1, .. })
    ));
    version
        .apply(
            &mut connection,
            ServerAction::Accepted {
                stream: 1,
                invocation: InvocationId::from_bytes([9; 16]),
            },
        )
        .expect("accepted frame applies");

    let (server, mut client) = UnixStream::pair().expect("socket pair");
    let (_reader, mut writer) = server.into_split();
    let mut pending = BTreeMap::from([(
        1,
        DispatchCompletion {
            sealed_producer: None,
            sealed_invocation: None,
            sealed_next_event_sequence: 1,
            sealed_next_outer_sequence: 2,
            actions: VecDeque::new(),
            cancellation: ServerAction::Cancelled { stream: 1 },
            cancellation_token: None,
            start_gate: None,
            start_delivered: true,
            terminal_delivered: false,
            terminal_claimed: false,
            worker_completed: false,
            _guards: None,
        },
    )]);
    merge_dispatch_completion(
        1,
        DispatchCompletion {
            sealed_producer: None,
            sealed_invocation: None,
            sealed_next_event_sequence: 1,
            sealed_next_outer_sequence: 2,
            actions: VecDeque::from([
                ServerAction::Events {
                    stream: 1,
                    events: vec![Event::Value(RuntimeValue::Boolean(true))],
                },
                ServerAction::Completed { stream: 1 },
            ]),
            cancellation: ServerAction::Cancelled { stream: 1 },
            cancellation_token: None,
            start_gate: None,
            start_delivered: true,
            terminal_delivered: false,
            terminal_claimed: false,
            worker_completed: false,
            _guards: None,
        },
        &mut pending,
        &mut BTreeSet::new(),
        &mut BTreeSet::new(),
    );
    assert!(!pending.get(&1).expect("merged completion").terminal_claimed);
    let mut retained_payload = BTreeMap::new();
    let mut cancelled = BTreeSet::new();
    let mut cancelled_pending = BTreeSet::new();
    let mut preflight_pending = BTreeSet::new();
    let mut preflight_cancelled = BTreeSet::new();
    let mut preflight_tasks = JoinSet::new();
    let mut producer_shutdown = JoinSet::new();
    let mut unstarted = VecDeque::new();
    let resources = LocalRawSocketResources::new();
    let (_shutdown_sender, mut shutdown) = watch::channel(false);
    let mut sealed_pull_tasks = JoinSet::new();
    let mut sealed_pull_in_flight = BTreeSet::new();
    let mut sealed_pull_waiting_bytes = BTreeMap::new();

    assert!(
        handle_client_frame(
            RawIncomingFrame {
                frame: ClientFrame::CallCancel { stream: 1 },
                reservation: PayloadReservation { _permit: None },
            },
            &dispatcher,
            &test_session(),
            &version,
            &resources,
            &mut connection,
            &mut retained_payload,
            &mut cancelled,
            &mut cancelled_pending,
            &mut preflight_pending,
            &mut preflight_cancelled,
            &mut preflight_tasks,
            &mut producer_shutdown,
            &mut pending,
            &mut unstarted,
            &mut writer,
            &mut shutdown,
        )
        .await
        .expect("queued completion cancellation is accepted")
    );
    assert_eq!(
        pending.get(&1).expect("pending completion remains").actions,
        VecDeque::from([ServerAction::Cancelled { stream: 1 }]),
    );
    assert!(dispatcher.cancelled.load(Ordering::SeqCst));

    assert!(
        flush_pending(
            &version,
            &mut connection,
            &mut pending,
            &mut sealed_pull_tasks,
            &mut sealed_pull_in_flight,
            &mut sealed_pull_waiting_bytes,
            &mut producer_shutdown,
            &mut writer,
            &mut shutdown,
        )
        .await
        .expect("cancelled completion flushes")
    );
    assert_eq!(
        read_server_frame(&mut client).await,
        ServerFrame::CallCancelled { stream: 1 }
    );
    assert!(
        timeout(Duration::from_millis(25), read_server_frame(&mut client))
            .await
            .is_err(),
        "queued value or completion escaped cancellation"
    );
}
#[test]
fn sealed_failure_after_value_preserves_contiguous_sequences() {
    let invocation = InvocationId::from_bytes([0x71; 16]);
    let value = InvokeValue::new(RuntimeValue::Integer(7)).expect("invoke value");
    let value_event = InvokeEvent::new(
        invocation,
        1,
        InvocationEventBody::ValueBatch {
            schema: None,
            values: vec![value],
        },
    )
    .expect("value event");
    let value_events = InvocationEventBatch::new(vec![InvocationEventRecord::new(2, value_event)])
        .expect("value event batch");
    let mut completion = DispatchCompletion {
        sealed_producer: None,
        sealed_invocation: Some(invocation),
        sealed_next_event_sequence: 2,
        sealed_next_outer_sequence: 3,
        actions: VecDeque::from([ServerAction::InvokeEvents {
            stream: 1,
            events: value_events,
        }]),
        cancellation: ServerAction::InvokeCancelled { stream: 1 },
        cancellation_token: None,
        start_gate: None,
        start_delivered: true,
        terminal_delivered: false,
        terminal_claimed: false,
        worker_completed: true,
        _guards: None,
    };

    queue_sealed_terminal_failure(
        1,
        &mut completion,
        invocation,
        InvocationFailurePhase::Internal,
        "INVOKE_INTERNAL_FAILURE",
        "invocation could not complete",
        InvocationRetryability::Unknown,
    );

    assert_eq!(completion.sealed_next_event_sequence, 3);
    assert_eq!(completion.sealed_next_outer_sequence, 4);
    assert_eq!(completion.actions.len(), 3);
    let Some(ServerAction::InvokeEvents { events, .. }) = completion.actions.get(1) else {
        panic!("failure event follows prior value event");
    };
    let failure = &events.records()[0];
    assert_eq!(failure.outer_sequence(), 3);
    assert_eq!(failure.event().sequence(), 2);
    assert_eq!(failure.event().invocation_id(), invocation);
    assert_eq!(
        failure.event().kind(),
        InvocationEventKind::InvocationFailed
    );
}

#[test]
fn disconnect_cancellation_targets_unstarted_and_client_work() {
    let invocation = InvocationId::from_bytes([0x52; 16]);
    let mut completion = DispatchCompletion {
        sealed_producer: None,
        sealed_invocation: Some(invocation),
        sealed_next_event_sequence: 1,
        sealed_next_outer_sequence: 2,
        actions: VecDeque::new(),
        cancellation: ServerAction::InvokeCancelled { stream: 1 },
        cancellation_token: None,
        start_gate: None,
        start_delivered: true,
        terminal_delivered: false,
        terminal_claimed: false,
        worker_completed: false,
        _guards: None,
    };

    assert!(should_cancel_on_disconnect(&completion));
    assert!(!should_drain_sealed_on_disconnect(&completion));
    completion.start_delivered = false;
    assert!(should_cancel_on_disconnect(&completion));
    assert!(!should_drain_sealed_on_disconnect(&completion));
    completion.sealed_invocation = None;
    completion.start_delivered = true;
    assert!(should_cancel_on_disconnect(&completion));
    completion.sealed_invocation = Some(invocation);
    let cancellation = ResourceCancellation::new();
    cancellation.request_cancel();
    completion.cancellation_token = Some(cancellation);
    assert!(!should_cancel_on_disconnect(&completion));
    assert!(!should_drain_sealed_on_disconnect(&completion));
    completion.start_delivered = false;
    assert!(!should_cancel_on_disconnect(&completion));
    completion.terminal_delivered = true;
    assert!(!should_cancel_on_disconnect(&completion));
    assert!(!should_drain_sealed_on_disconnect(&completion));
}

#[test]
fn pre_start_internal_failure_wins_over_cancel_marker() {
    let mut pending = BTreeMap::new();
    let mut cancelled = BTreeSet::from([1]);

    merge_dispatch_completion(
        1,
        DispatchCompletion {
            sealed_producer: None,
            sealed_invocation: None,
            sealed_next_event_sequence: 1,
            sealed_next_outer_sequence: 2,
            actions: VecDeque::from([ServerAction::Failed {
                stream: 1,
                failure: CallFailure::InternalFailure,
            }]),
            cancellation: ServerAction::Cancelled { stream: 1 },
            cancellation_token: None,
            start_gate: None,
            start_delivered: false,
            terminal_delivered: false,
            terminal_claimed: false,
            worker_completed: false,
            _guards: None,
        },
        &mut pending,
        &mut cancelled,
        &mut BTreeSet::new(),
    );

    let completion = pending.get(&1).expect("pre-start completion remains");
    assert_eq!(
        completion.actions,
        VecDeque::from([ServerAction::Failed {
            stream: 1,
            failure: CallFailure::InternalFailure,
        }]),
    );
    assert!(completion.terminal_claimed);
    assert!(cancelled.is_empty());
}

#[test]
fn pre_start_invoke_terminal_keeps_started_event_before_result() {
    let invocation = InvocationId::from_bytes([0x43; 16]);
    let started = InvokeEvent::new(
        invocation,
        0,
        InvocationEventBody::Started {
            visible_principal: None,
        },
    )
    .expect("started invocation event");
    let started_events = InvocationEventBatch::new(vec![InvocationEventRecord::new(1, started)])
        .expect("started invocation event batch");
    let terminal = InvokeEvent::new(
        invocation,
        1,
        InvocationEventBody::Completed {
            duration_nanoseconds: 1,
        },
    )
    .expect("terminal invocation event");
    let terminal_events = InvocationEventBatch::new(vec![InvocationEventRecord::new(1, terminal)])
        .expect("terminal invocation event batch");
    let (start_gate, _start_receiver) = oneshot::channel();
    let mut pending = BTreeMap::from([(
        1,
        DispatchCompletion {
            sealed_producer: None,
            sealed_invocation: Some(invocation),
            sealed_next_event_sequence: 1,
            sealed_next_outer_sequence: 2,
            actions: VecDeque::from([ServerAction::InvokeEvents {
                stream: 1,
                events: started_events,
            }]),
            cancellation: ServerAction::InvokeCancelled { stream: 1 },
            cancellation_token: None,
            start_gate: Some(start_gate),
            start_delivered: false,
            terminal_delivered: false,
            terminal_claimed: false,
            worker_completed: false,
            _guards: None,
        },
    )]);
    queue_cancellation_actions(pending.get_mut(&1).expect("pending invocation"), 1);
    let mut cancelled_pending = BTreeSet::from([1]);

    merge_dispatch_completion(
        1,
        DispatchCompletion {
            sealed_producer: None,
            sealed_invocation: Some(invocation),
            sealed_next_event_sequence: 1,
            sealed_next_outer_sequence: 2,
            actions: VecDeque::from([
                ServerAction::InvokeEvents {
                    stream: 1,
                    events: terminal_events,
                },
                ServerAction::Completed { stream: 1 },
            ]),
            cancellation: ServerAction::InvokeCancelled { stream: 1 },
            cancellation_token: None,
            start_gate: None,
            start_delivered: false,
            terminal_delivered: false,
            terminal_claimed: false,
            worker_completed: false,
            _guards: None,
        },
        &mut pending,
        &mut BTreeSet::new(),
        &mut cancelled_pending,
    );

    let completion = pending.get(&1).expect("pending invocation remains");
    assert_eq!(completion.sealed_invocation, Some(invocation));
    assert_eq!(completion.actions.len(), 3);
    assert!(matches!(
        completion.actions.front(),
        Some(ServerAction::InvokeEvents { events, .. })
            if events.records()[0].event().kind() == InvocationEventKind::InvocationStarted
    ));
    assert!(matches!(
        completion.actions.get(1),
        Some(ServerAction::InvokeEvents { events, .. })
            if events.records()[0].event().kind() == InvocationEventKind::InvocationCompleted
    ));
    assert!(matches!(
        completion.actions.get(2),
        Some(ServerAction::Completed { stream: 1 })
    ));
    assert!(completion.terminal_claimed);
    assert!(cancelled_pending.is_empty());
}

#[tokio::test]
async fn sealed_cancel_terminal_waits_for_worker_completion() {
    let version = RawProtocolVersion::One;
    let mut connection = ProtocolConnection::new();
    let mut pending = BTreeMap::from([(
        1,
        DispatchCompletion {
            sealed_producer: None,
            sealed_invocation: None,
            sealed_next_event_sequence: 1,
            sealed_next_outer_sequence: 2,
            actions: VecDeque::from([ServerAction::InvokeCancelled { stream: 1 }]),
            cancellation: ServerAction::InvokeCancelled { stream: 1 },
            cancellation_token: None,
            start_gate: None,
            start_delivered: true,
            terminal_delivered: false,
            terminal_claimed: false,
            worker_completed: false,
            _guards: None,
        },
    )]);
    let (server, _client) = UnixStream::pair().expect("socket pair");
    let (_reader, mut writer) = server.into_split();
    let (_shutdown_sender, mut shutdown) = watch::channel(false);
    let mut sealed_pull_tasks = JoinSet::new();
    let mut sealed_pull_in_flight = BTreeSet::new();
    let mut sealed_pull_waiting_bytes = BTreeMap::new();
    let mut producer_shutdown = JoinSet::new();

    assert!(
        flush_pending(
            &version,
            &mut connection,
            &mut pending,
            &mut sealed_pull_tasks,
            &mut sealed_pull_in_flight,
            &mut sealed_pull_waiting_bytes,
            &mut producer_shutdown,
            &mut writer,
            &mut shutdown,
        )
        .await
        .expect("pending cancellation flushes without an error")
    );

    let completion = pending.get(&1).expect("cancellation remains queued");
    assert!(matches!(
        completion.actions.front(),
        Some(ServerAction::InvokeCancelled { stream: 1 })
    ));
    assert!(!completion.worker_completed);
    assert!(!completion.terminal_delivered);
}

#[test]
fn queued_internal_failure_is_not_replaced_by_cancel() {
    let mut pending = BTreeMap::from([(
        1,
        DispatchCompletion {
            sealed_producer: None,
            sealed_invocation: None,
            sealed_next_event_sequence: 1,
            sealed_next_outer_sequence: 2,
            actions: VecDeque::new(),
            cancellation: ServerAction::Cancelled { stream: 1 },
            cancellation_token: None,
            start_gate: None,
            start_delivered: true,
            terminal_delivered: false,
            terminal_claimed: false,
            worker_completed: false,
            _guards: None,
        },
    )]);
    let mut cancelled = BTreeSet::new();
    let mut cancelled_pending = BTreeSet::from([1]);

    merge_dispatch_completion(
        1,
        DispatchCompletion {
            sealed_producer: None,
            sealed_invocation: None,
            sealed_next_event_sequence: 1,
            sealed_next_outer_sequence: 2,
            actions: VecDeque::from([ServerAction::Failed {
                stream: 1,
                failure: CallFailure::InternalFailure,
            }]),
            cancellation: ServerAction::Cancelled { stream: 1 },
            cancellation_token: None,
            start_gate: None,
            start_delivered: true,
            terminal_delivered: false,
            terminal_claimed: false,
            worker_completed: false,
            _guards: None,
        },
        &mut pending,
        &mut cancelled,
        &mut cancelled_pending,
    );

    let completion = pending.get(&1).expect("pending completion remains");
    assert_eq!(
        completion.actions,
        VecDeque::from([ServerAction::Failed {
            stream: 1,
            failure: CallFailure::InternalFailure,
        }]),
    );
    assert!(completion.terminal_claimed);
    assert!(cancelled_pending.is_empty());
}

#[test]
fn queued_terminal_invoke_events_are_not_replaced_by_cancel() {
    let invocation = InvocationId::from_bytes([0x42; 16]);
    let event = InvokeEvent::new(
        invocation,
        2,
        InvocationEventBody::Completed {
            duration_nanoseconds: 1,
        },
    )
    .expect("terminal invocation event");
    let events = InvocationEventBatch::new(vec![InvocationEventRecord::new(1, event)])
        .expect("terminal invocation event batch");
    let mut pending = BTreeMap::from([(
        1,
        DispatchCompletion {
            sealed_producer: None,
            sealed_invocation: None,
            sealed_next_event_sequence: 1,
            sealed_next_outer_sequence: 2,
            actions: VecDeque::new(),
            cancellation: ServerAction::InvokeCancelled { stream: 1 },
            cancellation_token: None,
            start_gate: None,
            start_delivered: true,
            terminal_delivered: false,
            terminal_claimed: false,
            worker_completed: false,
            _guards: None,
        },
    )]);
    let mut cancelled = BTreeSet::new();
    let mut cancelled_pending = BTreeSet::from([1]);

    merge_dispatch_completion(
        1,
        DispatchCompletion {
            sealed_producer: None,
            sealed_invocation: None,
            sealed_next_event_sequence: 1,
            sealed_next_outer_sequence: 2,
            actions: VecDeque::from([
                ServerAction::InvokeEvents { stream: 1, events },
                ServerAction::Completed { stream: 1 },
            ]),
            cancellation: ServerAction::InvokeCancelled { stream: 1 },
            cancellation_token: None,
            start_gate: None,
            start_delivered: true,
            terminal_delivered: false,
            terminal_claimed: false,
            worker_completed: false,
            _guards: None,
        },
        &mut pending,
        &mut cancelled,
        &mut cancelled_pending,
    );

    let completion = pending.get(&1).expect("pending completion remains");
    assert!(matches!(
        completion.actions.front(),
        Some(ServerAction::InvokeEvents { .. })
    ));
    assert!(completion.terminal_claimed);
    assert!(cancelled_pending.is_empty());
    assert!(cancelled.is_empty());
}
